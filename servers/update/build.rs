// Link with the user-program layout (libs/rt/user.ld), and build in the
// public key that updates must be signed with (config/update-key.pub,
// made by `cargo xtask keygen`). Without it, updates are refused.
include!("../../libs/rt/link.rs");

fn main() {
    link_user_program();
    let key = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/update-key.pub");
    println!("cargo:rerun-if-changed={key}");
    let bytes: Option<Vec<u8>> = std::fs::read_to_string(key).ok().and_then(|t| {
        let t = t.trim();
        (t.len() == 64)
            .then(|| {
                (0..32)
                    .map(|i| u8::from_str_radix(&t[2 * i..2 * i + 2], 16).ok())
                    .collect()
            })
            .flatten()
    });
    let code = match bytes {
        Some(b) => format!("pub const PUBLIC_KEY: Option<[u8; 32]> = Some({b:?});\n"),
        None => "pub const PUBLIC_KEY: Option<[u8; 32]> = None;\n".to_string(),
    };
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("key.rs");
    std::fs::write(out, code).unwrap();
}
