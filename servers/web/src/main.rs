//! The panel's settings page: a small HTTP server on the LAN.
//!
//! A browser first pairs: it asks the panel to show a PIN and types it in;
//! then it gets a session cookie. With that it can read the settings
//! (secrets are never sent back, only whether they are set) and change
//! them; the config server checks every change against the schema.
//!
//! Defenses: changes need a custom header and a matching Origin (CSRF);
//! responses forbid framing and inline scripts, and allow inline styles
//! only with the page load's nonce; requests have size and time limits; six
//! connections at a time.
//!
//! The page itself is an Angular app (webui/), embedded at build time.
//!
//! Capabilities: "net", "config" (ADMIN), "ui" (to show the PIN),
//! "ha_state" (read-only, for status and entity names), "system" (log and
//! reboot).

#![no_std]
#![no_main]

extern crate alloc;

mod api;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use log::{info, warn};
use touchos_http::{self as http, Request, Response};
use touchos_netc::{Listener, TcpStream};
use touchos_rt::{Local, exec, time};
use touchos_webauth::Auth;

touchos_rt::entry!(main);

/// Browsers open up to six connections to a host at once.
const MAX_CONNS: u32 = 6;
const HEAD_TIMEOUT_MS: u64 = 10_000;
const IDLE_TIMEOUT_MS: u64 = 15_000;

pub struct Rng;
impl touchos_webauth::Rng for Rng {
    fn fill(&mut self, out: &mut [u8]) {
        touchos_rt::rand::fill(out)
    }
}

pub static AUTH: Local<Option<Auth>> = Local::new(None);
/// Connections being served.
static CONNS: Local<u32> = Local::new(0);

pub fn auth<R>(f: impl FnOnce(&mut Auth) -> R) -> R {
    AUTH.with(|a| f(a.as_mut().expect("auth")))
}

/// What came in: a request, or an upload that was handled while it
/// arrived.
enum Incoming {
    Request(Request),
    Done(Response),
}

/// Read one request, with limits on size and time.
async fn read_request(s: &mut TcpStream) -> Result<Incoming, u16> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 2048];
    let start = time::now_ms();
    let mut last = start;
    loop {
        match http::parse_head(&buf) {
            // An update: its body goes straight into shared memory.
            Ok((req, head_len, len)) if req.method == "POST" && req.path == "/api/update" => {
                let already = buf.split_off(head_len);
                return Ok(Incoming::Done(upload(s, req, already, len).await));
            }
            Ok(_) => match http::parse(&buf) {
                Ok(r) => return Ok(Incoming::Request(r)),
                Err(http::Error::Partial) => {}
                Err(e) => return Err(e.status()),
            },
            Err(http::Error::Partial) => {}
            Err(e) => return Err(e.status()),
        }
        let now = time::now_ms();
        if now - last > IDLE_TIMEOUT_MS || (buf.len() < 4 && now - start > HEAD_TIMEOUT_MS) {
            return Err(408);
        }
        match s.try_read(&mut chunk) {
            Ok(Some(0)) | Err(_) => return Err(400),
            Ok(Some(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                last = now;
                if buf.len() > http::MAX_HEAD + http::MAX_BODY {
                    return Err(413);
                }
            }
            Ok(None) => {
                exec::wait_bits_or_ms(s.bit(), 100).await;
            }
        }
    }
}

/// `POST /api/update`: a signed update bundle (application/octet-stream),
/// up to touchos_update::MAX_BUNDLE bytes. Checked like every change
/// (X-Touchos header, origin, session) before its body is read.
async fn upload(s: &mut TcpStream, req: Request, already: Vec<u8>, len: usize) -> Response {
    if !req.from_our_page() {
        return error(
            403,
            "request refused (missing X-Touchos header or wrong origin)",
        );
    }
    if !api::signed_in(&req) {
        return error(401, "not paired");
    }
    if len == 0 || len > touchos_update::MAX_BUNDLE {
        return error(413, "not an update file (wrong size)");
    }
    let Ok(mem) = touchos_rt::sys::mem_create(len as u64, 0) else {
        return error(507, "not enough memory for the upload");
    };
    let r = receive(s, mem, already, len).await;
    let resp = match r {
        Ok(()) => {
            info!("update upload: {} KiB received", len / 1024);
            api::install(mem, len)
        }
        Err(status) => error(status, "upload incomplete"),
    };
    let _ = touchos_rt::sys::cap_drop(mem);
    resp
}

/// The body of an upload into `mem`.
async fn receive(s: &mut TcpStream, mem: u32, already: Vec<u8>, len: usize) -> Result<(), u16> {
    let map = touchos_rt::caps::Mapping::map(mem, true).map_err(|_| 507u16)?;
    let dst = map.ptr::<u8>();
    let mut pos = already.len().min(len);
    unsafe { core::ptr::copy_nonoverlapping(already.as_ptr(), dst, pos) };
    let mut chunk = alloc::vec![0u8; 16 * 1024];
    let mut last = time::now_ms();
    while pos < len {
        match s.try_read(&mut chunk[..(len - pos).min(16 * 1024)]) {
            Ok(Some(0)) | Err(_) => return Err(400),
            Ok(Some(n)) => {
                unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(pos), n) };
                pos += n;
                last = time::now_ms();
            }
            Ok(None) => {
                if time::now_ms() - last > IDLE_TIMEOUT_MS {
                    return Err(408);
                }
                // Until the net server has more (it signals our bit).
                exec::wait_bits_or_ms(s.bit(), 100).await;
            }
        }
    }
    Ok(())
}

/// Changes must come from our own page (custom header, same origin), as
/// JSON.
fn csrf_ok(req: &Request) -> bool {
    let json = req.body.is_empty() || req.header("content-type").is_some_and(|c| c.starts_with("application/json"));
    req.from_our_page() && json
}

/// The Content-Security-Policy. `nonce`: the one inline-style permit of
/// this page load (Angular Material adds its component styles as <style>
/// elements, which carry it); none for everything but the page itself.
fn csp(nonce: Option<&str>) -> String {
    let style = match nonce {
        Some(n) => format!("style-src 'self' 'nonce-{n}'; "),
        None => String::new(),
    };
    format!(
        "default-src 'self'; {style}frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
    )
}

fn secure(mut r: Response, api: bool) -> Response {
    if !r
        .headers
        .iter()
        .any(|(n, _)| n == "Content-Security-Policy")
    {
        r = r.header("Content-Security-Policy", csp(None));
    }
    r = r
        .header("X-Frame-Options", "DENY")
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer");
    if api {
        r = r.header("Cache-Control", "no-store");
    }
    r
}

pub fn error(status: u16, msg: &str) -> Response {
    Response::json(status, format!("{{\"error\":{}}}", http::json_str(msg)))
}

async fn serve_conn(mut s: TcpStream) {
    let resp = match read_request(&mut s).await {
        Err(status) => error(status, http::reason(status)),
        Ok(Incoming::Done(r)) => secure(r, true),
        Ok(Incoming::Request(req)) => {
            let api = req.path.starts_with("/api/");
            let mutating = matches!(req.method.as_str(), "POST" | "PATCH" | "PUT" | "DELETE");
            let r = if mutating && !csrf_ok(&req) {
                error(
                    403,
                    "request refused (missing X-Touchos header or wrong origin)",
                )
            } else {
                route(&req).await
            };
            secure(r, api)
        }
    };
    let _ = s.write_all(&resp.to_bytes()).await;
    // Give the server a moment to send before the socket closes.
    exec::sleep_ms(50).await;
    CONNS.with(|n| *n -= 1);
    if let Some(after) = api::take_pending_reboot() {
        exec::sleep_ms(after).await;
        api::reboot_now();
    }
}

/// One file of the settings page (the Angular app from webui/).
struct Asset {
    path: &'static str,
    mime: &'static str,
    body: &'static [u8],
    gzip: &'static [u8],
    /// The name carries a hash of the content.
    immutable: bool,
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

async fn route(req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        (_, p) if p.starts_with("/api/") => api::handle(req).await,
        ("GET", p) => match asset(p) {
            Some(a) => serve_asset(req, a),
            None => error(404, "not found"),
        },
        _ => error(405, "method not allowed"),
    }
}

/// The file for a path. The app's own pages (/settings, /tiles, …) are all
/// index.html: the app shows the right one itself.
fn asset(path: &str) -> Option<&'static Asset> {
    let find = |p: &str| ASSETS.iter().find(|a| a.path == p);
    if let Some(a) = find(path) {
        return Some(a);
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        None
    } else {
        find("/index.html")
    }
}

/// In index.html, replaced by each response's nonce (see csp()).
const NONCE_MARK: &[u8] = b"__TOUCHOS_CSP_NONCE__";

fn serve_asset(req: &Request, a: &Asset) -> Response {
    if a.path == "/index.html" {
        return serve_index(a);
    }
    let r = if req.accepts_gzip() {
        Response::new(200, a.mime, a.gzip).header("Content-Encoding", "gzip")
    } else {
        Response::new(200, a.mime, a.body)
    };
    r.header("Vary", "Accept-Encoding").header(
        "Cache-Control",
        if a.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        },
    )
}

/// The page itself, with a fresh nonce: small, so never compressed or cached.
fn serve_index(a: &Asset) -> Response {
    let mut raw = [0u8; 16];
    touchos_rt::rand::fill(&mut raw);
    let nonce: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let mut body = Vec::with_capacity(a.body.len());
    let mut rest = a.body;
    while let Some(i) = rest.windows(NONCE_MARK.len()).position(|w| w == NONCE_MARK) {
        body.extend_from_slice(&rest[..i]);
        body.extend_from_slice(nonce.as_bytes());
        rest = &rest[i + NONCE_MARK.len()..];
    }
    body.extend_from_slice(rest);
    Response::new(200, a.mime, body)
        .header("Content-Security-Policy", csp(Some(&nonce)))
        .header("Cache-Control", "no-store")
}

/// Settings that apply while running: the session length.
async fn watch_settings() {
    loop {
        let (old, new) = touchos_config::client::settings::changed().await;
        if new.web.session_hours != old.web.session_hours {
            auth(|a| a.set_session_hours(new.web.session_hours));
            info!("sessions last {} h now", new.web.session_hours);
        }
    }
}

fn main() {
    let cfg = touchos_config::client::settings::get();
    if !cfg.web.enabled {
        info!("web interface disabled (web.enabled = false)");
        return;
    }
    AUTH.with(|a| *a = Some(Auth::new(cfg.web.session_hours)));
    let port = cfg.web.port;
    exec::spawn(watch_settings());
    exec::spawn(async move {
        let mut announced = false;
        // A restarted net server forgets our listener and our service: then
        // accept fails, and we bind and register again.
        loop {
            let listener = loop {
                match Listener::bind(port, MAX_CONNS as u16) {
                    Ok(l) => break l,
                    Err(e) => {
                        warn!("can't listen on port {port}: {e}; retrying");
                        exec::sleep_ms(2000).await;
                    }
                }
            };
            if let Err(e) = touchos_netc::mdns_register("_http._tcp", "", port, &["path=/"]) {
                warn!("can't announce the settings page over mDNS: {e}");
            }
            if !announced {
                announced = true;
                let (ip, _) = touchos_netc::wait_configured().await;
                let port_part = if port == 80 { String::new() } else { format!(":{port}") };
                let host = touchos_netc::status().map(|s| s.hostname).unwrap_or_default();
                info!(
                    "settings page at http://{}.{}.{}.{}{port_part}/ and http://{host}.local{port_part}/",
                    ip[0], ip[1], ip[2], ip[3]
                );
            }
            loop {
                while CONNS.with(|n| *n) >= MAX_CONNS {
                    exec::sleep_ms(20).await;
                }
                match listener.accept().await {
                    Ok(s) => {
                        CONNS.with(|n| *n += 1);
                        exec::spawn(serve_conn(s));
                    }
                    Err(e) => {
                        warn!("accept: {e}; listening again");
                        exec::sleep_ms(1000).await;
                        break;
                    }
                }
            }
        }
    });
    exec::run()
}
