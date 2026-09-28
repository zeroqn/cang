//! libkrun VM launch integration.
//!
//! The binding itself lives in the `cang-libkrun` crate, which links libkrun's
//! Rust API into cang; this module keeps cang's launch policy - what the
//! payload, the init config, the devices and the VMM builder are configured
//! with, and in which order.

mod launcher;

pub(in crate::runtime) use cang_libkrun::LinkedLibkrunApi;
pub(in crate::runtime) use launcher::DirectLibkrunLauncher;
pub(in crate::runtime) use launcher::RENDER_SERVER_FD_ENV;

#[cfg(test)]
mod tests;
