//! Which tiles and rooms there are (from Home Assistant's state and the
//! settings), and what a tile shows: its icon, name and temperature.

use super::*;

pub(super) fn icon_for(e: &ha::Entity) -> char {
    let c = match e.domain() {
        "light" => 0xf0eb,
        "switch" | "input_boolean" => 0xf011,
        "fan" => 0xf021,
        "cover" => 0xf0db,
        "lock" => {
            if e.state == "locked" {
                0xf023
            } else {
                0xf09c
            }
        }
        "media_player" => {
            if e.state == "playing" {
                0xf04c
            } else {
                0xf04b
            }
        }
        "climate" => {
            if e.state == "cool" {
                0xf2dc
            } else {
                0xf06d
            }
        }
        "binary_sensor" => 0xf0f3,
        "scene" | "script" => 0xf04b,
        "vacuum" => 0xf015,
        "weather" => 0xf0c2,
        "sensor" => match e.unit.as_str() {
            "°C" | "°F" => 0xf2c9,
            "%" => 0xf043,
            "W" | "kW" | "kWh" | "Wh" | "V" | "A" => 0xf0e7,
            _ => 0xf0e4,
        },
        _ => 0xf015,
    };
    char::from_u32(c).unwrap()
}

pub(super) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f
            .to_uppercase()
            .chain(c.map(|ch| if ch == '_' { ' ' } else { ch }))
            .collect(),
        None => String::new(),
    }
}

pub(super) const CLIMATE_BTN: i32 = s(72);

/// Thermostat tile buttons: (plus x, minus x, top y).
pub(super) fn climate_buttons(x: i32, y: i32, w: i32, h: i32) -> (i32, i32, i32) {
    let plus = x + w - M - CLIMATE_BTN;
    let minus = plus - CLIMATE_BTN - s(110); // room for the target temperature in between
    (plus, minus, y + h - M - CLIMATE_BTN)
}

pub(super) fn fmt_temp(t: f64) -> String {
    if (t * 10.0) as i64 % 10 == 0 {
        format!("{:.0}°", t)
    } else {
        format!("{:.1}°", t)
    }
}

/// Tiles from `[[tile]]` in touchos.toml, or an automatic selection limited to
/// `dashboard.types`; plus the rooms (HA areas) that contain them.
pub(super) fn tile_entities(st: &ha::HaState) -> (Vec<Tile>, Vec<(RoomKey, String)>) {
    let cfg = config::get();
    let area_of = |id: &str| st.entity_area.get(id).cloned();
    let configured: Vec<Tile> = cfg
        .tiles
        .iter()
        .map(|t| Tile {
            entity: t.entity.clone(),
            name: t.name.clone(),
            area: area_of(&t.entity),
        })
        .collect();
    let tiles = if !configured.is_empty() {
        configured
    } else {
        let types = &cfg.dashboard.types;
        let mut auto = Vec::new();
        for d in types {
            for e in st
                .entities
                .values()
                .filter(|e| e.domain() == d && !st.hidden.contains(&e.id))
            {
                if d == "sensor" && !matches!(e.unit.as_str(), "°C" | "%" | "W" | "kWh") {
                    continue;
                }
                auto.push(Tile {
                    entity: e.id.clone(),
                    name: None,
                    area: area_of(&e.id),
                });
            }
        }
        auto
    };

    // Rooms, in the configured order or alphabetically; "Other" for tiles without an area.
    let has = |k: &RoomKey| {
        tiles.iter().any(|t| match k {
            RoomKey::Other => t.area.is_none(),
            RoomKey::Area(a) => t.area.as_ref() == Some(a),
            RoomKey::All => true,
        })
    };
    let mut rooms: Vec<(RoomKey, String)> = Vec::new();
    match &cfg.dashboard.rooms {
        Some(names) => {
            for n in names {
                let key = if n.eq_ignore_ascii_case("other") {
                    Some(RoomKey::Other)
                } else {
                    st.areas
                        .iter()
                        .find(|(id, name)| {
                            name.eq_ignore_ascii_case(&n) || id.eq_ignore_ascii_case(&n)
                        })
                        .map(|(id, _)| RoomKey::Area(id.clone()))
                };
                match key {
                    Some(k) if has(&k) => rooms.push((k, n.clone())),
                    Some(_) => {}
                    None => ::log::warn!("ui: room \"{n}\" not found in Home Assistant"),
                }
            }
        }
        None => {
            for (id, name) in &st.areas {
                let k = RoomKey::Area(id.clone());
                if has(&k) {
                    rooms.push((k, name.clone()));
                }
            }
            if !rooms.is_empty() && has(&RoomKey::Other) {
                rooms.push((RoomKey::Other, "Other".to_string()));
            }
        }
    }
    if rooms.is_empty() {
        rooms.push((RoomKey::All, "All".to_string()));
    }
    (tiles, rooms)
}
