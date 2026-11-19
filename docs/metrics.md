# Metrics (Prometheus)

With `metrics.enabled = true` (settings page: Metrics), the panel serves `http://<panel>/metrics` in the Prometheus text format, so Prometheus, VictoriaMetrics or Grafana Agent on your network can scrape it. Off (the default), `/metrics` answers 404.

The endpoint needs no session: anyone on the network can read the figures below, but nothing can be changed through it.

## A scrape config

```yaml
scrape_configs:
  - job_name: touchos
    scrape_interval: 15s
    static_configs:
      - targets: ["touchos-221a2b.local"]   # or the panel's IP address
```

The servers write their counters at most once a second, so scraping more often than that adds nothing.

## What's in it

All names start with `touchos_`. Counters (`*_total`) count from boot, or from the server's start for the servers' own counters; a restart starts them at 0 again, which Prometheus's `rate()` handles.

| Metric | From | What |
|---|---|---|
| `uptime_seconds`, `ticks_total` | kernel | Time since boot, timer ticks |
| `idle_seconds_total` | kernel | Time the CPU waited for an interrupt. `1 - rate(idle)` is the CPU load |
| `context_switches_total` | kernel | Address-space switches: the heartbeat's "switches/s" |
| `memory_free_bytes`, `kernel_heap_used_bytes`, `kernel_heap_size_bytes` | kernel | Free RAM, and the kernel's heap |
| `threads` | kernel | Threads, the kernel's own included |
| `process_cpu_seconds_total{process}` | kernel | CPU time per thread (one thread per process; `idle` is the idle thread, `kernel` the kernel's heartbeat) |
| `process_switches_total{process}` | kernel | Times each was switched to |
| `process_memory_bytes{process}` | kernel | What its address space owns: image, stack and heap. Shared memory (the Home Assistant state, rings, DMA buffers) and devices aren't counted, and memory is never given back, so it only grows |
| `irq_total{irq}` | kernel | Interrupts per controller line that fired, for the drivers' lines (the timer isn't counted) |
| `build_info{version,build,slot}` | web | Always 1 |
| `server_up`, `server_gave_up`, `server_restarts_total`, `server_crashes_total` `{server}` | init | Each program init started; `gave_up`: crashed too often, not restarted any more |
| `server_last_exit{server,how}` | init | How it ended last: `exit` (the value is the exit code), `crash` (the CPU exception) or `killed` |
| `net_receive_bytes_total`, `net_receive_packets_total`, `net_transmit_bytes_total`, `net_transmit_packets_total`, `net_transmit_dropped_total` `{interface}` | net | Ethernet frames per interface (`wired`, `wlan`) |
| `net_up`, `net_tcp_connections`, `net_dhcp_leases_total`, `net_link_changes_total` | net | |
| `wifi_associated`, `wifi_reconnects_total`, `wifi_rssi_dbm`, `wifi_channel`, `wifi_rate_kbps` | wifi | The connection (the signal is the access point's at join) |
| `wifi_rx_frames_total`, `wifi_rx_crc_errors_total`, `wifi_rx_malformed_total`, `wifi_usb_errors_total{direction}` | wifi | The RT5370's receive side and USB |
| `wifi_tx_frames_total`, `wifi_tx_ok_total`, `wifi_tx_failed_total`, `wifi_tx_retries_total` | wifi | Frames handed to the chip, and the chip's results: acknowledged, given up, retries |
| `ha_connected`, `ha_entities`, `ha_reconnects_total`, `ha_events_total` | ha | The Home Assistant connection |

CPU time is measured at each switch with the clock's resolution (about 30 µs), so a thread that runs for less than that at a time is under- or overcounted, but over all threads it adds up: the `rate()` of all `process_cpu_seconds_total` together (`idle` included) is 1. A process that ended takes its share with it.

## Where it comes from

- **The kernel:** the `STATS` syscall (`touchos_abi::stats`), which needs the `STATS` right on the `system` capability. Only web has it.
- **The servers:** each writes a metrics board (`touchos_proto::metrics`): `init_metrics`, `net_metrics`, `wifi_metrics` and `ha_metrics`, made by init and read by web. Every metric's type and help text are listed in `touchos_proto::metrics::FAMILIES`; a new metric goes there first.
