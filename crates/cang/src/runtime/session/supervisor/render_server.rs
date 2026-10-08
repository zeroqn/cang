//! Standalone sandboxed `virgl_render_server` runner for the venus `--gpu=drm`
//! path.
//!
//! The venus render server runs as a separate host process spawned by the cang
//! launcher *before* the network-enter helper, with its own landlock +
//! no_new_privs + seccomp sandbox. The launcher starts a short-lived bootstrap
//! process (`cang internal render-server-bootstrap`) with
//! `std::process::Command`, so the sandbox is applied in a fresh single-threaded
//! child rather than after a raw `fork()` in a possibly-multithreaded launcher.
//! The bootstrap closes the launcher-side fds it must not carry (the socketpair
//! parent end and the managed-attach readiness writer), applies landlock and
//! seccomp, then execs `virgl_render_server`. It connects back to the
//! in-process virglrenderer inside libkrun over a `SOCK_SEQPACKET` socketpair
//! whose parent end is delivered to the VM worker through the
//! `CANG_RENDER_SERVER_FD` environment variable (the fd survives the helper
//! exec and the VM-worker fork because the socketpair fds are not CLOEXEC).

use std::ffi::{CString, OsString};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use tracing::warn;

use crate::runtime::host_tools::{
    RuntimeTool, package_render_server_seccomp_policy_path_for_exe, runtime_tool_program,
};
use crate::runtime::landlock::apply_render_server_rules;
use crate::runtime::seccomp::{apply_compiled_enforce_policy, compile_enforce_policy};
use crate::runtime::session::supervisor::RENDER_SERVER_BOOTSTRAP_ARG;
use crate::runtime::session::supervisor::readiness_pipe::READY_FD_ENV;
use crate::runtime::vm::libkrun::RENDER_SERVER_FD_ENV;

pub(crate) const RENDER_SERVER_EXEC_PATH_ENV: &str = "RENDER_SERVER_EXEC_PATH";
pub(crate) const RENDER_SERVER_LD_LIBRARY_PATH_ENV: &str = "LD_LIBRARY_PATH";
pub(crate) const RENDER_SERVER_VK_DRIVER_FILES_ENV: &str = "VK_DRIVER_FILES";
// Redirect Mesa's on-disk shader cache into a landlock-allowed writable
// tempfs (the render server has no writable rules for $HOME/.cache).
const RENDER_SERVER_MESA_SHADER_CACHE_DIR_ENV: &str = "MESA_SHADER_CACHE_DIR";
const RENDER_SERVER_MESA_SHADER_CACHE_DIR_VALUE: &str = "/dev/shm/mesa-cache";

const RENDER_SERVER_CHILD_FD_ENV: &str = "CANG_RENDER_SERVER_CHILD_FD";
const RENDER_SERVER_PARENT_FD_ENV: &str = "CANG_RENDER_SERVER_PARENT_FD";

const MESA_LIBDIR_ENV: &str = "CANG_MESA_LIBDIR";
const MESA_ICD_ENV: &str = "CANG_MESA_ICD";
const VULKAN_LOADER_LIBDIR_ENV: &str = "CANG_VULKAN_LOADER_LIBDIR";
const RENDER_SERVER_POLICY_OVERRIDE_ENV: &str = "CANG_RENDER_SERVER_POLICY";

/// Host store paths the venus render server loads, resolved from the cang
/// package closure by the launcher wrapper.
#[derive(Debug, Clone)]
pub(crate) struct RenderServerEnv {
    pub(crate) exec_path: PathBuf,
    pub(crate) mesa_lib_dir: PathBuf,
    pub(crate) mesa_icd: PathBuf,
    pub(crate) vulkan_loader_lib_dir: PathBuf,
}

impl RenderServerEnv {
    pub(crate) fn resolve() -> Result<Self> {
        let exec_path = PathBuf::from(runtime_tool_program(RuntimeTool::VirglRenderServer));
        if !exec_path.is_file() {
            bail!(
                "render server executable '{}' does not exist; install cang's packaged virgl_render_server",
                exec_path.display()
            );
        }
        Ok(Self {
            exec_path,
            mesa_lib_dir: required_env_path(MESA_LIBDIR_ENV, "mesa library directory")?,
            mesa_icd: required_env_path(MESA_ICD_ENV, "mesa Vulkan ICD")?,
            vulkan_loader_lib_dir: required_env_path(
                VULKAN_LOADER_LIBDIR_ENV,
                "vulkan-loader library directory",
            )?,
        })
    }

    fn ld_library_path(&self) -> OsString {
        // The vulkan loader must find libvulkan_radeon.so from the mesa lib
        // dir; the loader itself is dlopen'd by virgl_render_server by soname.
        let mut value = self.vulkan_loader_lib_dir.as_os_str().to_os_string();
        value.push(":");
        value.push(self.mesa_lib_dir.as_os_str());
        value
    }

    /// The runner's own environment, exported to the exec'd process.
    pub(crate) fn env_vars(&self) -> Vec<(OsString, OsString)> {
        vec![
            (
                OsString::from(RENDER_SERVER_EXEC_PATH_ENV),
                self.exec_path.clone().into_os_string(),
            ),
            (
                OsString::from(RENDER_SERVER_LD_LIBRARY_PATH_ENV),
                self.ld_library_path(),
            ),
            (
                OsString::from(RENDER_SERVER_VK_DRIVER_FILES_ENV),
                self.mesa_icd.clone().into_os_string(),
            ),
            (
                OsString::from(RENDER_SERVER_MESA_SHADER_CACHE_DIR_ENV),
                OsString::from(RENDER_SERVER_MESA_SHADER_CACHE_DIR_VALUE),
            ),
        ]
    }
}

fn required_env_path(name: &str, description: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow!(
                "{description} is not set; expected environment variable {name} (set by the cang package wrapper)"
            )
        })
}

/// In-flight render-server runner owned by the cang launcher process.
///
/// Dropping the guard terminates the runner and closes the launcher's copy of
/// the parent socketpair end; the VM worker keeps its own inherited copy.
pub(crate) struct RenderServerGuard {
    pid: libc::pid_t,
    parent_fd: RawFd,
    pub(crate) env: RenderServerEnv,
}

impl RenderServerGuard {
    /// Detach the runner from the launcher's lifetime.
    ///
    /// Used when a managed session detaches while the VM worker is still
    /// running: the worker inherited its own copy of `parent_fd`, so the
    /// render server must stay alive to keep serving venus context creation.
    /// Dropping the guard afterwards only closes the launcher's `parent_fd`.
    pub(crate) fn disarm(&mut self) {
        self.pid = -1;
    }

    fn terminate_runner(&mut self) {
        if self.pid > 0 {
            // SAFETY: pid is the launcher's direct fork child; ECHILD is fine
            // if the runner already exited.
            unsafe {
                libc::kill(self.pid, libc::SIGTERM);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
            self.pid = -1;
        }
    }
}

impl Drop for RenderServerGuard {
    fn drop(&mut self) {
        self.terminate_runner();
        // SAFETY: parent_fd is the launcher's own socketpair end.
        unsafe {
            libc::close(self.parent_fd);
        }
    }
}

pub(crate) fn spawn_render_server(ready_fd: Option<RawFd>) -> Result<RenderServerGuard> {
    let env = RenderServerEnv::resolve()?;

    let mut fds = [0; 2];
    // SAFETY: socketpair writes the two connected fds into fds; SOCK_SEQPACKET
    // preserves message boundaries between the render server and the in-process
    // virglrenderer, and the fds are deliberately not CLOEXEC so they survive
    // the helper exec and the VM-worker fork.
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to create render-server socketpair");
    }
    let (parent_fd, child_fd) = (fds[0], fds[1]);

    // The sandbox is applied in the bootstrap child instead of after a raw
    // `fork()`, so `Command::spawn` (posix_spawn) keeps this safe even though
    // the waypipe broker threads are already running.
    let executable = std::env::current_exe()
        .context("failed to resolve cang executable for render server bootstrap")?;
    let mut command = Command::new(&executable);
    command.arg("internal").arg(RENDER_SERVER_BOOTSTRAP_ARG);
    command.envs(env.env_vars());
    command.env(RENDER_SERVER_CHILD_FD_ENV, child_fd.to_string());
    command.env(RENDER_SERVER_PARENT_FD_ENV, parent_fd.to_string());
    if let Some(fd) = ready_fd {
        command.env(READY_FD_ENV, fd.to_string());
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            // SAFETY: closing the launcher's own socketpair fds on the error path.
            unsafe {
                libc::close(parent_fd);
                libc::close(child_fd);
            }
            return Err(err).context("failed to spawn render server bootstrap");
        }
    };

    // SAFETY: the bootstrap child owns the other end of the socketpair.
    unsafe {
        libc::close(child_fd);
    }
    // SAFETY: this one-shot set_var runs before the helper spawn and only
    // mutates the process environment; the waypipe broker threads that may
    // already be live do not read the environment. The env var carries the
    // parent end of the socketpair to the VM worker through the helper exec.
    unsafe { std::env::set_var(RENDER_SERVER_FD_ENV, parent_fd.to_string()) };
    let pid = child.id() as libc::pid_t;
    Ok(RenderServerGuard {
        pid,
        parent_fd,
        env,
    })
}

fn render_server_policy_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(RENDER_SERVER_POLICY_OVERRIDE_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return Ok(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| package_render_server_seccomp_policy_path_for_exe(&exe))
        .ok_or_else(|| anyhow!("cannot resolve the render-server seccomp policy path"))
}

pub(crate) fn run_render_server_bootstrap(args: Vec<OsString>) -> Result<()> {
    if !args.is_empty() {
        bail!(
            "render server bootstrap takes no arguments, got {}",
            args.len()
        );
    }
    let child_fd = parse_required_fd_env(RENDER_SERVER_CHILD_FD_ENV)?;
    let parent_fd = parse_required_fd_env(RENDER_SERVER_PARENT_FD_ENV)?;
    let ready_fd = parse_optional_fd_env(READY_FD_ENV)?;
    close_render_server_inherited_fds(Some(parent_fd), ready_fd);

    // Landlock is defense in depth here; if the running kernel lacks support we
    // still enforce the seccomp allowlist below.
    if let Err(err) = apply_render_server_rules() {
        warn!(
            error = %err,
            "render server landlock rules not applied; seccomp filter still enforced"
        );
    }

    let policy_path = render_server_policy_path()?;
    let seccomp = compile_enforce_policy(&policy_path)?;
    apply_compiled_enforce_policy(&seccomp)?;
    exec_render_server(child_fd)?;
    Ok(())
}

fn exec_render_server(child_fd: RawFd) -> Result<()> {
    let exec_path = std::env::var_os(RENDER_SERVER_EXEC_PATH_ENV).ok_or_else(|| {
        anyhow!("{RENDER_SERVER_EXEC_PATH_ENV} is not set in render server bootstrap")
    })?;
    let exec_path = CString::new(exec_path.as_os_str().as_bytes())
        .expect("render server exec path contains no NUL byte");
    let arg0 = CString::new("virgl_render_server").expect("static argv string has no NUL byte");
    let socket_option = CString::new(format!("--socket-fd={child_fd}"))
        .expect("socket-fd option contains no NUL byte");
    let argv = [arg0.as_ptr(), socket_option.as_ptr(), std::ptr::null()];
    // SAFETY: execv with a NUL-terminated path and argv array; only returns on
    // failure.
    unsafe {
        libc::execv(exec_path.as_ptr(), argv.as_ptr());
    }
    Err(std::io::Error::last_os_error()).context("failed to exec virgl_render_server")
}

/// Close the launcher-side fds the bootstrap must not carry into the exec'd
/// render server: the socketpair parent end (its real peer is the VM worker's
/// inherited copy) and the managed-attach readiness writer (leaving it open
/// would suppress the readiness EOF and delay managed-startup failure until the
/// readiness timeout).
fn close_render_server_inherited_fds(parent_fd: Option<RawFd>, ready_fd: Option<RawFd>) {
    for fd in [parent_fd, ready_fd].into_iter().flatten() {
        // SAFETY: the bootstrap holds its own inherited copy of these fds;
        // closing them is best-effort and cannot affect the launcher's copies.
        unsafe {
            libc::close(fd);
        }
    }
}

fn parse_required_fd_env(name: &str) -> Result<RawFd> {
    let value = std::env::var_os(name)
        .ok_or_else(|| anyhow!("{name} is not set; expected the render server bootstrap fd env"))?;
    parse_fd_env_value(name, value)
}

fn parse_optional_fd_env(name: &str) -> Result<Option<RawFd>> {
    match std::env::var_os(name) {
        Some(value) => parse_fd_env_value(name, value).map(Some),
        None => Ok(None),
    }
}

fn parse_fd_env_value(name: &str, value: OsString) -> Result<RawFd> {
    let text = value
        .into_string()
        .map_err(|_| anyhow!("{name} is not valid UTF-8"))?;
    text.parse::<RawFd>()
        .with_context(|| format!("{name} value '{text}' is not a file descriptor"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::seccomp::allowed_syscalls_from_policy;

    fn env() -> RenderServerEnv {
        RenderServerEnv {
            exec_path: PathBuf::from(
                "/nix/store/vgl-virglrenderer-1.3.0/libexec/virgl_render_server",
            ),
            mesa_lib_dir: PathBuf::from("/nix/store/mesa-mesa-26.1.8/lib"),
            mesa_icd: PathBuf::from(
                "/nix/store/mesa-mesa-26.1.8/share/vulkan/icd.d/radeon_icd.x86_64.json",
            ),
            vulkan_loader_lib_dir: PathBuf::from("/nix/store/vk-vulkan-loader-1.4.341.0/lib"),
        }
    }

    #[test]
    fn render_server_env_vars_export_the_runner_environment() {
        let vars = env().env_vars();
        assert!(vars.contains(&(
            OsString::from(RENDER_SERVER_EXEC_PATH_ENV),
            OsString::from("/nix/store/vgl-virglrenderer-1.3.0/libexec/virgl_render_server")
        )));
        assert!(vars.contains(&(
            OsString::from(RENDER_SERVER_LD_LIBRARY_PATH_ENV),
            OsString::from(
                "/nix/store/vk-vulkan-loader-1.4.341.0/lib:/nix/store/mesa-mesa-26.1.8/lib"
            )
        )));
        assert!(vars.contains(&(
            OsString::from(RENDER_SERVER_VK_DRIVER_FILES_ENV),
            OsString::from("/nix/store/mesa-mesa-26.1.8/share/vulkan/icd.d/radeon_icd.x86_64.json")
        )));
        assert!(vars.contains(&(
            OsString::from(RENDER_SERVER_MESA_SHADER_CACHE_DIR_ENV),
            OsString::from(RENDER_SERVER_MESA_SHADER_CACHE_DIR_VALUE)
        )));
    }

    #[test]
    fn disarmed_render_server_guard_leaves_the_runner_alive_on_drop() {
        // A detached managed session returns from run_helper_process while the
        // VM worker (which inherits its own copy of the socketpair) is still
        // running; the render server must survive the launcher exit so the
        // proxy can keep creating venus contexts.  Disarming the guard makes
        // Drop skip the SIGTERM and just close the launcher's parent_fd.
        // SAFETY: the child only loops in pause(); the parent reaps it after
        // the assertion, so no state is shared with the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        let mut guard = RenderServerGuard {
            pid,
            parent_fd: -1,
            env: env(),
        };
        guard.disarm();
        drop(guard);
        let runner_alive = unsafe { libc::kill(pid, 0) } == 0;
        // Cleanup even on assertion failure.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert!(
            runner_alive,
            "disarmed render server guard must not signal its runner on drop"
        );
    }

    #[test]
    fn render_server_seccomp_policy_allows_venus_driver_syscalls() {
        // The venus render server's RADV driver calls these during the first
        // context create (shader-cache setup + worker thread tuning). With the
        // default mismatch_action "trap", a missing allowlist entry SIGSYS-kills
        // the server ~5s after proxy init, before the first CtxCreate completes.
        // `fallocate` is what the on-disk shader cache uses to grow its cache
        // file: the runner sets MESA_SHADER_CACHE_DIR, so the driver always
        // enables that cache and always reaches this call.
        // `rename` is how that cache publishes an entry (`renameat2` was
        // allowed, plain `rename` was not): without it the per-context worker -
        // the process that owns the guest's venus ring and its alive-bit
        // heartbeat - is SIGSYS-killed the first time a compiled shader is
        // committed, and the guest then waits for a ring seqno that can never
        // advance ("expired ring alive status"). See
        // docs/wayfinder/guest-vaapi-video/tickets/07-*.
        // Regression guard for the chromium --gpu=drm venus smoke baseline.
        let policy_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/seccomp/render-server.json");
        let allowed =
            allowed_syscalls_from_policy(&policy_path).expect("parse render server policy");
        for syscall in [
            "fallocate",
            "flock",
            "mkdir",
            "rename",
            "sched_setscheduler",
            "setpriority",
        ] {
            assert!(
                allowed.contains(syscall),
                "render server seccomp policy must allow {syscall} (venus RADV driver)"
            );
        }
    }

    #[test]
    fn render_server_bootstrap_rejects_non_empty_args() {
        let err = run_render_server_bootstrap(vec!["unexpected".into()])
            .expect_err("render server bootstrap takes no arguments");
        assert!(format!("{err:#}").contains("no arguments"));
    }

    #[test]
    fn render_server_bootstrap_fd_parse_rejects_missing_env() {
        let name = "CANG_RENDER_SERVER_TEST_SHOULD_NOT_EXIST";
        let err = parse_required_fd_env(name)
            .expect_err("missing required render server fd env must fail");
        assert!(format!("{err:#}").contains(name));
    }

    #[test]
    fn close_render_server_inherited_fds_closes_only_targeted_fds() {
        let mut targeted = [0; 2];
        let mut untargeted = [0; 2];
        // SAFETY: pipe writes two valid fds into the array on success.
        assert_eq!(unsafe { libc::pipe(targeted.as_mut_ptr()) }, 0);
        // SAFETY: pipe writes two valid fds into the array on success.
        assert_eq!(unsafe { libc::pipe(untargeted.as_mut_ptr()) }, 0);

        // Move the targeted fds to high numbers (>= 1000) so concurrent test
        // threads, which allocate from the lowest available fd, cannot reuse
        // the numbers before the closure check completes below.
        // SAFETY: F_DUPFD_CLOEXEC returns the lowest free fd >= 1000;
        // ulimit -n is 1048576 on the CI and development machines so the
        // call never fails with EINVAL.
        let hi_targeted = [
            unsafe { libc::fcntl(targeted[0], libc::F_DUPFD_CLOEXEC, 1000) },
            unsafe { libc::fcntl(targeted[1], libc::F_DUPFD_CLOEXEC, 1000) },
        ];
        assert!(hi_targeted[0] >= 1000, "F_DUPFD_CLOEXEC returned a low fd");
        assert!(hi_targeted[1] >= 1000, "F_DUPFD_CLOEXEC returned a low fd");
        // SAFETY: closing the original low-numbered copies; the high-numbered
        // dups are live and will be closed by the function under test.
        unsafe {
            libc::close(targeted[0]);
            libc::close(targeted[1]);
        }

        close_render_server_inherited_fds(Some(hi_targeted[0]), Some(hi_targeted[1]));

        // SAFETY: F_GETFD returns -1 with errno EBADF only for closed fds.
        assert_eq!(unsafe { libc::fcntl(hi_targeted[0], libc::F_GETFD) }, -1);
        assert_eq!(unsafe { libc::fcntl(hi_targeted[1], libc::F_GETFD) }, -1);
        // SAFETY: F_GETFD returns a non-negative value for open fds.
        assert!(unsafe { libc::fcntl(untargeted[0], libc::F_GETFD) } != -1);
        assert!(unsafe { libc::fcntl(untargeted[1], libc::F_GETFD) } != -1);
        // SAFETY: closing the test's own pipe fds.
        unsafe {
            libc::close(untargeted[0]);
            libc::close(untargeted[1]);
        }
    }
}
