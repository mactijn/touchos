//! The settings page: the Angular app in webui/, built into build/webui,
//! which servers/web embeds.
//!
//! Needs Node.js (npm). Without it, an existing build/webui is used as is;
//! with no build at all the web server embeds a placeholder page.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use crate::{root, sh};

pub fn build(force: bool) -> Result<()> {
    let src = root().join("webui");
    let out = root().join("build/webui");
    let stamp = out.join(".built");
    let built = mtime(&stamp);
    if !force && built.is_some() && newest(&src)? <= built.unwrap() {
        return Ok(());
    }
    if Command::new("npm").arg("--version").output().is_err() {
        if out.join("index.html").exists() {
            println!("note: npm not found; using the existing build/webui (may be stale)");
            return Ok(());
        }
        println!(
            "warning: npm not found: the settings page will be a placeholder (install Node.js 24+)"
        );
        return Ok(());
    }
    // Dependencies: install when missing or when package-lock.json changed.
    let installed = mtime(&src.join("node_modules/.package-lock.json"));
    let lock = mtime(&src.join("package-lock.json"));
    if installed.is_none() || lock > installed {
        sh(Command::new("npm")
            .current_dir(&src)
            .args(["ci", "--no-audit", "--no-fund"]))?;
    }
    sh(Command::new("npx").current_dir(&src).args(["ng", "build"]))?;
    std::fs::write(&stamp, b"").context("build/webui/.built")?;
    Ok(())
}

/// `npm start` in webui/ with the API forwarded to a panel.
pub fn serve(panel: &str) -> Result<()> {
    let src = root().join("webui");
    if !src.join("node_modules").exists() {
        sh(Command::new("npm")
            .current_dir(&src)
            .args(["ci", "--no-audit", "--no-fund"]))?;
    }
    let status = Command::new("npx")
        .current_dir(&src)
        .args(["ng", "serve"])
        .env("TOUCHOS_PANEL", panel)
        .status()
        .context("npx ng serve (is Node.js installed?)")?;
    if !status.success() {
        bail!("ng serve failed");
    }
    Ok(())
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// The newest change in the app's sources (not node_modules, not caches).
fn newest(dir: &Path) -> Result<SystemTime> {
    let mut t = mtime(dir).unwrap_or(SystemTime::UNIX_EPOCH);
    for e in std::fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let e = e?;
        let name = e.file_name();
        if matches!(name.to_str(), Some("node_modules" | ".angular" | "dist")) {
            continue;
        }
        let p = e.path();
        let m = if p.is_dir() {
            newest(&p)?
        } else {
            mtime(&p).unwrap_or(t)
        };
        t = t.max(m);
    }
    Ok(t)
}
