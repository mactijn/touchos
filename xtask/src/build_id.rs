//! The build id every image carries on its kernel command line
//! (`build=<id>`): init, the update server and the settings page show it.

use std::process::Command;

use crate::root;

/// `git describe` of the source tree, e.g. "346a937-dirty".
pub fn build_id() -> String {
    Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .current_dir(root())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// `cmdline` with `build=<id>` first.
pub fn with_build(cmdline: &str) -> String {
    format!("build={} {cmdline}", build_id()).trim().to_string()
}
