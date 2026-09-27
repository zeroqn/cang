//! libkrun VM launch integration.

mod api;
mod dynamic;
mod launcher;

#[cfg(test)]
pub(in crate::runtime::vm::libkrun) use api::LibkrunApi;
pub(in crate::runtime) use dynamic::DynamicLibkrunApi;
pub(in crate::runtime) use launcher::DirectLibkrunLauncher;
pub(in crate::runtime) use launcher::RENDER_SERVER_FD_ENV;

#[cfg(test)]
use dynamic::{
    CANG_LIBKRUN_COMPAT_NET_FEATURES, planned_libkrun_init_load_order_for_exe,
    planned_libkrun_load_order, planned_libkrun_load_order_for_exe,
    required_symbol_presence_for_test,
};

#[cfg(test)]
mod tests;
