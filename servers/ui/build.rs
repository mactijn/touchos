//! Build script: user-program linker script + pre-rasterized font atlases.
//!
//! Fonts are rasterized here, on the host, so the UI needs no font engine
//! at runtime.

use std::fmt::Write as _;
use std::path::PathBuf;

struct FaceSpec {
    name: &'static str,
    file: &'static str,
    px: f32,
    chars: &'static [(u32, u32)],
}

/// ASCII, Latin-1, en dash, ellipsis.
const TEXT: &[(u32, u32)] = &[
    (0x20, 0x7e),
    (0xa0, 0xff),
    (0x2013, 0x2013),
    (0x2026, 0x2026),
];
const DIGITS: &[(u32, u32)] = &[(0x20, 0x20), (0x2d, 0x3a), (0xb0, 0xb0)];
/// Font Awesome 4 icons used by the dashboard.
const ICONS: &[(u32, u32)] = &[
    (0xf0eb, 0xf0eb), // lightbulb-o
    (0xf011, 0xf011), // power-off
    (0xf2c9, 0xf2c9), // thermometer-half
    (0xf043, 0xf043), // tint
    (0xf0e7, 0xf0e7), // bolt
    (0xf1e6, 0xf1e6), // plug
    (0xf023, 0xf023), // lock
    (0xf09c, 0xf09c), // unlock
    (0xf04b, 0xf04b), // play
    (0xf04c, 0xf04c), // pause
    (0xf015, 0xf015), // home
    (0xf185, 0xf186), // sun-o, moon-o
    (0xf021, 0xf021), // refresh (fan)
    (0xf0db, 0xf0db), // columns (cover)
    (0xf1eb, 0xf1eb), // wifi
    (0xf071, 0xf071), // warning
    (0xf0e4, 0xf0e4), // dashboard
    (0xf06d, 0xf06d), // fire
    (0xf2dc, 0xf2dc), // snowflake
    (0xf0c2, 0xf0c2), // cloud
    (0xf26c, 0xf26c), // television
    (0xf028, 0xf028), // volume-up
    (0xf0f3, 0xf0f3), // bell
    (0xf1b9, 0xf1b9), // car
    (0xf2cc, 0xf2cc), // shower
    (0xf0eb, 0xf0eb),
];

const FACES: &[FaceSpec] = &[
    FaceSpec {
        name: "TEXT_SMALL",
        file: "NotoSans-Regular.ttf",
        px: 26.0,
        chars: TEXT,
    },
    FaceSpec {
        name: "TEXT",
        file: "NotoSans-Regular.ttf",
        px: 32.0,
        chars: TEXT,
    },
    FaceSpec {
        name: "TEXT_BOLD",
        file: "NotoSans-Bold.ttf",
        px: 34.0,
        chars: TEXT,
    },
    FaceSpec {
        name: "VALUE",
        file: "NotoSans-Bold.ttf",
        px: 64.0,
        chars: TEXT,
    },
    FaceSpec {
        name: "CLOCK",
        file: "NotoSans-Bold.ttf",
        px: 120.0,
        chars: DIGITS,
    },
    FaceSpec {
        name: "ICON",
        file: "fontawesome-webfont.ttf",
        px: 64.0,
        chars: ICONS,
    },
    FaceSpec {
        name: "ICON_SMALL",
        file: "fontawesome-webfont.ttf",
        px: 32.0,
        chars: ICONS,
    },
];

include!("../../libs/rt/link.rs");

fn main() {
    link_user_program();
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed=fonts");
    println!("cargo:rerun-if-changed=build.rs");

    // Layout scale: the dashboard is laid out in design pixels (a 1920x1200
    // 10.1" panel); 3/5 for the Toon's 800x480 7" panel keeps text about the
    // same physical size (224 vs 133 dpi).
    let scale = (3, 5);
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut rs = String::new();
    writeln!(
        rs,
        "/// Layout scale (numerator, denominator) for this build's panel.\npub const SCALE: (i32, i32) = ({}, {});",
        scale.0, scale.1
    )
    .unwrap();
    for spec in FACES {
        let px = (spec.px * scale.0 as f32 / scale.1 as f32).round();
        let bytes = std::fs::read(format!("{dir}/fonts/{}", spec.file)).expect("font file");
        let font =
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("parse font");
        let mut data: Vec<u8> = Vec::new();
        let mut glyphs = String::new();
        let mut chars: Vec<u32> = spec.chars.iter().flat_map(|&(a, b)| a..=b).collect();
        chars.sort();
        chars.dedup();
        for c in chars {
            let Some(ch) = char::from_u32(c) else {
                continue;
            };
            if font.lookup_glyph_index(ch) == 0 && c != 0x20 {
                continue;
            }
            let (m, bitmap) = font.rasterize(ch, px);
            let off = data.len();
            data.extend_from_slice(&bitmap);
            writeln!(
                glyphs,
                "Glyph {{ ch: {c}, w: {}, h: {}, xmin: {}, ymin: {}, adv: {}, off: {off} }},",
                m.width,
                m.height,
                m.xmin,
                m.ymin,
                m.advance_width.round() as i32
            )
            .unwrap();
        }
        let bin = format!("font_{}.bin", spec.name.to_lowercase());
        std::fs::write(out.join(&bin), &data).unwrap();
        writeln!(
            rs,
            "pub static {}: Face = Face {{ glyphs: &[{glyphs}], data: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{bin}\")) }};",
            spec.name,
        )
        .unwrap();
    }
    std::fs::write(out.join("fonts.rs"), rs).unwrap();
}
