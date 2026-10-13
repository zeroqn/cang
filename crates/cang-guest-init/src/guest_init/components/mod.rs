pub(in crate::guest_init) mod disk;
pub(in crate::guest_init) mod env;
pub(in crate::guest_init) mod fdwatch;
pub(in crate::guest_init) mod hardening;
pub(in crate::guest_init) mod home;
pub(in crate::guest_init) mod net;
pub(in crate::guest_init) mod nix;
pub(in crate::guest_init) mod opengl;
pub(in crate::guest_init) mod podman;
pub(in crate::guest_init) mod pulse;
pub(in crate::guest_init) mod rootless;
pub(in crate::guest_init) mod shell;
pub(in crate::guest_init) mod swap;
pub(in crate::guest_init) mod wayland;
pub(in crate::guest_init) mod waypipe;

/// Helpers shared by component tests that write a script and then `exec` it.
#[cfg(test)]
pub(in crate::guest_init) mod test_support {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Write an executable stub script at `path`.
    ///
    /// The write happens in a child, not in this process. The harness runs tests
    /// in parallel and a `fork()` in another test's thread copies this process's
    /// descriptor table: a stub written here can still be open for writing in that
    /// copy when a test `exec`s it, and the kernel then refuses the `exec` with
    /// `ETXTBSY` (`Text file busy`) for as long as any process holds such a
    /// descriptor. That is what made
    /// `components::waypipe::tests::failed_replacement_can_be_retried` flake.
    /// Writing the stub from a child leaves no writable descriptor in this
    /// process, so a sibling's fork has nothing to inherit.
    pub(in crate::guest_init) fn write_executable_stub(path: &Path, contents: &str) {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("cat > \"$1\" && chmod 0755 \"$1\"")
            .arg("write-stub")
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn the stub writer");

        child
            .stdin
            .as_mut()
            .expect("stub writer stdin")
            .write_all(contents.as_bytes())
            .expect("write stub contents");
        drop(child.stdin.take());

        let status = child.wait().expect("wait for the stub writer");
        assert!(status.success(), "stub writer exited with {status}");

        assert!(
            !holds_writable_descriptor(path),
            "{} is still open in this process; a concurrent fork could inherit it              and break the exec with ETXTBSY",
            path.display()
        );
    }

    /// Whether this process holds a descriptor for `path` at all - a descriptor
    /// open for writing is what makes an `exec` fail with `ETXTBSY`.
    fn holds_writable_descriptor(path: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return false;
        };
        entries.flatten().any(|entry| {
            std::fs::read_link(entry.path())
                .map(|target| target == path)
                .unwrap_or(false)
        })
    }
}
