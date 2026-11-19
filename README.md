# touchos

A small operating system, written from scratch in Rust, that turns an **Eneco Toon v1** thermostat into a Home Assistant wall panel. There is no Linux underneath: a microkernel with its drivers and services in user-space processes. touchos draws a native touch dashboard for your Home Assistant rooms and entities, and talks to Home Assistant's WebSocket API over TLS.

**Start here: [Quick start on the Toon](docs/quick-start-toon.md).**

> touchos replaces the Toon's own firmware (U-Boot stays). It is a hobby project, not affiliated with Eneco, Quby, Prodrive or Home Assistant.

## What works on the Toon

- **The dashboard:** your Home Assistant areas as rooms, a tile per entity (lights with dimming, switches, covers, fans, locks, media, scenes, thermostats, sensors), a clock and status line. Touch to toggle, drag to dim, swipe for pages.
- **Networking:** Wi-Fi (the built-in RT5370, WPA2-PSK) or Ethernet; DHCP, DNS, NTP; TLS 1.2 and 1.3 (rustls) to Home Assistant, with the certificate checked against Mozilla's root store.
- **A settings page** in the browser (pair with a code shown on the panel): every setting, tile order, the log, restart.
- **Signed updates** from the settings page into the copy of the system that isn't running, with an automatic return to the old copy if the new one fails.
- **Robustness:** every driver in its own process, restarted when it crashes; the hardware watchdog restarts a hung system; settings saved crash-safely in NAND.
- Backlight dimming and night hours, the RTC and temperature sensors, USB disks on the external port (read-only).
- Groundwork for the boiler link (OpenTherm, receive only) and the Z-Wave chip.

[ROADMAP.md](ROADMAP.md) lists what's planned. Audio, for example, isn't there yet.

## How it's built

The kernel (`kernel/`, about 6,000 lines) does memory and address spaces, a preemptive scheduler, IPC (calls, notifications, shared memory), capabilities, device access, time and the watchdog. Everything else is a process (`servers/`) that `init` starts with exactly the capabilities it needs:

| Process | Does |
|---|---|
| `init` | Starts the others, restarts crashed ones, pets the watchdog, judges a new copy on trial |
| `config` | Owns the settings; serves them and saves checked changes |
| `storage` | The NAND: the settings and the two copies of the system |
| `platform` | I²C: the touch controller, the RTC, the temperature sensors |
| `usb` | The external USB port (EHCI): USB disks (read-only) |
| `wifi` | The RT5370 Wi-Fi chip and the WPA2 station |
| `fec` | The Ethernet controller |
| `net` | TCP/IP (smoltcp): DHCP, DNS, NTP, TCP for the others |
| `ha` | The Home Assistant client |
| `ui` | The dashboard |
| `web` | The settings page |
| `update` | Installs signed updates |
| `boiler`, `zwave` | The boiler link and the Z-Wave controller (early) |

Most of the logic lives in libraries (`libs/`) that are tested on the development machine. [docs/architecture.md](docs/architecture.md) has the details.

## Building

You need [rustup](https://rustup.rs) (the toolchain comes from `rust-toolchain.toml`) and Node.js 24+ for the settings page.

```sh
cargo xtask build          # the Toon's kernel and initrd, in build/arm-toon/
cargo xtask qemu           # run it in QEMU's imx25-pdk (no hardware needed)
cargo xtask check          # all host tests, both builds, the settings page's tests
cargo xtask selftest       # the on-target tests, in QEMU
cargo xtask                # every command
```

Loading it onto a Toon needs a Raspberry Pi wired to its JTAG header once: see the [quick start](docs/quick-start-toon.md).

## Documentation

- [Quick start on the Toon](docs/quick-start-toon.md): from a stock Toon to a working panel
- [Settings and using the panel](docs/settings.md)
- [Metrics](docs/metrics.md): `/metrics` for Prometheus
- [Development](docs/development.md): the `xtask` commands, tests, debugging, the settings page's code
- [Architecture](docs/architecture.md): the kernel, the processes, where code goes
- [The Toon's hardware](docs/hardware-toon.md), as touchos drives it
- [The Pi-side tools](tools/toon/README.md): console, JTAG, NAND backup

## License

MIT, see [LICENSE](LICENSE). Third-party code and assets: [THIRD_PARTY.md](THIRD_PARTY.md).
