//! Launch planning and helper/libkrun execution contract assembly.
//!
//! `plan` resolves host-side launch intent from CLI/config/environment. `config`
//! owns the serialized helper/libkrun contract. Component modules below this
//! namespace make each launch contribution discoverable by semantic owner.

pub(crate) mod components;
pub(crate) mod config;
pub(crate) mod plan;

pub(crate) use components::persistent_disks::nix_lower::NixLowerDiskStatus;
pub(crate) use components::persistent_disks::{
    HostPersistentDiskPreparer, PersistentDiskPreparer, prepare_nix_lower_block,
};
pub(crate) use plan::LaunchPlan;
