# Quick start: touchos on an Eneco Toon v1

This guide takes a stock Toon v1 to a Home Assistant panel running touchos. That takes about an hour, most of it the NAND backup.

You load touchos once over JTAG from a Raspberry Pi. After that the Toon boots touchos by itself, and updates go through its settings page.

> **This replaces the Toon's own firmware.** touchos is written into the NAND from offset 0x300000 on, over the stock Linux kernel and root file system. U-Boot, its settings and the boot splash stay as they are. **There is no automated way back to the stock firmware yet.** Step 5 makes a full, verified backup, so the old contents aren't lost, but restoring them is up to you.

## What you need

- **An Eneco Toon v1**, the first model (Prodrive ED2.0 board, Freescale i.MX27). The Toon 2 is different hardware and won't work. Power it the way it is normally powered, through its boiler module.
- **A Raspberry Pi 4** with Raspberry Pi OS (trixie or bookworm; Lite is enough), reachable over SSH with a key. It drives the Toon's JTAG and serial console.
- **11 jumper wires** (female to female) from the Pi's header to the Toon's 14-pin JTAG header. [ToonRooter](https://github.com/martenjacobs/ToonRooter) shows where the header is.
- **A Linux PC** for building, with:
  - [rustup](https://rustup.rs) (the right nightly toolchain installs itself from `rust-toolchain.toml`) and `git`;
  - **Node.js 24 or newer** (`npm`), which builds the settings page (without it, the panel serves a placeholder page and you can't pair or update);
  - optionally `qemu-system-arm`, to try touchos without hardware first.
- **A Home Assistant** reachable by IP address or DNS name (touchos answers to its own `.local` name, but can't look up others), with a long-lived access token (your profile → Security → Long-lived access tokens).
- **Wi-Fi with WPA2-PSK (AES)**, or an Ethernet cable to the Toon.

## 1. Build

```sh
git clone https://github.com/<you>/touchos && cd touchos
cargo xtask build              # the kernel, the servers and the settings page, for the Toon
```

The first build fetches the toolchain and takes a few minutes. The result is in `build/arm-toon/`.

## 2. Try it in QEMU (optional)

```sh
cargo xtask qemu --secs 60     # QEMU imx25-pdk: the boot log on your terminal
```

QEMU has no screen here; the log shows the system starting. While it runs, the settings page is at http://localhost:8080/ (the pairing code is in the log). `cargo xtask selftest` runs the on-target tests the same way.

## 3. Prepare the Pi

1. Turn on the serial port and turn off the serial login. In `sudo raspi-config` → Interface Options → Serial Port, answer *no* to the login shell and *yes* to the hardware, then reboot. This puts `enable_uart=1` in `/boot/firmware/config.txt` and keeps the serial port out of `/boot/firmware/cmdline.txt`.
2. Install the tools and give your user the serial port and GPIO pins:
   ```sh
   sudo apt install openocd python3-serial
   sudo usermod -aG dialout,gpio $USER      # then log in again (the first user is in both already)
   ```
   Raspberry Pi OS's own `openocd` build (0.12) has the `bcm2835gpio` driver that `pi4-jtag.cfg` uses.
3. Copy the Toon tools to the Pi, from your touchos checkout:
   ```sh
   ssh pi@raspberrypi.local 'mkdir -p touchos-toon toon-backup'
   rsync -a --exclude nandboot tools/toon/ pi@raspberrypi.local:touchos-toon/
   ```
   They live in `~/touchos-toon`; the serial log and the backup go to `~/toon-backup`. See [tools/toon/README.md](../tools/toon/README.md) for what each tool does.

## 4. Wire the Pi to the Toon

With both switched off. Toon pin 1 is top left, with the Ethernet port at the bottom left. The Toon's I/O is 3.3 V, so the Pi connects directly.

| Toon pin | Signal | Pi GPIO | Pi header pin |
|---|---|---|---|
| 2 | TRST | 8 | 24 |
| 3, 5, 14 | GND | GND | 25, 20, 9 |
| 4 | TCK | 11 | 23 |
| 6 | TMS | 25 | 22 |
| 7 | SRST | 24 | 18 |
| 8 | TDI | 10 | 19 |
| 10 | TDO | 9 | 21 |
| 11 | RxD (into the Toon) | 14 (TXD) | 8 |
| 13 | TxD (out of the Toon) | 15 (RXD) | 10 |

Pins 1 (RTCK) and 9 (Vt, 3.3 V) stay unconnected.

## 5. Start the console and back up the NAND

On the Pi, start the console service **before** the Toon gets power. It logs the Toon's console and types U-Boot's password, so the Toon stops at the U-Boot prompt instead of starting the stock system:

```sh
cd ~/touchos-toon && echo auto > autostop && touch guard
setsid nohup python3 toonconsole.py >console.err 2>&1 </dev/null &
```

Now power the Toon. (If it is on already, `./toon-ocd reset` restarts it, and it stops at the U-Boot prompt the same way.) Check that it answers:

```sh
tail ~/toon-backup/serial.log    # U-Boot's banner, then "(autostop: password sent)"
./toon-cmd version               # a U-Boot command: prints U-Boot's version
./toon-ocd 'halt; reg pc'        # JTAG: halts the CPU, shows where it was, resumes it
```

Then back up all 128 MiB of NAND. It reads only; each 16 MiB chunk is checked against U-Boot's own CRC. It takes about 16 minutes and can be resumed if it's interrupted:

```sh
python3 nand-backup.py           # into ~/toon-backup/nand
```

Copy the backup to your PC as well (`rsync -a pi@raspberrypi.local:toon-backup/nand ./toon-nand-backup`). **Keep it private:** it holds the previous owner's data, such as Wi-Fi passwords and account tokens.

## 6. Configure the build

On your PC, in the touchos checkout:

1. **Wi-Fi firmware.** The Toon's RT5370 Wi-Fi chip needs `rt3071.bin` from linux-firmware (4 KiB; it can't be in this repository). Debian and Ubuntu ship it compressed:
   ```sh
   mkdir -p build/firmware
   zstd -d /lib/firmware/rt3071.bin.zst -o build/firmware/rt3071.bin   # or: cp /lib/firmware/rt3071.bin build/firmware/
   ```
   (`sudo apt install firmware-misc-nonfree` if it's missing.) Without it the build still works, with Ethernet only.
2. **Settings.** Copy the example and fill in your Home Assistant host, its token, and `[wifi]`:
   ```sh
   cp config/touchos.example.toml config/touchos.toml
   ```
   `config/touchos.toml` is gitignored (it holds your token). [settings.md](settings.md) lists every setting. You can change them all later on the settings page.
3. **The update key.** Once:
   ```sh
   cargo xtask keygen
   ```
   This writes a signing key to `~/.config/touchos/update-key` and its public half to `config/update-key.pub`. A build accepts only updates signed with the key whose public half it carries. So run this before the first flash, and keep the secret key safe.

## 7. Flash touchos into the NAND

Tell xtask where the Pi is, then flash. Flashing builds touchos, loads it into the Toon's RAM over JTAG, and writes the boot stage, the system and your settings into the NAND. Every block is read back and compared:

```sh
export TOON_HOST=pi@raspberrypi.local
cargo xtask flash --slot A --config      # about a minute; ends with "flash done"
cargo xtask nandboot --secs 90           # restart; U-Boot now boots touchos by itself
```

`nandboot` stops the console service from interrupting U-Boot, resets the Toon, and shows its console. You should see:

```
nandboot: copy A, the active copy (start 1)
...
[I] net: DHCP: 192.168.1.42/24 ...
[I] web: settings page at http://192.168.1.42/
[I] ha: connected, ... entities
```

From a restart it takes about 11 s to the kernel, and Home Assistant connects about 10 s later. The dashboard shows your Home Assistant areas and their entities. Tap a tile to toggle it; [settings.md](settings.md#using-the-panel) has the gestures.

From now on the Toon starts touchos at every power-on, and the watchdog restarts it if it hangs. You can disconnect the Pi.

## 8. The settings page

Open `http://touchos-xxxx.local/` in a browser on the same network: `xxxx` is the end of the Wi-Fi MAC address, and the log has the name and the IP address (`web: settings page at ...`). If `.local` names don't work on your computer, use the IP address (`http://<panel-ip>/`). Click "Show a code on the panel" and type the 6-digit code from the Toon's screen. You can then change every setting, choose and order tiles, see the log, and install updates.

The page is plain HTTP: anyone on your network can read what you send it.

## 9. Updates

Make a signed update on your PC, then install it from the settings page:

```sh
cargo xtask bundle            # build/touchos-<build>.update
```

On the settings page: Software → choose the file → Install. The panel checks the signature and every file, writes the update into the copy of the system that isn't running (A or B), and restarts into it **on trial**. If the new copy runs for a minute without a program failing and with a network address, it becomes the active copy. If it fails, or doesn't start three times, the Toon goes back to the old copy. Your settings stay as they are.

With the Pi still connected, `cargo xtask flash --slot B` (without `--config`, which would replace the saved settings) does the same over JTAG.

## Troubleshooting

- **No U-Boot banner in `serial.log`:** check pins 11 and 13 and GND, that the serial login is off, and that `toonconsole.py` is running (`pgrep -af toonconsole`; its errors are in `~/touchos-toon/console.err`).
- **"no U-Boot prompt after the reset":** the console service didn't stop U-Boot. Check that `~/touchos-toon/autostop` holds `auto`. A U-Boot version `toonconsole.py` doesn't know the password of boots on instead; add its password there.
- **JTAG errors from `toon-ocd`:** check the JTAG wires and GND. `pi4-jtag.cfg` runs at 8 MHz; lower `TOON_JTAG_KHZ` there for long wires.
- **The screen stays on the boot splash after a power cycle:** the console service still stops U-Boot (`autostop` is `auto` after `cargo xtask load`). Run `cargo xtask nandboot`, or stop `toonconsole.py`.
- **"rt2800: no firmware":** `build/firmware/rt3071.bin` was missing when you built. Add it and update.
- **No Home Assistant:** use its IP address, not a `.local` name, and check the token. `[homeassistant] tls = true` for an HTTPS-only setup on another port than 443. The log says why (`ha:` lines). On the panel, use `cargo xtask log` with `debug.log_host` set to your PC, or the Pi's `~/toon-backup/serial.log`.
- **A new copy that doesn't come up** goes back to the old one by itself (above). The log of the failed start is in the Pi's `serial.log` if it was connected.

## Going further

- [development.md](development.md): every `cargo xtask` command, the tests, the log over UDP, working on the settings page.
- [architecture.md](architecture.md): the kernel and the processes.
- [hardware-toon.md](hardware-toon.md): the Toon's hardware, as touchos drives it.
