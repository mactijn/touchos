//! `/metrics` (Prometheus, opt-in with `metrics.enabled`, no session): the
//! kernel's figures from the STATS syscall, and the servers' metrics
//! boards (`init_metrics`, `net_metrics`, `wifi_metrics`, `ha_metrics`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use touchos_abi::stats;
use touchos_http::prom::{self, Kind};
use touchos_http::{Request, Response};
use touchos_proto::metrics::{self as pm, Metrics};
use touchos_rt::shm::{self, Board, Shared};
use touchos_rt::{Local, cap, sys};

/// The kernel's report (the STATS syscall), or None without the right.
pub fn kernel_report() -> Option<Vec<u8>> {
    sys::stats(cap("system")?).ok()
}

/// A thread's figures, summed per name (a process being replaced can
/// have a dead thread of the same name for a moment).
#[derive(Debug, Clone, Default)]
pub struct Proc {
    pub name: String,
    pub pid: u32,
    pub state: u8,
    pub priority: u8,
    pub idle: bool,
    pub cpu_us: u64,
    pub switch_ins: u64,
    pub mem_bytes: u64,
}

/// The kernel's threads, one entry per name, kernel threads included.
pub fn processes(r: &stats::Report) -> Vec<Proc> {
    let mut by: BTreeMap<String, Proc> = BTreeMap::new();
    for t in r.threads() {
        let e = by.entry(t.name().into()).or_insert_with(|| Proc { name: t.name().into(), ..Proc::default() });
        // The live thread's own fields win over a dead one's.
        if t.state != stats::state::DEAD || e.pid == 0 {
            e.pid = t.pid;
            e.state = t.state;
            e.priority = t.priority;
            e.idle = t.idle != 0;
        }
        e.cpu_us += t.cpu_us;
        e.switch_ins += t.switch_ins as u64;
        e.mem_bytes = e.mem_bytes.max(t.mem_bytes);
    }
    by.into_values().collect()
}

/// The metrics boards, mapped on first use.
static BOARDS: Local<Vec<(&'static str, Option<Shared<Board>>)>> = Local::new(Vec::new());
const BOARD_NAMES: [&str; 4] = ["init_metrics", "net_metrics", "wifi_metrics", "ha_metrics"];

/// What the server behind board `name` published (None: no such board
/// or nothing yet).
pub fn board(name: &'static str) -> Option<Metrics> {
    BOARDS.with(|v| {
        if v.is_empty() {
            for n in BOARD_NAMES {
                v.push((n, shm::board(n, false)));
            }
        }
        let b = v.iter().find(|(n, _)| *n == name)?.1.as_ref()?;
        let mut buf = Vec::new();
        b.read(0, &mut buf)?;
        touchos_proto::decode(&buf)
    })
}

fn kind(k: pm::Kind) -> Kind {
    match k {
        pm::Kind::Counter => Kind::Counter,
        pm::Kind::Gauge => Kind::Gauge,
    }
}

/// Add a sample of a metric from [`pm::FAMILIES`].
fn put(w: &mut prom::Writer, name: &str, labels: &[(&str, &str)], value: f64) {
    if let Some((k, help)) = pm::family(name) {
        w.sample(name, kind(k), help, labels, value);
    }
}

/// The whole text.
pub fn text() -> String {
    let mut w = prom::Writer::new();
    if let Some(bytes) = kernel_report()
        && let Some(r) = stats::Report::parse(&bytes)
    {
        let h = r.header;
        put(&mut w, "touchos_uptime_seconds", &[], h.uptime_us as f64 / 1e6);
        put(&mut w, "touchos_ticks_total", &[], h.ticks as f64);
        put(&mut w, "touchos_idle_seconds_total", &[], h.idle_us as f64 / 1e6);
        put(&mut w, "touchos_context_switches_total", &[], h.switches as f64);
        put(&mut w, "touchos_memory_free_bytes", &[], h.free_bytes as f64);
        put(&mut w, "touchos_kernel_heap_used_bytes", &[], h.heap_used as f64);
        put(&mut w, "touchos_kernel_heap_size_bytes", &[], h.heap_size as f64);
        put(&mut w, "touchos_threads", &[], h.threads as f64);
        for p in processes(&r) {
            let l = [("process", p.name.as_str())];
            put(&mut w, "touchos_process_cpu_seconds_total", &l, p.cpu_us as f64 / 1e6);
            put(&mut w, "touchos_process_switches_total", &l, p.switch_ins as f64);
            if p.pid != 0 {
                put(&mut w, "touchos_process_memory_bytes", &l, p.mem_bytes as f64);
            }
        }
        for i in r.irqs() {
            let line = alloc::format!("{}", i.line);
            put(&mut w, "touchos_irq_total", &[("irq", line.as_str())], i.count as f64);
        }
    }
    let arg = |k: &str| touchos_rt::arg(k).filter(|v| *v != "-").unwrap_or("");
    put(
        &mut w,
        "touchos_build_info",
        &[("version", env!("CARGO_PKG_VERSION")), ("build", arg("build")), ("slot", arg("slot"))],
        1.0,
    );
    for name in BOARD_NAMES {
        for m in board(name).map(|m| m.items).unwrap_or_default() {
            let labels: Vec<(&str, &str)> = m.labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            put(&mut w, &m.name, &labels, m.value as f64);
        }
    }
    w.finish()
}

pub fn handle(_req: &Request) -> Response {
    if !touchos_config::client::settings::get().metrics.enabled {
        return crate::error(404, "not found");
    }
    Response::new(200, prom::CONTENT_TYPE, text())
}
