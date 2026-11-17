//! The JSON API behind the settings page.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Map, Value as Json, json};
use touchos_config::patch::{self, Op, REDACTED};
use touchos_config::schema::{Apply, FIELDS, Kind, TILE_FIELDS};
use touchos_config::toml::{Document, Value};
use touchos_config::{Config, Field};
use touchos_http::{Request, Response};
use touchos_config::client::{Client, Update};
use touchos_rt::shm::{self, Board, Shared};
use touchos_rt::{Local, cap, ipc, sys, time};
use touchos_webauth::{PairError, StartError};

use crate::{Rng, auth, error};

const COOKIE: &str = "touchos_sid";

fn ok(v: Json) -> Response {
    Response::json(200, v.to_string())
}

pub fn signed_in(req: &Request) -> bool {
    let now = time::now_ms();
    req.cookie(COOKIE)
        .is_some_and(|t| auth(|a| a.check(now, t)))
}

pub async fn handle(req: &Request) -> Response {
    let m = req.method.as_str();
    let p = req.path.as_str();
    // Open to everyone: pairing.
    match (m, p) {
        ("GET", "/api/session") => return ok(json!({ "paired": signed_in(req) })),
        ("POST", "/api/pair/start") => return pair_start(),
        ("POST", "/api/pair") => return pair(req),
        _ => {}
    }
    if !signed_in(req) {
        return error(401, "not paired");
    }
    match (m, p) {
        ("POST", "/api/logout") => {
            if let Some(t) = req.cookie(COOKIE) {
                auth(|a| a.logout(t));
            }
            ok(json!({})).header("Set-Cookie", format!("{COOKIE}=; Path=/; Max-Age=0"))
        }
        ("GET", "/api/schema") => ok(schema()),
        ("GET", "/api/config") => get_config(),
        ("PATCH", "/api/config") => patch_config(req),
        ("GET", "/api/status") => ok(status()),
        ("GET", "/api/log") => log(req),
        ("GET", "/api/ha/entities") => ok(entities()),
        ("GET", "/api/zones") => ok(json!(touchos_civil::listed().collect::<Vec<_>>())),
        ("GET", "/api/update") => ok(update_state()),
        ("POST", "/api/reboot") => {
            PENDING_REBOOT.with(|p| *p = Some(1000));
            ok(json!({ "rebooting": true }))
        }
        _ => error(404, "no such API"),
    }
}

// ---- pairing ----

fn pair_start() -> Response {
    let now = time::now_ms();
    match auth(|a| a.start_pairing(now, &mut Rng)) {
        Ok(pin) => {
            let secs = touchos_webauth::PIN_TTL_MS / 1000;
            let mut data = Vec::from(pin.as_bytes());
            data.extend_from_slice(&(secs as u32).to_le_bytes());
            let shown = cap("ui")
                .and_then(|ui| ipc::call(ui, touchos_proto::ui::SHOW_PIN, &data, &[]).ok())
                .is_some_and(|m| m.label == touchos_proto::ui::OK);
            if !shown {
                auth(|a| a.cancel_pairing());
                return error(503, "the panel can't show a PIN right now");
            }
            log::info!("pairing: PIN shown on the panel");
            if log_pin() {
                // Development only (`cargo xtask pair` reads this line).
                log::info!("pairing: PIN {pin} (debug.log_pin)");
            }
            ok(json!({ "expires_in": secs }))
        }
        Err(StartError::TooSoon { retry_after_ms } | StartError::Locked { retry_after_ms }) => {
            Response::json(
                429,
                json!({ "error": "wait before asking for another PIN", "retry_after": retry_after_ms.div_ceil(1000) })
                    .to_string(),
            )
        }
    }
}

/// debug.log_pin, read now (it applies without a restart).
fn log_pin() -> bool {
    client()
        .and_then(|c| c.get().ok())
        .is_some_and(|(_, cfg)| cfg.debug.log_pin)
}

fn pair(req: &Request) -> Response {
    let Ok(body) = serde_json::from_slice::<Json>(&req.body) else {
        return error(400, "expected {\"pin\": \"123456\"}");
    };
    let typed = body["pin"].as_str().unwrap_or("");
    let now = time::now_ms();
    match auth(|a| a.pair(now, typed, &mut Rng)) {
        Ok(token) => {
            if let Some(ui) = cap("ui") {
                let _ = ipc::call(ui, touchos_proto::ui::HIDE_PIN, &[], &[]);
            }
            log::info!("pairing: a browser paired");
            let max_age = auth(|a| a.session_max_secs());
            ok(json!({ "paired": true })).header(
                "Set-Cookie",
                format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"),
            )
        }
        Err(PairError::Wrong { tries_left }) => Response::json(
            403,
            json!({ "error": "wrong PIN", "tries_left": tries_left }).to_string(),
        ),
        Err(PairError::NoPin) => error(409, "no PIN is being shown; ask for a new one"),
        Err(PairError::Locked { retry_after_ms }) => {
            if let Some(ui) = cap("ui") {
                let _ = ipc::call(ui, touchos_proto::ui::HIDE_PIN, &[], &[]);
            }
            log::warn!(
                "pairing: too many wrong PINs; locked for {} s",
                retry_after_ms / 1000
            );
            Response::json(
                429,
                json!({ "error": "too many wrong PINs", "retry_after": retry_after_ms.div_ceil(1000) })
                    .to_string(),
            )
        }
    }
}

// ---- settings ----

fn kind_json(k: Kind) -> Json {
    match k {
        Kind::Int { min, max } => json!({ "type": "int", "min": min, "max": max }),
        Kind::Bool => json!({ "type": "bool" }),
        Kind::Str { max } => json!({ "type": "text", "max": max }),
        Kind::Token | Kind::Psk => json!({ "type": "secret" }),
        Kind::Enum(opts) => json!({ "type": "enum", "options": opts }),
        Kind::DomainList | Kind::NameList => json!({ "type": "list" }),
        Kind::Host => json!({ "type": "text", "hint": "host" }),
        Kind::Endpoint => json!({ "type": "text", "hint": "IP:port" }),
        Kind::TimeRange => json!({ "type": "text", "hint": "HH:MM-HH:MM" }),
        Kind::EntityId => json!({ "type": "text", "hint": "domain.name" }),
        Kind::TimeZone => json!({ "type": "zone" }),
        Kind::HostName => json!({ "type": "text", "max": 63, "hint": "touchos-xxxx" }),
    }
}

fn value_json(v: &Value) -> Json {
    match v {
        Value::Str(s) => Json::String(s.clone()),
        Value::Int(i) => json!(i),
        Value::Bool(b) => json!(b),
        Value::List(l) => Json::Array(l.iter().map(value_json).collect()),
    }
}

fn field_json(f: &Field) -> Json {
    json!({
        "id": f.id,
        "section": f.section(),
        "label": f.label,
        "help": f.help,
        "kind": kind_json(f.kind),
        "default": f.default.value().as_ref().map(value_json),
        "apply": match f.apply {
            Apply::Live => "live",
            Apply::Restart(_) => "restart",
            Apply::Reboot => "reboot",
        },
    })
}

fn schema() -> Json {
    json!({
        "sections": [
            ["homeassistant", "Home Assistant"],
            ["wifi", "Wi-Fi"],
            ["display", "Display"],
            ["dashboard", "Dashboard"],
            ["time", "Time"],
            ["net", "Network"],
            ["web", "Web interface"],
            ["debug", "Debugging"],
        ],
        "fields": FIELDS.iter().filter(|f| !f.hidden).map(field_json).collect::<Vec<_>>(),
        "tile_fields": TILE_FIELDS.iter().map(field_json).collect::<Vec<_>>(),
    })
}

fn client() -> Option<Client> {
    Client::new("config")
}

fn get_config() -> Response {
    let Some(c) = client() else {
        return error(503, "config server not reachable");
    };
    let Ok((version, text)) = c.text() else {
        return error(503, "config server not reachable");
    };
    let doc = Document::parse(&text).unwrap_or_default();
    let raw = doc.raw();
    let mut values = Map::new();
    for f in FIELDS.iter().filter(|f| !f.hidden) {
        let v = raw.values.get(f.id);
        let j = if f.kind.is_secret() {
            json!({ "set": v.is_some_and(|v| v.as_str() == Some(REDACTED)) })
        } else {
            match v {
                Some(v) => value_json(v),
                None => continue,
            }
        };
        values.insert(f.id.to_string(), j);
    }
    let tiles: Vec<Json> = Config::parse(&text)
        .map(|(cfg, _)| cfg.tiles)
        .unwrap_or_default()
        .into_iter()
        .map(|t| json!([t.entity, t.name.unwrap_or_default()]))
        .collect();
    let problems: Vec<String> = c
        .status()
        .map(|(_, _, p)| p.lines().map(String::from).collect())
        .unwrap_or_default();
    ok(json!({
        "version": version,
        "values": values,
        "tiles": tiles,
        "problems": problems,
        "saved": false,
    }))
}

fn json_value(j: &Json) -> Option<Value> {
    Some(match j {
        Json::String(s) => Value::Str(s.clone()),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Int(n.as_i64()?),
        Json::Array(a) => Value::List(a.iter().map(json_value).collect::<Option<_>>()?),
        _ => return None,
    })
}

fn patch_config(req: &Request) -> Response {
    let Ok(body) = serde_json::from_slice::<Json>(&req.body) else {
        return error(400, "expected JSON");
    };
    let Some(version) = body["version"].as_u64() else {
        return error(400, "missing version");
    };
    let mut ops = Vec::new();
    if let Some(set) = body["set"].as_object() {
        for (id, j) in set {
            let secret = FIELDS.iter().any(|f| f.id == id && f.kind.is_secret());
            // An empty secret means "keep the current one".
            if secret && j.as_str().is_some_and(|s| s.is_empty()) {
                continue;
            }
            match json_value(j) {
                Some(v) => ops.push(Op::Set(id.clone(), v)),
                None => return error(400, &format!("{id}: unsupported value")),
            }
        }
    }
    if let Some(unset) = body["unset"].as_array() {
        for id in unset.iter().filter_map(Json::as_str) {
            ops.push(Op::Unset(id.to_string()));
        }
    }
    if let Some(tiles) = body["tiles"].as_array() {
        let t = tiles
            .iter()
            .map(|t| {
                (
                    t[0].as_str().unwrap_or("").to_string(),
                    t[1].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        ops.push(Op::Tiles(t));
    }
    if ops.is_empty() {
        return ok(json!({ "version": version, "restart": [], "saved": false }));
    }
    let Some(c) = client() else {
        return error(503, "config server not reachable");
    };
    match c.update(version, &patch::to_text(&ops)) {
        Ok(Update::Ok {
            version,
            effects,
            saved,
        }) => {
            let mut restart = Vec::new();
            if effects.restart_ha {
                restart.push("Home Assistant connection");
            }
            if effects.restart_wifi {
                restart.push("Wi-Fi");
            }
            if effects.reboot {
                restart.push("reboot");
            }
            log::info!("settings changed through the web interface");
            ok(
                json!({ "version": version, "restart": restart, "saved": saved == 1, "save_failed": saved == 2 }),
            )
        }
        Ok(Update::Conflict(cur)) => Response::json(
            409,
            json!({ "error": "the settings changed meanwhile; reload", "version": cur })
                .to_string(),
        ),
        Ok(Update::Invalid(why)) => {
            let errors: Vec<Json> = why
                .lines()
                .map(|l| {
                    let (f, m) = l.split_once(": ").unwrap_or(("", l));
                    json!({ "field": f, "message": m })
                })
                .collect();
            Response::json(422, json!({ "errors": errors }).to_string())
        }
        Ok(Update::Denied) => error(403, "not allowed to change settings"),
        Err(_) => error(503, "config server not reachable"),
    }
}

// ---- updates ----

/// The update server's state (see touchos_proto::update), as JSON.
fn update_state() -> Json {
    use touchos_proto::update::{Phase, State};
    let st: Option<State> = board_bytes(&UPDATE_STATE, "update_state").and_then(|b| touchos_proto::decode(&b));
    let Some(st) = st else {
        return json!({ "enabled": false });
    };
    let phase = match &st.phase {
        Phase::Idle => json!({ "state": "idle" }),
        Phase::Writing { name, done, total } => {
            json!({ "state": "writing", "file": name, "done": done, "total": total })
        }
        Phase::Restarting { build } => json!({ "state": "restarting", "build": build }),
        Phase::Failed(why) => json!({ "state": "failed", "error": why }),
    };
    json!({
        "enabled": st.enabled,
        "running": st.running.map(|c| c.to_string()),
        "build": st.build,
        "other": st.other,
        "phase": phase,
    })
}

/// Hand an uploaded bundle (in `mem`, `len` bytes) to the update server,
/// which checks it before answering.
pub fn install(mem: u32, len: usize) -> Response {
    use touchos_proto::update as up;
    let Some(update) = cap("update") else {
        return error(503, "updates aren't available on this panel");
    };
    let Ok(ro) = sys::cap_mint(mem, touchos_abi::rights::MAP_READ, 0) else {
        return error(500, "out of capabilities");
    };
    let r = ipc::call(update, up::INSTALL, &(len as u32).to_le_bytes(), &[ro]);
    let _ = sys::cap_drop(ro);
    match r {
        Ok(m) if m.label == up::OK => Response::json(
            202,
            json!({ "build": String::from_utf8_lossy(m.data()) }).to_string(),
        ),
        Ok(m) if m.label == up::BUSY => error(409, "an update is already being installed"),
        Ok(m) => error(422, &String::from_utf8_lossy(m.data())),
        Err(_) => error(503, "the update server isn't reachable"),
    }
}

// ---- status, log, entities ----

/// The boards we read, mapped once (ha_state is 2 MiB).
static HA_STATE: Local<Option<Shared<Board>>> = Local::new(None);
static UPDATE_STATE: Local<Option<Shared<Board>>> = Local::new(None);

/// The value on the board we were given as `name` (mapped on first use).
fn board_bytes(slot: &Local<Option<Shared<Board>>>, name: &str) -> Option<Vec<u8>> {
    slot.with(|b| {
        if b.is_none() {
            *b = shm::board(name, false);
        }
        let mut buf = Vec::new();
        b.as_ref()?.read(0, &mut buf)?;
        Some(buf)
    })
}

/// The dashboard's copy of the Home Assistant state.
fn ha_snapshot() -> Option<touchos_proto::ha::Snapshot> {
    touchos_proto::decode(&board_bytes(&HA_STATE, "ha_state")?)
}

fn status() -> Json {
    let net = touchos_netc::status();
    let ip = net
        .and_then(|s| s.ipv4)
        .map(|(ip, p)| format!("{}.{}.{}.{}/{p}", ip[0], ip[1], ip[2], ip[3]));
    let ha = ha_snapshot();
    let cfg = client().and_then(|c| c.status().ok());
    let arg = |k: &str| touchos_rt::arg(k).filter(|v| *v != "-");
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "slot": arg("slot"),
        "build": arg("build"),
        "uptime_s": time::now_ms() / 1000,
        "clock_set": time::unix_now() > 1_600_000_000,
        "ip": ip,
        "ha": {
            "connected": ha.as_ref().is_some_and(|h| h.connected),
            "entities": ha.as_ref().map_or(0, |h| h.entities.len()),
        },
        "config": {
            "version": cfg.as_ref().map(|c| c.0),
            "problems": cfg.map(|c| c.2.lines().map(String::from).collect::<Vec<_>>()).unwrap_or_default(),
        },
    })
}

fn log(req: &Request) -> Response {
    let n: usize = req
        .param("n")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
        .min(2000);
    let Some(system) = cap("system") else {
        return error(503, "no access to the log");
    };
    let mut lines: alloc::collections::VecDeque<String> = alloc::collections::VecDeque::new();
    let mut cursor = 0;
    let mut buf = alloc::vec![0u8; 8192];
    while let Ok((len, next)) = sys::klog_read(system, &mut buf, cursor) {
        if len == 0 {
            break;
        }
        cursor = next;
        for l in String::from_utf8_lossy(&buf[..len]).lines() {
            if lines.len() == n {
                lines.pop_front();
            }
            lines.push_back(l.to_string());
        }
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    Response::new(200, "text/plain; charset=utf-8", out)
}

fn entities() -> Json {
    let Some(h) = ha_snapshot() else {
        return json!([]);
    };
    Json::Array(
        h.entities
            .values()
            .filter(|e| !h.hidden.contains(&e.id))
            .map(|e| json!([e.id, e.name]))
            .collect(),
    )
}

// ---- reboot ----

static PENDING_REBOOT: touchos_rt::Local<Option<u64>> = touchos_rt::Local::new(None);

/// After the response went out: the ms to wait, if a reboot was asked for.
pub fn take_pending_reboot() -> Option<u64> {
    PENDING_REBOOT.with(|p| p.take())
}

pub fn reboot_now() {
    let Some(s) = cap("system") else { return };
    log::warn!("rebooting (asked through the web interface)");
    let _ = sys::reboot(s);
}
