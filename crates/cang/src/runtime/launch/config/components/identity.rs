//! Host identity contribution to guest bootstrap environment.

use super::super::model::{HOST_GID_ENV, HOST_UID_ENV};

pub(crate) fn required_env(host_uid: u32, host_gid: u32) -> Vec<(String, String)> {
    vec![
        (HOST_UID_ENV.to_owned(), host_uid.to_string()),
        (HOST_GID_ENV.to_owned(), host_gid.to_string()),
    ]
}
