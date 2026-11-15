//! Link with the user-program layout (libs/rt/user.ld), and embed the
//! settings page: the Angular app in build/webui (built from webui/ by
//! `cargo xtask build`), each file as is and gzip-compressed.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

include!("../../libs/rt/link.rs");

fn main() {
    link_user_program();

    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/webui");
    println!("cargo:rerun-if-changed={}", dist.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dist)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| p.is_file() && !name(p).starts_with('.') && mime(p).is_some());
    files.sort();
    if !files.iter().any(|p| name(p) == "index.html") {
        println!(
            "cargo:warning=build/webui/index.html missing: the settings page is a placeholder (cargo xtask build makes it; it needs Node.js)"
        );
        let p = out.join("index.html");
        std::fs::write(
            &p,
            "<!doctype html><title>touchos</title><p>This build of touchos has no settings page: \
             it was built without Node.js (see webui/README.md).</p>",
        )
        .unwrap();
        files = vec![p];
    }

    let mut code = String::from("static ASSETS: &[Asset] = &[\n");
    for p in &files {
        let n = name(p);
        let data = std::fs::read(p).unwrap();
        let gz = out.join(format!("{n}.gz"));
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&data).unwrap();
        std::fs::write(&gz, enc.finish().unwrap()).unwrap();
        writeln!(
            code,
            "    Asset {{ path: {:?}, mime: {:?}, body: include_bytes!({:?}), gzip: include_bytes!({:?}), immutable: {} }},",
            format!("/{n}"),
            mime(p).unwrap(),
            p.canonicalize().unwrap(),
            gz,
            hashed(n),
        )
        .unwrap();
    }
    code.push_str("];\n");
    std::fs::write(out.join("assets.rs"), code).unwrap();
}

fn name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn mime(p: &Path) -> Option<&'static str> {
    Some(match p.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" | "webmanifest" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// Angular's output hashing (`main-YSV7KRGO.js`): the name changes with the
/// content, so browsers may keep such a file forever.
fn hashed(n: &str) -> bool {
    let stem = n.rsplit_once('.').map_or(n, |(s, _)| s);
    stem.rsplit_once('-').is_some_and(|(_, h)| {
        h.len() >= 8 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}
