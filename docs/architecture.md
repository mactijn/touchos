# Architecture

touchos is a small microkernel. Everything else runs in user-space processes, each in its own address space, and a process can use only what its capability table holds.

## The kernel

The kernel (`kernel/`) does only this:

- memory and address spaces, and loading programs (static ELF, checked by `libs/elf`);
- a preemptive scheduler: strict priorities, round-robin within one, a 250 Hz tick. It is written for several cores; the ARM926 has one. Address-space switches skip the cache flush through the ARM926's FCSE (fast context switch extension);
- IPC: synchronous call/reply, notifications, shared memory (with waiting on it);
- capabilities, and handing them on (`CAP_MINT` narrows the rights);
- device registers and interrupt lines, handed out by capability;
- time, the log console, the watchdog, and a random-number pool (fed by clock jitter and the i.MX27's SAHARA RNG);
- on the Toon, the pin and clock set-up that must happen before any driver runs: a block whose clock is gated hangs the bus.

`kernel/src/arch/arm` is the CPU (ARMv5TE: traps, context switch, paging with VIVT caches). `kernel/src/board/imx` is the board (boot contract, interrupt controller, timer, UART, watchdog, RNG, and the Toon's or QEMU imx25's set-up). The `board-toon` or `board-imx25` cargo feature picks one.

The kernel is started by touchos's boot stage in NAND (`tools/toon/nandboot`, started by the Toon's U-Boot), by QEMU, or over JTAG. All of them put the kernel, the initrd, the config and a boot-info block in RAM at fixed offsets (`libs/abi`'s `imx_boot`).

## The processes

`init` is the first process. It starts the others from the initrd (`servers/`) with exactly the capabilities each needs, from a board record in `servers/init/src/board.rs`.

| Process | Does |
|---|---|
| `init` | Starts the others; restarts crashed ones (backoff 1, 2, 4, 8 s; gives up after 5 crashes in a minute); pets the watchdog while the critical programs run; judges a copy of the system on trial; handles `autoreboot=` |
| `config` | The one owner of the settings: serves them (secrets only to those that need them), notifies changes, applies checked changes from the settings page, saves them through `storage` |
| `storage` | The NAND (libs/nand): `touchos.toml` in a ring of config records, the two copies of the system, the state record that says which copy is active or on trial |
| `platform` | The I²C controllers and their devices: the touch controller (TSC2007), the RTC (sets the clock), the temperature sensors |
| `usb` | The external port's EHCI: enumeration, USB disks (mass storage, read-only) |
| `wifi` | The RT5370 on the internal EHCI (`libs/rt2800`), and the chip-independent WPA2 station (`libs/wlan`) |
| `fec` | The i.MX Ethernet controller |
| `net` | smoltcp: DHCP, DNS, TCP for the other processes (data through shared memory), NTP, the UDP log |
| `ha` | The Home Assistant client (WebSocket over TLS) |
| `ui` | The dashboard: owns the framebuffer and the backlight |
| `web` | The settings page (HTTP) |
| `update` | Installs signed updates into the copy that isn't running |
| `boiler` | The boiler link (UART2, OpenTherm), receive only |
| `zwave` | The Z-Wave controller (UART3, Serial API) |
| `hello`, `selftest` | Tests, run with the `selftest` kernel argument |

Data moves between processes in shared-memory rings and boards: input events, Ethernet frames, Home Assistant state, network status.

The Toon has no IOMMU. A driver that does DMA (`usb`, `wifi`, `fec`) can still reach all memory, so isolation protects against crashes, not against a malicious driver. `storage` uses no DMA.

## Two copies of the system

The NAND holds two copies of the system, A and B, and one config ring that both share (layout in `libs/nand`, measurements in [hardware-toon.md](hardware-toon.md)):

- **The boot stage** reads the state record: which copy is active, and which (if any) is on trial. It loads that copy and the newest config, and adds `slot=A|B` to the kernel's command line.
- **An update** (`update` through `storage`) writes the copy that isn't running and puts it on trial. A copy on trial gets three starts.
- **init judges it** (`libs/update`'s `trial`): healthy after a minute with no program failure and a network address. Then it becomes the active copy. At its first failure, or if it isn't healthy within five minutes, the trial ends and the active copy starts next.
- **The active copy** that starts four times in a row without being confirmed (a hang or crash loop that the watchdog ends) hands over to the other copy, if that one has a valid header.

## Libraries

Libraries (`libs/`) hold the logic. Most of it is tested on the development machine: `cargo xtask check` runs the tests of every crate in `HOST_TESTS` (`xtask/src/check.rs`).

- `rt`: the user runtime (start-up, syscalls, the async executor, shared memory);
- `abi`: the syscall interface, and the i.MX boot contract;
- `proto`: the messages between processes, rings and boards;
- `drv`: what user-space drivers need: DMA memory, register windows (`mmio`, with the clock-first `poll_until`), the link rings;
- `usb`: the EHCI driver, enumeration, descriptors;
- `wlan`: the chip-independent Wi-Fi station (802.11 frames, the WPA2 handshake, CCMP, rate control);
- `rt2800`: the RT5370 Wi-Fi chip;
- `nand`: the i.MX27 NAND controller, the layout, the config ring and system copies;
- `blockdev`, `gpt`: block devices and partition tables (USB disks);
- `storage`: the storage server's rules (the mode, who may read what);
- `config`: the settings' schema, validation, patches, and the client;
- `http`, `webauth`, `netc`, `hass`: HTTP requests and WebSocket frames, pairing and sessions, the net client (with TLS: rustls, its RustCrypto provider, and rustls-webpki for the certificates), the Home Assistant messages;
- `certs`: the root certificates TLS trusts (`cacerts.der`, from the distro's CA bundle by `cargo xtask cacerts`). The file is in the initrd, and init shares one read-only copy (the "cacerts" capability) with each program that speaks TLS;
- `update`: signed update bundles, and the rule a copy on trial is judged by;
- `opentherm`, `zwave`: the boiler link's frames, the Z-Wave Serial API;
- `elf`, `tar`, `civil`, `atomics`: program files, the initrd, calendar time, atomic operations for ARMv5 (which has none).

## Where hardware code goes

- **Where a driver lives.** A driver goes in `libs/` when it implements a library's interface (`BlockDev`, `HostController`, `Radio`) or more than one process uses it. A driver with nothing to plug into stays in the one server that uses it: `fec`, the I²C devices in `platform`, the backlight in `ui`.
- **How hardware is chosen.** By the board init gets from the kernel (`board=`) and the capabilities it hands out: `platform` takes `board=` from init, and `usb`, `wifi` and `storage` drive the controllers whose capabilities they hold.
