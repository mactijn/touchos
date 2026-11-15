# Development

## The `xtask` commands

Everything goes through `cargo xtask <command>`. `cargo xtask` alone prints the list, and a misspelt flag is an error.

```sh
cargo xtask build [--board toon|imx25]   # kernel, initrd, config, boot info in build/arm-<board>/ (default: toon)
cargo xtask qemu [--secs N]              # boot in QEMU -M imx25-pdk: the console on stdout, the settings page on localhost:8080
cargo xtask load [--secs N]              # load into the Toon's RAM over JTAG ($TOON_HOST) and show its console
cargo xtask flash [--slot A|B] [--config]  # write touchos into the Toon's NAND; --config also writes config/touchos.toml
cargo xtask nandboot [--secs N]          # let the Toon boot from its NAND by itself, and show its console
cargo xtask bundle                       # build/touchos-<build>.update, a signed update for the settings page
cargo xtask keygen                       # once: the signing key for updates (~/.config/touchos/update-key)
cargo xtask check [--quick]              # host tests, both boards' builds, the settings page's unit tests
cargo xtask selftest [--secs N]          # boot QEMU imx25 with `selftest`; fails on a FAIL line or a kernel panic
cargo xtask log [PORT]                   # print the kernel log sent over UDP (debug.log_host; port 5555)
cargo xtask pair [URL]                   # pair without the screen (PIN from the UDP log; needs debug.log_pin)
cargo xtask webui [serve [URL]]          # build the settings page; serve: live reload on localhost:4200
```

`build`, `qemu`, `load`, `flash` and `bundle` also take `--release` and `--cmdline "ARGS"` (extra kernel arguments, below).

- **`check`** is the one command for "did I break anything that can be checked without hardware". `--quick` runs the host tests and `cargo check` of both boards (about 20 s).
- **`selftest`** boots QEMU with the `selftest` argument. init then runs the test programs (`servers/hello`, `servers/selftest` and its own checks: IPC, memory, capabilities, crashes and restarts, the config server) and the run passes when they finish with PASS lines and no FAIL line or panic. The log is in `build/selftest-imx25.log`.
- **`load`** is the quick loop on real hardware: it loads into RAM over JTAG, so nothing is written to NAND. It sets the console service back to stopping U-Boot (`autostop`), so end a session with `nandboot`, or the next power-on stops at the U-Boot prompt.

## What a build reads besides the source

| File or variable | Used for |
|---|---|
| `config/touchos.toml` (gitignored) | The Toon's settings (`build`, `load`, `flash --config`); without it, `config/touchos.example.toml` |
| `config/qemu.toml`, or `config/qemu.local.toml` (gitignored) | QEMU's settings (`qemu`, `selftest`, `build --board imx25`) |
| `$TOUCHOS_CONFIG` | A settings file for any build, ahead of the others |
| `config/update-key.pub` | The public key a build accepts updates from; without it, updates are refused |
| `~/.config/touchos/update-key`, or `$TOUCHOS_UPDATE_KEY` | The signing key (`keygen` makes it, `bundle` uses it) |
| `build/firmware/rt3071.bin` | The RT5370's firmware, put into the Toon's initrd (see the quick start) |
| `npm` (Node.js 24 or newer) | Builds the settings page; without it, `build/webui/` is reused, or a placeholder page is served |
| `$TOON_HOST` | The Raspberry Pi wired to the Toon (`user@host`, with `tools/toon` in `~/touchos-toon`) |
| `$TOUCHOS_PANEL` | A panel for `webui serve` |
| `$QEMU_EXTRA` | Extra QEMU arguments for `qemu` (e.g. `-device tmp105,bus=i2c-bus.0,address=0x48`) |

## The log

The kernel log goes to the serial console (on the Pi: `~/toon-backup/serial.log`). With `debug.log_host = "<your PC>:5555"` it also goes over UDP: `cargo xtask log` prints it.

In QEMU (`config/qemu.toml`) the log goes to the host (10.0.2.2) already, and `log_pin` is on: `cargo xtask pair` pairs with the settings page at `localhost:8080` without looking at a screen. It writes the session to `build/panel.cookies`, for `curl -b build/panel.cookies -H 'X-Touchos: 1' ...`. It needs UDP port 5555, so stop `cargo xtask log` first. Leave `log_pin` off on a panel in real use: the log travels over the network unencrypted.

## Kernel arguments for testing

With `--cmdline "..."` (on `build`, `qemu`, `load`, `flash` or `bundle`):

| Argument | Effect |
|---|---|
| `selftest` | Run the on-target tests (settings are not saved: `storage=ro`) |
| `storage=ro` / `off` | Changes apply until the next restart / storage isn't used |
| `noui` | Keep the log console on screen instead of the dashboard |
| `watchdog=off` | Don't arm the watchdog (debugging with breakpoints) |
| `panic=halt` | Stay on a kernel panic's message instead of restarting |
| `autoreboot=N` | Restart after N seconds |
| `test_fail=panic` | The kernel panics after 20 s |
| `test_fail=hang` | The kernel hangs with interrupts off after 20 s |
| `test_fail=kstack` | A kernel stack overflow after 20 s (must be reported, not corrupt memory) |
| `test_fail=init` | init stops petting the watchdog after 20 s |
| `test_fail=crash:NAME` | init makes program NAME (e.g. `ui`) crash after 20 s, and 2 s after each restart |
| `test_fail=nonet` | init ignores the network address, so a copy on trial never counts as healthy |
| `test_fail=usb` | The first USB driver fails once, 40 s after it started |
| `zwave=pair` / `reset-pair` / `include` | The Z-Wave server pairs a device (see `servers/zwave`) |

A test boot with `test_fail=` is never made the active copy. To test the trial on hardware: `cargo xtask bundle --cmdline "test_fail=crash:ui"`, install it from the settings page, and watch the Toon go back to the old copy at the first health check.

**Recovery.** The kernel arms the i.MX watchdog at boot (60 s). init shows it a sign of life every 10 s, but only while no critical program (config, storage, net, wifi, ui, platform, web) has crashed so often that init gave up on it. A kernel panic restarts after 10 s (30 s on a test boot).

## The settings page's code

- **Where.** `webui/` is an Angular app with Angular Material (standalone components, signals, no zone.js). `cargo xtask build` builds it into `build/webui/` when anything in `webui/` changed, and `servers/web/build.rs` embeds those files, each also gzip-compressed. [webui/README.md](../webui/README.md) has the rules the panel imposes.
- **Requests.** The panel answers one request per connection, so the app is one script (about 1 MB, 200 KB gzipped) with no lazy-loaded parts. Icons are SVGs inside it, and the font is the browser's own: the page needs nothing from the internet.
- **Inline styles need the nonce.** Angular Material adds its styles as `<style>` elements. The panel's Content-Security-Policy allows them only with a random nonce, made for each load of `index.html` and handed to Angular through `ngCspNonce`. Inline scripts stay forbidden.
- **Developing.** `cargo xtask webui serve http://<panel-ip>` (or the default, QEMU at `localhost:8080`) serves the app with live reload on `localhost:4200`. The proxy presents itself to the panel as the panel's own page, because the panel accepts only its own Host and Origin.

## Screenshots

`tools/toon/fbshot` takes a screenshot of the dashboard in QEMU (`cargo xtask qemu` running). On the Toon: `toon-ocd "halt; dump_image /tmp/fb.raw 0xc1400000 768000"` on the Pi, then `tools/toon/fb2png.py`.
