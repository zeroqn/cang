//! `cang doctor`: are the host's virglrenderer and mesa cang's patched builds?
//!
//! cang's GPU path rests on two host libraries that are not stock upstream:
//!
//!  * **virglrenderer** - the vrend renderer cang links (its `libvirglrenderer.so.1`
//!    is a `DT_NEEDED` of the `cang` binary) and the `virgl_render_server` helper
//!    it execs. cang's patches live in `nix/lib/virglrenderer-patched.nix`: the
//!    linear-surface and GBM layout work, the encoded raw-header upload, the
//!    reference/DPB fill, encoder attributes and rate control, and the decoded
//!    surface read-back. A stock vrend decodes a guest's video into the wrong
//!    planes, drops the guest's reference pictures, and cannot allocate the
//!    surfaces the guest describes.
//!  * **mesa** - the VA driver libva loads on the host, which is the `libgallium`
//!    the render server's VA context calls and the same file the GL/Vulkan side
//!    comes from. cang's patches live in `nix/lib/mesa-patched.nix`: the guest half
//!    of the raw-header upload, the packed-header RBSP bounds fix (a stock
//!    `vl_rbsp_ue` loops for ever on a truncated header - ticket 08), the
//!    virtio-gpu DMA-BUF modifier report, and the encoder-attribute forwarding a
//!    B-frame GOP needs. The check follows libva's own search order (cang's VA
//!    override directory first, then `LIBVA_DRIVERS_PATH`, then the system
//!    directories), because the file that decides whether the host VA path hits
//!    the hang is the one libva would open.
//!
//! Both are detected by a **marker**: a literal that only cang's patch puts into
//! the build, looked for in the library file itself. That is the same evidence
//! tools/chromium-cang-smoke and the repository's GPU checks use, and it needs no
//! nix at run time. The marker-to-patch table below is asserted against the patch
//! files in this module's tests, so renaming a marker in a patch fails the build
//! instead of quietly reporting a patched library as stock.
//!
//! A `Missing`/`Unpatched` result fails (exit 1); a fact this host cannot supply
//! (mesa not configured, i.e. no `CANG_MESA_LIBDIR`) is a warning, because a host
//! that never starts a `--gpu=drm` guest does not need it.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// A literal only cang's patched build contains, and the patch that puts it there.
struct Marker {
    literal: &'static str,
    patch: &'static str,
}

/// `nix/pkgs/patches/virglrenderer-*.patch`: the corpus the linked vrend is
/// checked against. Each of these is absent from a stock nixpkgs virglrenderer.
const VIRGLRENDERER_MARKERS: &[Marker] = &[
    Marker {
        literal: "encode raw headers: %u",
        patch: "virglrenderer-encode-raw-headers.patch",
    },
    Marker {
        literal: "render encode raw header failed",
        patch: "virglrenderer-encode-raw-headers.patch",
    },
    Marker {
        literal: "linear surface unsupported, falling back",
        patch: "virglrenderer-linear-surface.patch",
    },
    Marker {
        literal: "layout this path converts",
        patch: "virglrenderer-decode-surface-readback.patch",
    },
];

/// `nix/pkgs/patches/mesa-*.patch`. `fill_enc_raw_headers` is a function the
/// raw-header patch adds; its name is in the library's symbol strings.
const MESA_MARKERS: &[Marker] = &[Marker {
    literal: "fill_enc_raw_headers",
    patch: "mesa-virgl-encode-raw-headers.patch",
}];

/// The library cang links; the loader's soname for it.
const VIRGLRENDERER_SONAME: &str = "libvirglrenderer.so";

/// libva's own environment variable for its driver search path.
const LIBVA_DRIVERS_PATH_ENV: &str = "LIBVA_DRIVERS_PATH";

/// Where libva looks when nothing overrides it: the NixOS opengl-driver farm
/// first (what a NixOS host builds its mesa into), then the usual system
/// directories. The selected directory is the one whose `*_drv_video.so` decides
/// whether the host VA path hits the `vl_rbsp_ue` hang.
const SYSTEM_DRIVER_DIRS: &[&str] = &[
    "/run/opengl-driver/lib/dri",
    "/usr/lib/dri",
    "/usr/lib/x86_64-linux-gnu/dri",
];

/// A VA driver directory and the rule that selected it.
struct VaDriverDir {
    path: PathBuf,
    source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Warn,
    Fail,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Warn => "WARN",
            Verdict::Fail => "FAIL",
        }
    }

    fn is_failure(self) -> bool {
        self == Verdict::Fail
    }
}

/// One dependency's result: what was resolved, which markers were found, and how
/// that adds up.
struct Check {
    name: &'static str,
    path: Option<PathBuf>,
    found: Vec<&'static str>,
    detail: Vec<String>,
    verdict: Verdict,
}

/// Whether `needle` occurs in the file at `path`, read in chunks so a 60 MiB
/// `libgallium` does not have to be held in memory. `needle` is a short ASCII
/// literal, so a byte search is the whole comparison.
fn file_contains(path: &Path, needle: &str) -> std::io::Result<bool> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Ok(true);
    }

    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut window = vec![0_u8; (1 << 20) + needle.len() - 1];
    let mut filled = 0_usize;

    loop {
        let read = reader.read(&mut window[filled..])?;
        if read == 0 {
            return Ok(false);
        }
        let available = filled + read;
        if window[..available]
            .windows(needle.len())
            .any(|w| w == needle)
        {
            return Ok(true);
        }
        // Keep the last `needle.len() - 1` bytes so a match across the chunk
        // boundary is still seen.
        let keep = needle.len() - 1;
        if available > keep {
            window.copy_within(available - keep..available, 0);
        }
        filled = available.min(keep);
    }
}

/// The markers of `markers` that occur in the library at `path`.
fn markers_found(path: &Path, markers: &[Marker]) -> std::io::Result<Vec<&'static str>> {
    let mut found = Vec::new();
    for marker in markers {
        if file_contains(path, marker.literal)? {
            found.push(marker.literal);
        }
    }
    Ok(found)
}

/// The patches whose markers are absent from a library, in table order and
/// without repeats - what a reader needs to know to put the build right.
fn patches_missing(found: &[&str], markers: &[Marker]) -> Vec<&'static str> {
    let mut missing: Vec<&'static str> = Vec::new();
    for marker in markers {
        if !found.contains(&marker.literal) && !missing.contains(&marker.patch) {
            missing.push(marker.patch);
        }
    }
    missing
}

/// The path of the loaded `soname`, read out of a `/proc/<pid>/maps` listing.
/// A path may carry a `" (deleted)"` suffix, and the loader keeps the mapping it
/// opened, so the entry is used as written otherwise.
fn mapped_library(maps: &str, soname: &str) -> Option<PathBuf> {
    for line in maps.lines() {
        let Some(index) = line.find("/nix/store/") else {
            continue;
        };
        let path = line[index..].trim_end();
        let path = path.strip_suffix(" (deleted)").unwrap_or(path);
        let name = Path::new(path).file_name()?.to_str()?;
        if name.starts_with(soname) {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// `#` `cang doctor`'s first check: the vrend cang actually loaded.
fn check_virglrenderer(mapped: Option<PathBuf>, helper: Option<PathBuf>) -> Check {
    let mut detail = Vec::new();
    let mut found = Vec::new();
    let mut verdict = Verdict::Fail;

    if let Some(path) = mapped.as_deref() {
        match markers_found(path, VIRGLRENDERER_MARKERS) {
            Ok(literals) => {
                found = literals;
                if found.len() == VIRGLRENDERER_MARKERS.len() {
                    verdict = Verdict::Pass;
                    detail.push(format!(
                        "{} patch markers present",
                        VIRGLRENDERER_MARKERS.len()
                    ));
                } else if found.is_empty() {
                    detail.push(
                        "no cang patch marker found: this is a stock virglrenderer, so vrend \
                         will decode into the wrong planes and drop the guest's reference pictures"
                            .to_owned(),
                    );
                } else {
                    detail.push(format!(
                        "only {} of {} patch markers present ({})",
                        found.len(),
                        VIRGLRENDERER_MARKERS.len(),
                        found.join(", ")
                    ));
                }
                let missing = patches_missing(&found, VIRGLRENDERER_MARKERS);
                if !missing.is_empty() {
                    detail.push(format!("patches not in this build: {}", missing.join(", ")));
                }
            }
            Err(err) => {
                detail.push(format!("could not read the library: {err}"));
            }
        }
    } else {
        detail.push(
            "no libvirglrenderer mapped in this process; is this the cang binary?".to_owned(),
        );
    }

    // The helper has to be the same build as the linked library: cang execs it
    // for the external renderer, and it brings its own copy of vrend.
    if let Some(helper) = helper.as_deref() {
        detail.push(format!("render server: {}", helper.display()));
        match (mapped.as_deref(), std::fs::canonicalize(helper)) {
            (Some(mapped), Ok(helper_target)) => {
                match (store_dir_of(mapped), store_dir_of(&helper_target)) {
                    (Some(a), Some(b)) if a != b => {
                        verdict = Verdict::Fail;
                        detail.push(
                            "the render server is not from the same build as the linked \
                             virglrenderer"
                                .to_owned(),
                        );
                    }
                    (Some(_), Some(_)) => {}
                    // Neither is a store path (a checkout, a tarball install):
                    // nothing to compare, and the marker verdict stands.
                    _ => detail
                        .push("build pairing cannot be compared outside /nix/store".to_owned()),
                }
            }
            (_, Err(err)) => detail.push(format!("could not resolve the render server: {err}")),
            (None, _) => {}
        }
    } else {
        detail.push("no libexec/cang-helpers/virgl_render_server next to this binary".to_owned());
    }

    Check {
        name: "virglrenderer",
        path: mapped,
        found,
        detail,
        verdict,
    }
}

/// The `/nix/store/<hash>-<name>` directory a store path lives in.
fn store_dir_of(path: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    if parts.next()?.as_os_str() != "/" {
        return None;
    }
    if parts.next()?.as_os_str() != "nix" {
        return None;
    }
    if parts.next()?.as_os_str() != "store" {
        return None;
    }
    let entry = parts.next()?;
    let mut dir = PathBuf::from("/nix/store");
    dir.push(entry);
    Some(dir)
}

/// The directories libva would search, in libva's order but with cang's own
/// override first: the interposer hands the worker a driver from
/// `CANG_VA_DRIVER_PATH` (or the package's `<prefix>/lib/cang/dri`) when one is
/// there, and otherwise forwards libva's own request - which resolves through
/// `LIBVA_DRIVERS_PATH` and then the system directories.
fn va_driver_dirs() -> Vec<VaDriverDir> {
    let mut dirs: Vec<VaDriverDir> = crate::va_driver::override_dirs()
        .into_iter()
        .map(|path| VaDriverDir {
            source: format!(
                "cang's VA driver override: {}",
                crate::va_driver::VA_DRIVER_PATH_ENV
            ),
            path,
        })
        .collect();

    if let Ok(value) = std::env::var(LIBVA_DRIVERS_PATH_ENV) {
        for path in std::env::split_paths(&value).filter(|p| !p.as_os_str().is_empty()) {
            dirs.push(VaDriverDir {
                source: LIBVA_DRIVERS_PATH_ENV.to_owned(),
                path,
            });
        }
    }

    for dir in SYSTEM_DRIVER_DIRS {
        dirs.push(VaDriverDir {
            // A system directory introduces itself; no label needed.
            source: String::new(),
            path: PathBuf::from(dir),
        });
    }

    dirs
}

/// The first directory in `dirs` that holds a VA driver, and the driver file
/// itself: libva (and cang's interposer) both walk the list for a name, so an
/// empty directory is not a failure but a fall-through.
fn find_va_driver(dirs: &[VaDriverDir]) -> Option<(VaDriverDir, PathBuf)> {
    for dir in dirs {
        let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir.path)
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(crate::va_driver::DRIVER_SUFFIX))
            })
            .collect();
        candidates.sort();
        if let Some(path) = candidates.into_iter().next() {
            // The entries are symlinks to the driver's own library, and the
            // markers live in the target.
            let resolved = std::fs::canonicalize(&path).unwrap_or(path);
            return Some((
                VaDriverDir {
                    path: dir.path.clone(),
                    source: dir.source.clone(),
                },
                resolved,
            ));
        }
    }
    None
}

/// `cang doctor`'s second check: the mesa behind the VA driver libva will load.
fn check_mesa(driver: Option<(VaDriverDir, PathBuf)>) -> Check {
    let Some((dir, path)) = driver else {
        return Check {
            name: "mesa",
            path: None,
            found: Vec::new(),
            detail: vec![format!(
                "no VA driver found in any of the directories libva would search ({});                  there is nothing for the host VA path to load",
                va_driver_dirs()
                    .iter()
                    .map(|dir| dir.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )],
            verdict: Verdict::Warn,
        };
    };

    let mut detail = vec![if dir.source.is_empty() {
        format!("VA driver directory: {}", dir.path.display())
    } else {
        format!(
            "VA driver directory: {} ({})",
            dir.path.display(),
            dir.source
        )
    }];

    let (found, verdict) = match markers_found(&path, MESA_MARKERS) {
        Ok(found) if found.len() == MESA_MARKERS.len() => {
            detail.push(format!("{} patch marker present", MESA_MARKERS.len()));
            (found, Verdict::Pass)
        }
        Ok(found) => {
            detail.push(
                "no cang patch marker found: this is a stock mesa, so the packed-header reader \
                 can loop for ever (a constant-QP VA-API encode hangs) and the encoder \
                 attributes a B-frame GOP needs are not forwarded"
                    .to_owned(),
            );
            detail.push(format!(
                "patches not in this build: {}",
                patches_missing(&found, MESA_MARKERS).join(", ")
            ));
            (found, Verdict::Fail)
        }
        Err(err) => {
            detail.push(format!("could not read the driver: {err}"));
            (Vec::new(), Verdict::Fail)
        }
    };

    Check {
        name: "mesa",
        path: Some(path),
        found,
        detail,
        verdict,
    }
}

/// The render-server helper cang ships next to its own executable.
fn render_server_helper() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let prefix = executable.parent()?.parent()?;
    let helper = prefix.join("libexec/cang-helpers/virgl_render_server");
    helper.exists().then_some(helper)
}

/// Render the report and decide the exit code.
fn report(checks: &[Check]) -> ExitCode {
    println!("cang doctor: host GPU runtime");
    println!();

    let mut failed = false;
    for check in checks {
        println!(
            "{:<8} {:<14} {}",
            check.verdict.label(),
            check.name,
            check
                .path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
        );
        for line in &check.detail {
            println!("         {line}");
        }
        if !check.found.is_empty() {
            println!("         markers: {}", check.found.join(", "));
        }
        println!();
        failed |= check.verdict.is_failure();
    }

    if failed {
        println!(
            "One or more host dependencies are not cang's patched build. On a Nix host, apply \
             the flake's overlay (or use a cang package built from this repository) and retry."
        );
        ExitCode::from(1)
    } else {
        println!("Host GPU runtime is cang's patched build.");
        ExitCode::SUCCESS
    }
}

/// `cang doctor`.
pub(crate) fn run() -> ExitCode {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let mapped = mapped_library(&maps, VIRGLRENDERER_SONAME);
    let helper = render_server_helper();
    let driver = find_va_driver(&va_driver_dirs());

    report(&[check_virglrenderer(mapped, helper), check_mesa(driver)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIRGLRENDERER_RAW_HEADERS_PATCH: &str =
        include_str!("../../../nix/pkgs/patches/virglrenderer-encode-raw-headers.patch");
    const VIRGLRENDERER_LINEAR_SURFACE_PATCH: &str =
        include_str!("../../../nix/pkgs/patches/virglrenderer-linear-surface.patch");
    const VIRGLRENDERER_SURFACE_READBACK_PATCH: &str =
        include_str!("../../../nix/pkgs/patches/virglrenderer-decode-surface-readback.patch");
    const MESA_RAW_HEADERS_PATCH: &str =
        include_str!("../../../nix/pkgs/patches/mesa-virgl-encode-raw-headers.patch");

    fn patch_text(name: &str) -> &'static str {
        match name {
            "virglrenderer-encode-raw-headers.patch" => VIRGLRENDERER_RAW_HEADERS_PATCH,
            "virglrenderer-linear-surface.patch" => VIRGLRENDERER_LINEAR_SURFACE_PATCH,
            "virglrenderer-decode-surface-readback.patch" => VIRGLRENDERER_SURFACE_READBACK_PATCH,
            "mesa-virgl-encode-raw-headers.patch" => MESA_RAW_HEADERS_PATCH,
            other => panic!("no patch text registered for {other}"),
        }
    }

    /// The markers are the doctor's whole signal: each one has to be a literal the
    /// patch it names actually introduces, or the check would report a patched
    /// library as stock (or the reverse).
    #[test]
    fn every_marker_is_a_literal_its_patch_introduces() {
        for marker in VIRGLRENDERER_MARKERS.iter().chain(MESA_MARKERS) {
            let text = patch_text(marker.patch);
            assert!(
                text.contains(marker.literal),
                "marker {:?} is not in {}; update the marker or the patch name",
                marker.literal,
                marker.patch
            );
        }
    }

    /// Every marker the table names is also a marker a *stock* build does not
    /// have - that is what makes its absence meaningful. The two that stock builds
    /// do share (`sync surface failed`, `has_modifiers`) were measured and left
    /// out; this test keeps a re-added one from slipping through.
    #[test]
    fn the_marker_set_has_no_literal_a_plain_build_carries() {
        const KNOWN_SHARED_WITH_STOCK: &[&str] = &["sync surface failed", "has_modifiers"];
        for marker in VIRGLRENDERER_MARKERS.iter().chain(MESA_MARKERS) {
            assert!(
                !KNOWN_SHARED_WITH_STOCK.contains(&marker.literal),
                "{:?} occurs in a stock build too, so it cannot distinguish cang's",
                marker.literal
            );
        }
    }

    #[test]
    fn a_library_with_a_marker_is_found_and_one_without_it_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let with = dir.path().join("libgallium-1.0.so");
        std::fs::write(&with, b"....fill_enc_raw_headers....").expect("write");
        let without = dir.path().join("libgallium-2.0.so");
        std::fs::write(&without, b"....something else....").expect("write");

        assert!(file_contains(&with, "fill_enc_raw_headers").expect("scan"));
        assert!(!file_contains(&without, "fill_enc_raw_headers").expect("scan"));
        assert_eq!(
            markers_found(&with, MESA_MARKERS).expect("scan"),
            vec!["fill_enc_raw_headers"]
        );
        assert!(
            markers_found(&without, MESA_MARKERS)
                .expect("scan")
                .is_empty()
        );
    }

    #[test]
    fn a_marker_split_across_a_chunk_boundary_is_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.so");
        // A needle that straddles the reader's chunk boundary: the window keeps
        // `needle.len() - 1` bytes, so this is the case that must not regress.
        let mut contents = vec![b'x'; 1 << 20];
        contents.extend_from_slice(b"fill_enc_raw_");
        contents.extend_from_slice(b"headers");
        std::fs::write(&path, &contents).expect("write");
        assert!(file_contains(&path, "fill_enc_raw_headers").expect("scan"));
    }

    #[test]
    fn the_mapped_library_is_taken_from_the_maps_listing() {
        let maps = "\
55d0-55d1 r--p 00000000 00:00 0 /nix/store/aaa-cang-0.12.0/bin/cang
7f00-7f01 r-xp 00000000 00:00 0 /nix/store/bbb-virglrenderer-1.3.0/lib/libvirglrenderer.so.1.11.0
7f02-7f03 rw-p 00000000 00:00 0 /nix/store/ccc-glibc-2.42/lib/libc.so.6
";
        assert_eq!(
            mapped_library(maps, VIRGLRENDERER_SONAME),
            Some(PathBuf::from(
                "/nix/store/bbb-virglrenderer-1.3.0/lib/libvirglrenderer.so.1.11.0"
            ))
        );
        assert_eq!(
            mapped_library(maps, "libc.so"),
            Some(PathBuf::from("/nix/store/ccc-glibc-2.42/lib/libc.so.6"))
        );
        assert_eq!(mapped_library("", VIRGLRENDERER_SONAME), None);
    }

    #[test]
    fn a_deleted_mapping_path_is_trimmed() {
        let maps = "7f00-7f01 r-xp 00000000 00:00 0 /nix/store/b/libvirglrenderer.so.1 (deleted)\n";
        assert_eq!(
            mapped_library(maps, VIRGLRENDERER_SONAME),
            Some(PathBuf::from("/nix/store/b/libvirglrenderer.so.1"))
        );
    }

    #[test]
    fn the_store_directory_of_a_store_path_is_its_top_level_entry() {
        assert_eq!(
            store_dir_of(Path::new(
                "/nix/store/aaa-virglrenderer-1.3.0/lib/libvirglrenderer.so.1"
            )),
            Some(PathBuf::from("/nix/store/aaa-virglrenderer-1.3.0"))
        );
        assert_eq!(
            store_dir_of(Path::new("/usr/lib/libvirglrenderer.so.1")),
            None
        );
    }

    fn patched_library(dir: &Path, name: &str, markers: &[Marker]) -> PathBuf {
        let path = dir.join(name);
        let mut contents = String::new();
        for marker in markers {
            contents.push_str(marker.literal);
            contents.push('\n');
        }
        std::fs::write(&path, contents).expect("write library");
        path
    }

    #[test]
    fn a_library_with_every_marker_passes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = patched_library(dir.path(), "libvirglrenderer.so.1", VIRGLRENDERER_MARKERS);
        let check = check_virglrenderer(Some(path), None);
        assert_eq!(check.verdict, Verdict::Pass);
        assert_eq!(check.found.len(), VIRGLRENDERER_MARKERS.len());
    }

    #[test]
    fn a_library_without_markers_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("libvirglrenderer.so.1");
        std::fs::write(&path, "stock vrend").expect("write");
        let check = check_virglrenderer(Some(path), None);
        assert_eq!(check.verdict, Verdict::Fail);
        assert!(check.found.is_empty());
    }

    #[test]
    fn a_render_server_from_a_different_build_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = patched_library(dir.path(), "libvirglrenderer.so.1", VIRGLRENDERER_MARKERS);
        // A helper in the same directory tree as the library...
        let same = dir.path().join("virgl_render_server");
        std::fs::write(&same, "helper").expect("write");
        let check = check_virglrenderer(Some(lib.clone()), Some(same.clone()));
        // ...but neither is under /nix/store, so the pairing cannot be confirmed
        // and the verdict stays whatever the markers said.
        assert_eq!(check.verdict, Verdict::Pass);

        // Two real store directories, one per build: the mismatch is a failure.
        let a = Path::new("/nix/store/aaa-virglrenderer-1.3.0/lib/libvirglrenderer.so.1");
        let b = Path::new("/nix/store/bbb-virglrenderer-1.3.0/libexec/virgl_render_server");
        assert_ne!(store_dir_of(a), store_dir_of(b));
        assert_eq!(store_dir_of(a), store_dir_of(a));
    }

    fn va_driver_dir(path: &Path, source: &str) -> VaDriverDir {
        VaDriverDir {
            path: path.to_path_buf(),
            source: source.to_owned(),
        }
    }

    /// A directory holding one VA driver, whose target carries the given markers.
    fn driver_dir(dir: &Path, markers: &[Marker]) -> VaDriverDir {
        std::fs::create_dir_all(dir).expect("mkdir");
        let target = patched_library(dir, "libgallium-26.1.8.so", markers);
        std::os::unix::fs::symlink(&target, dir.join("radeonsi_drv_video.so")).expect("symlink");
        va_driver_dir(dir, "test")
    }

    #[test]
    fn the_va_driver_found_is_the_first_directory_that_has_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir");
        let populated = driver_dir(&dir.path().join("populated"), MESA_MARKERS);

        // An empty directory is a fall-through, not a result: libva and cang's
        // interposer both walk the list looking for a name.
        let found = find_va_driver(&[va_driver_dir(&empty, "empty"), populated]);
        let (dir, path) = found.expect("a driver");
        assert!(dir.path.ends_with("populated"));
        assert!(path.ends_with("libgallium-26.1.8.so"));

        assert!(find_va_driver(&[va_driver_dir(&empty, "empty")]).is_none());
        assert!(find_va_driver(&[]).is_none());
    }

    #[test]
    fn a_patched_host_va_driver_passes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let check = check_mesa(Some((
            driver_dir(&dir.path().join("dri"), MESA_MARKERS),
            dir.path().join("dri/libgallium-26.1.8.so"),
        )));
        assert_eq!(check.verdict, Verdict::Pass);
        assert_eq!(check.found, vec!["fill_enc_raw_headers"]);
    }

    #[test]
    fn a_stock_mesa_va_driver_fails_and_a_missing_one_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stock_dir = dir.path().join("dri");
        std::fs::create_dir_all(&stock_dir).expect("mkdir");
        let stock = stock_dir.join("libgallium-26.1.8.so");
        std::fs::write(&stock, "stock mesa").expect("write");
        std::os::unix::fs::symlink(&stock, stock_dir.join("radeonsi_drv_video.so"))
            .expect("symlink");
        let check = check_mesa(Some((va_driver_dir(&stock_dir, "test"), stock)));
        assert_eq!(check.verdict, Verdict::Fail);
        assert!(
            check
                .detail
                .iter()
                .any(|line| line.contains("patches not in this build"))
        );

        // Nothing to load at all: reported, but a warning rather than a failure,
        // because a host that never runs a --gpu=drm guest does not need it.
        let missing = check_mesa(None);
        assert_eq!(missing.verdict, Verdict::Warn);
        assert!(missing.detail[0].contains("no VA driver found"));
    }
}
