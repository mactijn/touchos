//! Servers' counters for the `/metrics` endpoint and the System page: each
//! server that has some writes a [`Metrics`] board (init makes them, web
//! reads them). Every name is in [`FAMILIES`], with its type and help
//! text, so the boards carry only names, labels and values.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Bytes of a metrics board.
pub const BYTES: u64 = 4096;

/// Producers write their board at most this often.
pub const EVERY_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Only goes up (from the server's start; a restart starts it again).
    Counter,
    Gauge,
}

/// Every metric a board or web may publish: (name, type, help).
pub const FAMILIES: &[(&str, Kind, &str)] = &[
    // The kernel (web, from the STATS syscall).
    ("touchos_uptime_seconds", Kind::Gauge, "Time since boot."),
    ("touchos_ticks_total", Kind::Counter, "Timer ticks since boot."),
    ("touchos_idle_seconds_total", Kind::Counter, "Time the CPU waited for an interrupt."),
    (
        "touchos_context_switches_total",
        Kind::Counter,
        "Address-space switches (the heartbeat's switches/s); 32 bits in the kernel, so it wraps.",
    ),
    ("touchos_memory_free_bytes", Kind::Gauge, "Free RAM."),
    ("touchos_kernel_heap_used_bytes", Kind::Gauge, "Kernel heap in use."),
    ("touchos_kernel_heap_size_bytes", Kind::Gauge, "Kernel heap size."),
    ("touchos_threads", Kind::Gauge, "Threads, the kernel's own included."),
    ("touchos_process_cpu_seconds_total", Kind::Counter, "CPU time per thread (about 30 µs resolution)."),
    ("touchos_process_switches_total", Kind::Counter, "Times each thread was switched to."),
    (
        "touchos_process_memory_bytes",
        Kind::Gauge,
        "Memory a process's address space owns (image, stack, heap; not shared memory or devices).",
    ),
    ("touchos_irq_total", Kind::Counter, "Interrupts per controller line (drivers' lines; not the timer)."),
    ("touchos_build_info", Kind::Gauge, "The running build: version and copy (A/B)."),
    // init: the servers it supervises.
    ("touchos_server_up", Kind::Gauge, "1 while the server runs."),
    ("touchos_server_gave_up", Kind::Gauge, "1 if init gave up restarting the server."),
    ("touchos_server_restarts_total", Kind::Counter, "Restarts of the server since boot."),
    ("touchos_server_crashes_total", Kind::Counter, "Times the server crashed or failed since boot."),
    (
        "touchos_server_last_exit",
        Kind::Gauge,
        "The server's last end: how (exit, crash or killed) and the exit code or fault vector.",
    ),
    // net.
    ("touchos_net_receive_bytes_total", Kind::Counter, "Bytes received (Ethernet frames)."),
    ("touchos_net_receive_packets_total", Kind::Counter, "Frames received."),
    ("touchos_net_transmit_bytes_total", Kind::Counter, "Bytes sent (Ethernet frames)."),
    ("touchos_net_transmit_packets_total", Kind::Counter, "Frames sent."),
    ("touchos_net_transmit_dropped_total", Kind::Counter, "Frames dropped because the driver's ring was full."),
    ("touchos_net_tcp_connections", Kind::Gauge, "Open TCP connections of the other processes."),
    ("touchos_net_dhcp_leases_total", Kind::Counter, "DHCP leases taken (the first and renewals that changed it)."),
    ("touchos_net_link_changes_total", Kind::Counter, "Times the active network link changed."),
    ("touchos_net_up", Kind::Gauge, "1 while there is an IPv4 address."),
    // wifi.
    ("touchos_wifi_associated", Kind::Gauge, "1 while joined to an access point."),
    ("touchos_wifi_reconnects_total", Kind::Counter, "Connections lost and joined again."),
    ("touchos_wifi_rssi_dbm", Kind::Gauge, "Signal of the access point at join."),
    ("touchos_wifi_channel", Kind::Gauge, "The channel."),
    ("touchos_wifi_rate_kbps", Kind::Gauge, "The current transmit rate."),
    ("touchos_wifi_rx_frames_total", Kind::Counter, "Frames received from the chip."),
    ("touchos_wifi_rx_crc_errors_total", Kind::Counter, "Received frames with a bad CRC."),
    ("touchos_wifi_rx_malformed_total", Kind::Counter, "Received transfers that didn't parse."),
    ("touchos_wifi_usb_errors_total", Kind::Counter, "USB transfer errors, per direction."),
    ("touchos_wifi_tx_frames_total", Kind::Counter, "Frames handed to the chip."),
    ("touchos_wifi_tx_ok_total", Kind::Counter, "Frames the chip sent and had acknowledged."),
    ("touchos_wifi_tx_failed_total", Kind::Counter, "Frames the chip gave up on."),
    ("touchos_wifi_tx_retries_total", Kind::Counter, "Retransmissions."),
    // ha.
    ("touchos_ha_connected", Kind::Gauge, "1 while connected to Home Assistant."),
    ("touchos_ha_entities", Kind::Gauge, "Entities mirrored from Home Assistant."),
    ("touchos_ha_reconnects_total", Kind::Counter, "Connections to Home Assistant made after the first."),
    ("touchos_ha_events_total", Kind::Counter, "Messages received from Home Assistant."),
];

/// The type and help text of a metric; None if it isn't in [`FAMILIES`].
pub fn family(name: &str) -> Option<(Kind, &'static str)> {
    FAMILIES.iter().find(|f| f.0 == name).map(|f| (f.1, f.2))
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: i64,
}

/// What a server publishes on its board.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub items: Vec<Metric>,
}

impl Metrics {
    pub fn new() -> Metrics {
        Metrics::default()
    }

    /// Add a sample without labels. (Names not in [`FAMILIES`] are a bug;
    /// web leaves them out.)
    pub fn set(&mut self, name: &str, value: i64) -> &mut Metrics {
        self.with(name, &[], value)
    }

    pub fn with(&mut self, name: &str, labels: &[(&str, &str)], value: i64) -> &mut Metrics {
        debug_assert!(family(name).is_some(), "{name} isn't in FAMILIES");
        self.items.push(Metric {
            name: name.into(),
            labels: labels.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
            value,
        });
        self
    }

    /// The first sample of `name` whose labels include these.
    pub fn get(&self, name: &str, labels: &[(&str, &str)]) -> Option<i64> {
        self.items
            .iter()
            .find(|m| m.name == name && labels.iter().all(|(k, v)| m.labels.iter().any(|(a, b)| a == k && b == v)))
            .map(|m| m.value)
    }
}

/// A producer's side: rebuilds its metrics at most every [`EVERY_MS`] and
/// writes the board when they changed.
#[derive(Default)]
pub struct Publisher {
    last: Option<Metrics>,
    at_ms: Option<u64>,
}

impl Publisher {
    /// Time to build them again?
    pub fn due(&self, now_ms: u64) -> bool {
        self.at_ms.is_none_or(|t| now_ms >= t + EVERY_MS)
    }

    /// The bytes to write if `m` differs from what was written last.
    pub fn update(&mut self, m: Metrics, now_ms: u64) -> Option<Vec<u8>> {
        self.at_ms = Some(now_ms);
        if self.last.as_ref() == Some(&m) {
            return None;
        }
        let b = crate::encode(&m);
        self.last = Some(m);
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_are_well_formed() {
        for (i, (name, kind, help)) in FAMILIES.iter().enumerate() {
            assert!(name.starts_with("touchos_"), "{name}");
            assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{name}");
            assert_eq!(name.ends_with("_total"), *kind == Kind::Counter, "{name}: counters end in _total");
            assert!(!help.is_empty() && !help.contains('\n'), "{name}");
            assert!(FAMILIES[..i].iter().all(|f| f.0 != *name), "{name} twice");
        }
    }

    #[test]
    fn metrics_round_trip_and_fit_a_board() {
        let mut m = Metrics::new();
        m.set("touchos_ha_connected", 1).with("touchos_wifi_usb_errors_total", &[("direction", "rx")], 3);
        m.set("touchos_wifi_rssi_dbm", -61);
        let b = crate::encode(&m);
        assert_eq!(crate::decode::<Metrics>(&b), Some(m.clone()));
        assert_eq!(m.get("touchos_wifi_usb_errors_total", &[("direction", "rx")]), Some(3));
        assert_eq!(m.get("touchos_wifi_usb_errors_total", &[("direction", "tx")]), None);
        assert_eq!(m.get("touchos_wifi_rssi_dbm", &[]), Some(-61));

        // init's board, the largest: 16 servers with all their samples.
        let mut init = Metrics::new();
        for i in 0..16 {
            let s = alloc::format!("server{i:02}");
            let l = [("server", s.as_str())];
            init.with("touchos_server_up", &l, 1)
                .with("touchos_server_gave_up", &l, 0)
                .with("touchos_server_restarts_total", &l, 1000)
                .with("touchos_server_crashes_total", &l, 1000)
                .with("touchos_server_last_exit", &[("server", s.as_str()), ("how", "crash")], 13);
        }
        assert!((crate::encode(&init).len() as u64) < BYTES - 64, "{}", crate::encode(&init).len());
    }

    #[test]
    fn publisher_writes_changes_at_most_every_second() {
        let mut p = Publisher::default();
        assert!(p.due(0));
        let mut m = Metrics::new();
        m.set("touchos_ha_entities", 5);
        assert!(p.update(m.clone(), 0).is_some());
        assert!(!p.due(999));
        assert!(p.due(1000));
        assert!(p.update(m.clone(), 1000).is_none(), "unchanged: not written again");
        m.set("touchos_ha_connected", 1);
        assert!(p.update(m, 2000).is_some());
    }
}
