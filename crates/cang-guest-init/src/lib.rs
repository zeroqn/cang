use anyhow::Result;
use std::process::ExitCode;

// The guest bootstrap and `cang-granted` allocate through mimalloc, matching the
// allocator the image preloads for its Nix-linked binaries. Declared in the
// library so both binaries of the crate get it.
#[global_allocator]
static GLOBAL_ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;

pub mod granted;
mod guest_init;

pub fn granted_entrypoint() -> ExitCode {
    match granted::run(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cang-granted: {err:#}");
            ExitCode::from(1)
        }
    }
}

pub fn entrypoint() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cang-guest-init: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    guest_init::entrypoint()
}
