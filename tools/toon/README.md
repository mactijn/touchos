# Toon v1 tools (on the Raspberry Pi)

These tools give an Eneco Toon v1 (Prodrive ED2.0, i.MX27) a serial console, JTAG and GDB from a Raspberry Pi 4. `cargo xtask load`, `flash` and `nandboot` use them over SSH (`$TOON_HOST`). Setting them up is part of the [quick start](../../docs/quick-start-toon.md).

They live on the Pi in `~/touchos-toon/` (`rsync -a --exclude nandboot tools/toon/ pi:touchos-toon/`); the serial log and the NAND backup go to `~/toon-backup/`.

| File | Does |
|---|---|
| `toonconsole.py` | The one owner of the serial port (`/dev/ttyS0`, 115200). It logs everything with timestamps to `~/toon-backup/serial.log`, and sends what is written to the FIFO `console.in`. With `auto` in the file `autostop`, it types U-Boot's password at the prompt (stock R6/R8 or ToonRooter's RAM U-Boot, picked by the version line), so the Toon stays in U-Boot. While the file `guard` exists, `Starting kernel` makes it halt the CPU over JTAG, so the stock Linux never gets going; if the file holds a text, that text is the trigger instead. |
| `toon-cmd` | Sends one console line and prints the output up to the next prompt. |
| `pi4-jtag.cfg` | OpenOCD interface: Pi 4 GPIO bit-banging (`bcm2835gpio`), the pins below, 8 MHz. |
| `ed20.cfg` | OpenOCD board config (ToonRooter's, ported to OpenOCD 0.12; see THIRD_PARTY.md). `reset init` sets up clocks and DDR. |
| `toon-ocd` | Runs `;`-separated OpenOCD commands, prints their results, and **always resumes** the CPU afterwards (`-k` keeps it halted). |
| `nand-backup.py` | Backs up all 128 MiB of NAND, read-only and verified: U-Boot reads each 16 MiB chunk (minus its bad blocks) into RAM and CRCs it; JTAG dumps it; the CRCs must match. Builds a physical image with 0xFF for bad blocks. It resumes after an interruption. About 16 minutes. |
| `nand-verify.py` | Checks that the NAND still matches the backup, read-only: U-Boot reads and CRCs each chunk again. About 70 s, with U-Boot at its prompt. Once touchos is in the NAND, only U-Boot's first 3 MiB still match. |
| `nandboot/` | touchos's boot stage in NAND, and the flasher that writes it (bare metal, its own workspace; `cargo xtask flash` builds and loads it). |
| `fb2png.py` | Turns a raw 800×480 RGB565 framebuffer dump into a PNG (no dependencies). |
| `fbshot` | A screenshot of the dashboard in QEMU (`cargo xtask qemu`): it asks QEMU's monitor (`build/arm-imx25/monitor.sock`) for the framebuffer and converts it. On the Toon: `toon-ocd "halt; dump_image /tmp/fb.raw 0xc1400000 768000"` and `fb2png.py`. |

## Wiring

This is ToonRooter's table. Toon pin 1 is top-left, with the Ethernet port at the bottom left. The Toon's I/O is 3.3 V (Vt, pin 9), so the Pi connects directly.

| Toon pin | Signal | Pi GPIO | Pi header pin |
|---|---|---|---|
| 1 | RTCK | — | — |
| 2 | TRST | 8 | 24 |
| 3 / 5 / 14 | GND | GND | 25 / 20 / 9 |
| 4 | TCK | 11 | 23 |
| 6 | TMS | 25 | 22 |
| 7 | SRST | 24 | 18 |
| 8 | TDI | 10 | 19 |
| 9 | Vt (3.3 V) | — | — |
| 10 | TDO | 9 | 21 |
| 11 | RxD (Toon in) | 14 (TXD) | 8 |
| 13 | TxD (Toon out) | 15 (RXD) | 10 |

OpenOCD 0.12's `interface/raspberrypi-native.cfg` puts TMS on GPIO 8, which is TRST on this wiring, and ToonRooter's own `rpi4.cfg` uses commands that 0.12 removed: use `pi4-jtag.cfg`. `toon-ocd` sets the Pi's pull-ups on SRST and TRST before each run (GPIO 24 has a pull-down by default).

## Use

```sh
# The console service (once per Pi boot, ideally before the Toon gets power).
cd ~/touchos-toon && echo auto > autostop && touch guard
setsid nohup python3 toonconsole.py >console.err 2>&1 </dev/null &
./toon-cmd version                                # a U-Boot command; prints the reply

# JTAG
./toon-ocd 'halt; reg pc; mdw 0xa1400000 4'       # resumes afterwards
./toon-ocd -k 'reset init'                        # reset, clocks + DDR, stay halted
openocd -f pi4-jtag.cfg -f ed20.cfg               # a server for telnet :4444 and GDB :3333
```

For GDB on your PC, forward the port (`ssh -L 3333:localhost:3333 pi`) and use `gdb-multiarch` with the kernel ELF from `build/arm-toon/touchos`.

Notes:
- `reset halt` can only halt about 300 ms after reset (the i.MX27's SRST also resets TRST), by which time U-Boot runs from DDR. `reset init` then skips the clock and DDR set-up; doing it again over a running U-Boot can make JTAG fail during the PLL switch.
- U-Boot keeps the LCD framebuffer at `0xa1400000`; touchos takes the display over as U-Boot set it up. So `load` waits for the U-Boot prompt before it loads.
- Never `saveenv` in U-Boot: touchos leaves U-Boot, its environment and the splash (NAND 0x0-0x2fffff) untouched and boots through U-Boot's stock `bootcmd`.

## touchos in NAND

```sh
cargo xtask flash --slot A --config   # boot stage + copy A + the config; verified
cargo xtask flash --slot B            # copy B, made active; the saved settings stay
cargo xtask nandboot --secs 60        # autostop off, guard gone, reset: U-Boot boots touchos by itself
cargo xtask load                      # back to loading from RAM (sets autostop to auto)
```

The flasher runs from RAM (JTAG) with the payload at 0xa2000000 and writes only from 0x300000 on: U-Boot, its environment and the splash stay as they are, so U-Boot's own `boot_nand` loads the boot stage from the old `kernel` partition. The layout and formats are in `libs/nand`; [docs/hardware-toon.md](../../docs/hardware-toon.md) has the measurements. After `nandboot`, every reset (the watchdog's too) boots touchos again.

Updates without JTAG go through the settings page (`cargo xtask bundle`); [docs/architecture.md](../../docs/architecture.md#two-copies-of-the-system) explains the trial and the fallback.

## Testing the EHCI driver in QEMU

QEMU's imx25 has the same ChipIdea EHCI (USB2 at `0x53FF4400`, `usb-bus.1`), and init gives it to the USB server there. That server only enumerates: a high-speed device added with `QEMU_EXTRA="-device ...,bus=usb-bus.1"` shows up as a `port 1: vendor:product ...` line (QEMU's EHCI enables only high-speed devices).
