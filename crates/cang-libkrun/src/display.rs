//! The headless display backend a GPU device is built on.
//!
//! `DisplayBackend::new` still takes the pre-ffier `krun_display_backend` vtable
//! struct by pointer: the Rust API copies it (`read_unaligned` + `verify`) and
//! rejects a backend without the mandatory `BASIC_FRAMEBUFFER` methods. cang
//! drives the GPU headless - venus renders through the sandboxed render server
//! and nothing ever scans out - so the vtable advertises the feature with
//! methods that refuse every request, which keeps a scanout that nobody set up
//! an explicit error instead of a null call.

use std::ffi::c_void;

use anyhow::Result;
use krun::DisplayBackend;

/// `KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER` from `libkrun_display.h`.
const KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER: u64 = 1;
/// `KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED` from `libkrun_display.h`.
const KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED: i32 = -2;

unsafe extern "C" fn display_disable_scanout(_instance: *mut c_void, _scanout_id: u32) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_configure_scanout(
    _instance: *mut c_void,
    _scanout_id: u32,
    _display_width: u32,
    _display_height: u32,
    _width: u32,
    _height: u32,
    _format: u32,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_alloc_frame(
    _instance: *mut c_void,
    _scanout_id: u32,
    _buffer: *mut *mut u8,
    _buffer_size: *mut usize,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_present_frame(
    _instance: *mut c_void,
    _scanout_id: u32,
    _frame_id: u32,
    _damage_area: *const c_void,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

/// The `krun_display_basic_framebuffer_vtable` callbacks.
///
/// The last four are the `KRUN_DISPLAY_FEATURE_DMABUF_CONSUMER` extension, which
/// upstream added to the same struct after `v2.0.0-cang.23`. They stay `None`:
/// cang advertises only `BASIC_FRAMEBUFFER`, and a backend must be zero
/// initialized beyond the fields it uses. They still have to be present, because
/// `krun_display_backend_new` copies the whole struct and rejects a
/// `vtable_size` below the pinned header's size, which is how a stale mirror
/// fails as `krun_gpu_device_new: invalid parameter` rather than at this call.
#[repr(C)]
struct KrunDisplayBasicFramebufferVtable {
    destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    disable_scanout: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    configure_scanout:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, u32) -> i32>,
    alloc_frame: Option<unsafe extern "C" fn(*mut c_void, u32, *mut *mut u8, *mut usize) -> i32>,
    present_frame: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_void) -> i32>,
    import_dmabuf: Option<unsafe extern "C" fn(*mut c_void, *const c_void) -> i32>,
    unref_dmabuf: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    configure_scanout_dmabuf:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, *const c_void) -> i32>,
    present_dmabuf: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> i32>,
}

/// `struct krun_display_backend`: the display backend a GPU device is built on.
#[repr(C)]
struct KrunDisplayBackendConfig {
    features: u64,
    create_userdata: *const c_void,
    create: Option<unsafe extern "C" fn(*mut *mut c_void, *const c_void, *const c_void) -> i32>,
    vtable: KrunDisplayBasicFramebufferVtable,
}

fn headless_display_backend_config() -> KrunDisplayBackendConfig {
    KrunDisplayBackendConfig {
        features: KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER,
        create_userdata: std::ptr::null(),
        create: None,
        vtable: KrunDisplayBasicFramebufferVtable {
            destroy: None,
            disable_scanout: Some(display_disable_scanout),
            configure_scanout: Some(display_configure_scanout),
            alloc_frame: Some(display_alloc_frame),
            present_frame: Some(display_present_frame),
            import_dmabuf: None,
            unref_dmabuf: None,
            configure_scanout_dmabuf: None,
            present_dmabuf: None,
        },
    }
}

/// Build the headless display backend cang attaches to its GPU device.
pub(crate) fn headless_display_backend() -> Result<DisplayBackend> {
    let config = headless_display_backend_config();
    // SAFETY: the callee copies the struct out of the pointer and verifies it;
    // the stack value does not have to outlive the call.
    let backend = unsafe {
        DisplayBackend::new(
            &config as *const KrunDisplayBackendConfig as *const c_void,
            std::mem::size_of::<KrunDisplayBackendConfig>(),
        )
    }?;
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::{KrunDisplayBackendConfig, KrunDisplayBasicFramebufferVtable};
    use std::mem::size_of;

    /// The pinned header this module mirrors. `deps/libkrun` is a path
    /// dependency of this crate, so the header is always in the tree.
    const HEADER: &str = include_str!("../../../deps/libkrun/src/display/libkrun_display.h");

    /// Count the fields of one `struct <name> { ... };` block.
    fn field_count(header: &str, name: &str) -> usize {
        let start = header
            .find(&format!("struct {name} {{"))
            .unwrap_or_else(|| panic!("the pinned header has no struct {name}"));
        let body = &header[start..];
        let end = body.find("};").expect("unterminated struct");
        body[..end]
            .lines()
            .filter(|line| {
                // Field declarations end with ';' and may carry a `// comment`.
                let code = line.split("//").next().unwrap_or_default().trim();
                code.ends_with(';') && !code.starts_with("struct ") && !code.contains("typedef")
            })
            .count()
    }

    /// A vtable or backend struct that falls behind the fork's header is
    /// rejected at launch as `krun_gpu_device_new: invalid parameter`, so pin
    /// the mirror to the header instead of rediscovering it per rebase.
    #[test]
    fn display_mirror_matches_the_pinned_header() {
        let pointer = size_of::<*const std::ffi::c_void>();
        let framebuffer = field_count(HEADER, "krun_display_basic_framebuffer_vtable");
        let dmabuf = field_count(HEADER, "krun_display_dmabuf_vtable");
        assert_eq!(framebuffer, 9, "unexpected basic framebuffer vtable shape");
        assert_eq!(dmabuf, 6, "unexpected dmabuf vtable shape");

        assert_eq!(
            size_of::<KrunDisplayBasicFramebufferVtable>(),
            framebuffer * pointer
        );
        // features + create_userdata + create, then the vtable union (the larger
        // of the two vtable variants).
        assert_eq!(
            size_of::<KrunDisplayBackendConfig>(),
            3 * pointer + framebuffer.max(dmabuf) * pointer
        );
    }
}
