# Eneco Toon v1 (Prodrive ED2.0): hardware notes

These notes describe the Toon v1 main board and what touchos does with it.
The values were read with JTAG and the serial console from a Raspberry Pi 4 (`tools/toon/`), under U-Boot, under touchos, and under the stock Linux (booted read-only with `init=/bin/sh`). The stock rootfs and kernel were also analysed offline from a NAND backup.
The main board runs on its own, without the LCD panel, powered through a Ketelmodule; everything below except the display and touch works that way.
Where this differs from https://toon.deobfuscated.org/hardware/toonv1/, it is noted below.

## Platform
| | |
|---|---|
| SoC | Freescale i.MX27 (MCIMX27L), **ARM926EJ-S r4**, MIDR `0x41069264`, 400.168 MHz, ARMv5TEJ, no FPU. VIVT caches |
| RAM | 128 MiB DDR at `0xa0000000`–`0xa7ffffff` |
| NAND | 128 MiB SLC, 128 KiB erase blocks, 2 KiB pages (512 B sub-pages), hardware ECC through the NFC (`mxc_nand`, registers at `0xd8000000`) |
| Board | "Prodrive B.V. ED2.0", U-Boot machine number `0xBF8` (3064), ATAGs at `0xa0000100` |
| Console | UART1 (`ttymxc0`, `0x1000a000`, IRQ 20), 115200 8N1, on the JTAG header pins 11 (RxD) and 13 (TxD) |
| JTAG | 14-pin header, **3.3 V** (Vt, pin 9, measured). TAPs: `imx27.etb 0x1b900f0f`, `imx27.cpu 0x07926121`. No RTCK in use. SRST pulls TRST |

- **The CPU is ARMv5TE:** there is no LDREX/STREX, so atomics need another approach (`libs/atomics`). The VIVT caches must be cleaned and flushed on an address-space switch unless the address spaces are kept apart with the FCSE, which is what touchos does.

## Boot
- **The boot ROM** loads U-Boot 2010.09-R8 (Dec 03 2012) from NAND. It runs at `0xa1f00000` and does not relocate itself.
- **Autoboot:** "Enter password - autoboot in 2 sec". For the password, see ToonRooter (https://github.com/martenjacobs/ToonRooter). A wrong or missing password boots `bootcmd=run boot_nand`.
- **`boot_nand`** runs `nandargs addmtd addtty addmisc`, then `nand read ${loadaddr} kernel` and `bootm`, with `loadaddr=0xA1000000`. The stock kernel is a legacy uImage, uncompressed, loaded and started at `0xa0008000`.
- **Stock bootargs:** `ubi.mtd=4 root=ubi0:rootfs rw rootfstype=ubifs <mtdparts> console=ttymxc0,115200 mem=128M lpj=999424`.
- **Useful U-Boot commands:** `nand read`, `crc32`, `md`, `go`, `bootm`, and also `tftpboot`/`dhcp`. `saveenv` writes the env, so never use it during an investigation.
- **ToonRooter's RAM U-Boot** (2010.09, Feb 2017; its password is in ToonRooter too) runs from `0xa1f00000` when loaded over JTAG.
- **JTAG `reset halt`:** it can only halt about 300 ms after reset (because of `srst_pulls_trst`). By then U-Boot has set up clocks and DDR and runs from DDR.
- **The JTAG dev loop:** `reset init`, `load_image` of an ELF, `resume`; halt, step and hardware breakpoints work. U-Boot's `tftpboot` is a second load path.

## NAND layout
The stock layout:

| # | Offset | Size | Name | Contents |
|---|---|---|---|---|
| — | `0x000000` | 1 MiB | u-boot | U-Boot R8 (not mapped in Linux) |
| 0 | `0x100000` | 512 KiB | u-boot-env | env, with CRC (read-only in Linux) |
| 1 | `0x180000` | 1.5 MiB | splash-image | BMP |
| 2 | `0x300000` | 3 MiB | kernel | uImage `Linux-2.6.36-R10-h28`, 1.9 MiB |
| 3 | `0x600000` | 3 MiB | kernel-backup | blank (all 0xFF) on one unit |
| 4 | `0x900000` | 119 MiB | rootfs | UBI, one UBIFS volume `rootfs`, LZO |

- **Bad blocks:** on one unit, blocks 25 (`0x320000`, in kernel), 59 (`0x760000`, in kernel-backup) and 377 (`0x2f20000`, in rootfs). `nand read` skips them. The same unit's UBI had 951 good PEBs, with erase counts up to 2896 (mean 1034).
- UBIFS often reports "recovery needed" (it was not unmounted cleanly), which is normal after power cuts.
- **Back up the NAND before you write to it.** `tools/toon/nand-backup.py` makes a verified physical image over JTAG (bad blocks filled with 0xFF), and `tools/toon/nand-verify.py` checks it. Your NAND backup holds the previous owner's data (Wi-Fi passwords, account tokens): keep it private.

## touchos on the NAND
touchos uses everything from `0x300000` on (block 24 onwards). Blocks 0–23 (U-Boot, its env and the splash image) are never written, and U-Boot's env is never changed. `libs/nand` has the controller driver and the layout; `tools/toon/nandboot` the boot stage and the flasher.
- **The NFC (v1)** reads and writes as Linux's `mxc_nand` does: four 512-byte subpages per 2 KiB page, hardware ECC, and the NFC waits for ready/busy itself. The chip ID reads `ec f1 00 95`. An erased page reads as 0xff without an ECC error.
- **Bad blocks:** a block is bad when the first spare byte of buffer 0 is not 0xff in page 0 or page 1, as U-Boot and Linux decide it. Every reader and writer steps over bad blocks; nothing is ever written to one. Bad blocks that appear later (a failed program or erase) are not marked yet: the write fails instead.
- **Layout** (in erase blocks):

  | Blocks | Use |
  |---|---|
  | 24–71 | the boot image (U-Boot's `boot_nand` reads 3 MiB of good blocks from `0x300000`, stepping over bad ones) |
  | 72–75 | state records (which copy is active) |
  | 76–83 | config records (one per block) |
  | 84–403 | copy A |
  | 404–723 | copy B |
  | 724–1023 | unused |

- **Booting:** U-Boot's stock `bootcmd` loads the boot stage (a 22 KB uImage, run at `0xa7000000`) and `bootm`s it. The stage reads the newest state record, loads that copy's kernel (a flat binary for `0xa0008000`), initrd and boot info, and the newest config, where the JTAG loader would put them, and jumps to the kernel. If a copy's CRCs fail, it tries the other one.
- **Flashing** (`cargo xtask flash`): the stage's second entry point, entered from JTAG with the payload in RAM. 8.4 MiB is written and verified in about 20 s. Without `--config`, the config ring is left alone, so a flash keeps the saved settings.
- **Settings** are saved in the config ring by the storage server (argument `nand`): each settings change from the web page is a new record, 2–3 KiB in 8–12 ms (erase, program and read back).
- **Updates** from the settings page (`cargo xtask bundle`) are written to the inactive copy, which is then put on trial; see `tools/toon/README.md`.
- **Boot time:** from U-Boot's first line to its NAND read 4 s (including the 2 s password wait), the read and `bootm` 1 s, the stage about 3 s (a bad-block scan of 952 blocks, then 8.4 MiB read and CRC-checked with the MMU and caches on; 10 s with the D-cache off). Measured from a restart: the boot stage starts after 8 s, the kernel 3 s later, and Home Assistant is connected about 10 s after that.

## Display
- The stock kernel log says "Configured for LCD: **TM070RDH11**" (the website names TM070RDH03). 800×480.
- **The LCDC (`0x10021000`, IRQ 61)** is set up by U-Boot. Linux keeps that setup unchanged (`PreserveUBootFramebuffer … skip _update_lcdc`):

| Register | Value | Meaning |
|---|---|---|
| LSSAR | `0xa1400000` | framebuffer (768,000 bytes) |
| LSR | `0x032001e0` | 800 × 480 |
| LVPWR | `0x190` | 400 words per line (800 × 16 bit) |
| LPCR | `0xcae80086` | TFT, colour, **BPIX = 5: 16 bpp (RGB565)**, FLMPOL = LPPOL = CLKPOL = 1, SCLKIDLE, SCLKSEL, PCD = 6 (LSCLK = PERCLK3 / 7) |
| LHCR | `0xbc005725` | H_WIDTH field 47, H_WAIT_1 field 87, H_WAIT_2 field 37 (raw fields; see the RM for the +n offsets) |
| LVCR | `0x0c000d1e` | V_WIDTH field 3, V_WAIT_1 field 13, V_WAIT_2 field 30 |
| LPCCR | `0x00140553` (U-Boot), `0x00140353` (Linux) | the LCDC's contrast PWM: Linux's `contrast` device, 83 of 32–224 |

- Linux's `fb0` ("DISP0 BG") reports 32 bpp with stride 3200, but the hardware stays in U-Boot's 16 bpp mode until something changes it. There is also a `fb1` ("DISP0 FG", the graphic window).
- **touchos** takes over U-Boot's mode and draws the dashboard in RGB565 into `0xa1400000`. Nothing about the display needs programming. LPCCR stays at U-Boot's value.
- **U-Boot sets up the LCDC only well into its start.** Loading touchos over JTAG must wait for the U-Boot prompt, or the panel shows nothing.
- **A screenshot over JTAG:** with touchos running the MMU is on, so dump the framebuffer through the kernel's direct map: `toon-ocd "halt; dump_image /tmp/fb.raw 0xc1400000 768000"`, then `tools/toon/fb2png.py`. The physical address gives a data abort.

## Backlight
- **PWM1 (`0x10006000`)** drives the backlight: Linux `ed2.0-bl.0`, brightness 50 of 100. It matches `backlight_brightness=50` in the U-Boot env.
- PWMCR is `0x00020001` (enabled), PWMSAR/PWMPR is `0x2df9`/`0x5bf1`, which is 50 %.
- touchos's `ui` drives PWM1 from the config (brightness, dimming).
- There is also a `leds-gpio` device (`ed20.cfg`'s "debug LEDs" on GPIO port F; their function is unknown).

## Touch, sensors, audio (I²C)
| Bus (`imx-i2c`) | Addr | Device | Linux driver |
|---|---|---|---|
| I2C1 `0x10012000`, IRQ 12 (Linux `i2c-0`) | 0x48 | ADT7410 temperature | adt7410 |
| | 0x49 | ADT7410 temperature | adt7410 |
| | 0x4c | TMP431 (declared; does not respond, per the website) | none bound |
| I2C2 `0x1001d000`, IRQ 1 (Linux `i2c-1`) | 0x18 | TLV320AIC3111 audio codec | none bound |
| | 0x48 | **TSC2007 touch controller** (on the main board), IRQ 244 (GPIO) | tsc2007, `event0` |
| | 0x6f | ISL1208 RTC | rtc-isl1208 |

The website lists one ADT7410 (0x49) and puts the codec on bus 0. Linux declares two ADT7410s here, and the codec on bus 1.
Which `imx-i2c` instance Linux numbers `i2c-0` follows the iomem order above; check against the RM before relying on it.

**What answers** (an address scan under touchos, about 100 kHz with IFDR 0x38):
- **I2C1:** only `0x49`, an ADT7410. Neither `0x48` nor `0x4c` answers: the second ADT7410 and the TMP431 are probably not fitted.
- **I2C2:** `0x48` (TSC2007) and `0x6f` (ISL1208). The codec at `0x18` answers only once PB25/PF13 are high (set for USB host 2, see below), so one of those pins probably also releases it from reset.
- **The ISL1208 holds UTC.** touchos only reads it.
- **Pins:** U-Boot leaves the I²C pins as GPIOs. touchos puts PD17/PD18 (I2C1) and PC5/PC6 (I2C2) on their primary function, with GIUS and GPR clear, as Linux does. The I²C clocks (PCCR0 bits 17/18) are already on.
- **The TSC2007's pen-down line is PF20:** Linux's IRQ 244 is GPIO interrupt 64 + 5 × 32 + 20, and under the stock firmware PF20 is a GPIO input (GIUS F `0xff926880`). It reads high with no finger (SSR F bit 20). All GPIO ports share AITC line 8. touchos waits on this line and takes a check sample every 5 s; if that sample finds a touch the line missed, it falls back to polling, with a warning.
  - **It dips low after every measurement:** with PENIRQ re-enabled (the POWER_DOWN command), the line goes low for a moment with no finger, and is high again by the time the driver looks. A wake-up that finds the line high takes no sample, which ends the cycle; otherwise each sample causes the next wake-up (about 750 a minute).
- **The touch panel:** x runs left to right, y bottom to top (raw values). On one unit the corners (the average of 5 to 8 taps each) were left 183, right 3935, top 3745, bottom 322 of 0..4095. Whether this varies per panel is not known. A landing or lifting finger can give a saturated reading (0 or 4095); touchos skips those.
- **A stuck bus after a JTAG reset:** a reset in the middle of a touch poll can leave a device on I2C2 holding SDA low (on the pins: PC5 low, PC6 high). The kernel does a bus recovery at boot (at most 9 clocks, then a STOP), as Linux does.
- **Open:** in about a third to half of the boots, the very first transfer on I2C1 (a probe of the unused address 0x7f) takes about 5.6 ms instead of 0.1 ms. Its result is still right (no acknowledge), and everything after it is normal. Cause unknown.

## USB and Wi-Fi
- **Two EHCI controllers:**
  - `mxc-ehci.0` at `0x10024000` (IRQ 56): the external port (the OTG controller; see "The external USB port" below).
  - `mxc-ehci.2` at `0x10024400` (IRQ 55): internal, host 2 (H2).
- **The Wi-Fi module is a Ralink RT5370 (USB `148f:5370`), not an RT5390.** It sits on `mxc-ehci.2`, port 1.
- **The stock driver** is Prodrive's vendor driver `rt5370sta.ko`, a module loaded by the stock scripts. `rtl8191su.ko` is also on rootfs. There are no files under `/lib/firmware`, so the firmware is probably built into the driver.
- **H2 is ULPI** to a Microchip USB3370 PHY (community page), high speed (the stock log: "usb 2-1: new high speed USB device using mxc-ehci and address 2").
- **How stock Linux brings H2 up** (disassembled from the Toon's own kernel):
  1. Board init: the ULPI pins as GPIOs, data low, STP (PA4) high; PB25 (`USB1_RESEST_N`) and PF13 (`WLAN_DIS_N`) outputs, high.
  2. Probe: clocks PCCR1 bits 25 (usb) and 11 (usb_ahb); USBMODE (+0x1a8) |= 3; PORTSC1 (+0x184) = `0x80000000` (PTS = ULPI); 10 ms.
  3. Then the 12 ULPI pins go to the controller: PA0 CLK, PA1 DIR, PA2 DATA7, PA3 NXT, PA4 STP on their primary function; PD19 DATA4, PD20 DATA3, PD21 DATA6, PD22 DATA0, PD23 DATA2, PD24 DATA1, PD26 DATA5 on their alternate function; no pull-ups.
  4. `USB_CTRL` (`0x10024600`): H2 SIC (bits 22:21) = 0, power mask (bit 16) and DT (bit 5) set; read-modify-write, the register is shared with the OTG and H1 ports.
  5. EHCI reset (USBMODE = 3 again after it) and run.
- **Golden values, H2 running** (read over JTAG under the stock kernel after it enumerated the RT5370):
  - CCM: CSCR `0x43f0020f` (USB_DIV 4), SPCTL0 `0x0475206f` (SPLL 300.0 MHz, so the USB clock is 60 MHz), SPCTL1 `0x8000` (locked), PCCR0 `0x97814800`, PCCR1 `0xe21a8f00`.
  - GPIO A: DDIR `0xffffffe0`, GIUS `0x0f000000`, GPR 0, PUEN 0. B: DDIR `0x02000000`, DR `0x02000000`, PUEN `0xfdffffff`. D: GIUS `0x1a01feff`, GPR `0x05f80100`. F: DDIR `0x00826880`, DR `0x00006000`, GIUS `0xff926880`.
  - `USB_CTRL` `0x00014020`.
  - H2: ID `0x0042fa05`, CAPLENGTH `0x40`, HCIVERSION `0x0100`, HCSPARAMS `0x00010011` (1 port, port power control), HCCPARAMS `0x6`; USBCMD `0x00010025`, BURSTSIZE `0x808`, TXFILLTUNING `0x00020000`, CONFIGFLAG 1, PORTSC1 `0x88001205` (ULPI, high speed, enabled, connected, powered), USBMODE 3.
- **Under touchos:** the kernel does the stock sequence (`setup_usb_h2` in `kernel/src/board/imx/toon.rs`), then the Wi-Fi server's EHCI driver resets the controller and the port.
  - H2's ULPI PHY reads `0424:000d` (Microchip/SMSC), as the OTG port's does.
  - The RT5370 enumerates at high speed: `148f:5370` "802.11 n WLAN", one vendor-specific interface (`ff:ff:ff`), bulk IN `0x81` and bulk OUT `0x01`–`0x06`, all 512 bytes.
  - U-Boot leaves the USB clocks (PCCR1 bits 11 and 25) off. Any access to the USB block (`0x10024000`) before they are on hangs the bus, also over JTAG.
- **The RT5370:**
  - MAC_CSR0 `0x53900503`: RT5390 family, revision 0x0503 (RT5370G).
  - Efuse v1.1: RF5370, 1T1R, frequency offset 0x2f, LNA 0x0a, RSSI offsets 0, TX power 14 (channels 1–9) and 15 (10–14), external LNA. The efuse also holds the MAC address.
  - `rt3071.bin` loads with a valid CRC and reads back; the MCU is ready after 14 ms.
  - The autorun check (DEVICE_MODE 0x11, IN) times out: this module doesn't answer it. That costs about 5 s of the init.
  - A passive scan of channels 1–13 takes 2.9 s: 210 ms per channel, two beacon intervals. With 110 ms (1.6 s in all) a scan sometimes missed an access point a few metres away. While scanning, MAC_SYS_CTRL is `0x8`: RX on, TX off, so no probe requests.
- **Wi-Fi under touchos:** the RT5370 as a WPA2 station (legacy 802.11g rates).
  - Management frames at 1 Mbit/s; data rates chosen by AMRR, as in OpenBSD. A step down waits for more than 10 transmissions, so it doesn't react to samples like 1 retry in 2 frames. In use the rate moves between 18 and 36 Mbit/s.
  - Authentication and association are ACKed at the first try; the 4-way handshake and the group key complete.
  - Leaving (the link lost, a failed attempt, or a restart for new settings), the station sends a deauthentication. A Raspberry Pi 4 as access point (Broadcom firmware) otherwise kept the panel as associated and ignored its next authentication, so a live `wifi.ssid` change never got the panel back on.
  - With the Ethernet PHY down or the cable unplugged, `net` moves to Wi-Fi: DHCP in about 1 s, TLS to Home Assistant in 1.6 s. After unplugging the cable, HA is connected again about 8 s later.
  - The web page answers in 44–105 ms over Wi-Fi; ping about 6 ms.
  - An hour on Wi-Fi: no reconnects, no warnings; 3005 frames sent, 0 failed (no ACK), 0 USB errors.
  - **Open: retries are 12–16 % of the transmissions at every rate from 6 to 54 Mbit/s,** even with a strong signal. A link too weak for the rate would improve at slower rates; this doesn't. So it's likely collisions (no RTS/CTS) or a chip timing setting (ACK timeout), not the signal.
  - Not yet tested on the hardware: beacon loss and the reconnect back-off.
- **Hardware CCMP:** the RT5370 encrypts the unicast frames touchos sends; received frames are decrypted by the host.
  - **Sending:** the pairwise key in PKEY (entry 1), WCID_ATTR(1) = cipher 4 (AES-CCMP) << 1 | bit 0. Bit 0 (OpenBSD's `RX_PKEY_EN`, Linux's `KEYTAB`) selects the pairwise table for sending too: without it the chip encrypts with the empty shared key, and the access point drops every frame (ACKed, but DHCP never answers). TXWI names entry 1, TXINFO has WIV set, and the CCMP header (the station's PN) is in TXWI's IV words. With the header in the frame instead, the access point drops the frames as well.
  - **Receiving:** with the access point's address in WCID_ENTRY, the chip decrypts its unicast frames (RXD bit 16) and strips the CCMP header *and* MIC: the body starts with the LLC header, and the length is 24 + 8 + the IP length. Without the packet number there is no replay check (Linux's rt2800 accepts that). So touchos keeps the address out of WCID_ENTRY, and the chip leaves received frames alone. Group frames (key id 1/2) are never decrypted by the chip.
  - **Measured** (a 1 MiB download from the web server over Wi-Fi, 50 s in a loop): software CCMP 35 downloads, about 730 KB/s; hardware CCMP 31 downloads. No difference within the noise: CCMP isn't what limits throughput. The synthetic CCMP test at boot (346–351 KiB/s, 64 KiB in 1500-byte frames) underestimates it: software CCMP moved 5.8 Mbit/s in the real download.
- **Receive filter while joined:** RX_FILTR_CFG adds DROP_NOT_MY_BSSD (bit 3) and DROP_MC (bit 5); leaving puts back the station filter, which keeps every network's beacons for the scan. With the station filter alone, the chip passed about 83 frames a second, and 25 of them were for our network (beacons 10, multicast 6, unicast 7, broadcast 1.5). The rest were the neighbours' beacons, which cost a wake-up each. An 8 ms EHCI interrupt threshold (USBCMD ITC 64) doesn't reduce those wake-ups and makes ping worse (17 ms instead of 6).
- **Since USB host 2 is set up** (PB25/PF13 high), I2C2 also has a device at `0x18`: the TLV320AIC3111 codec.

## The external USB port (OTG controller)
Read over JTAG under stock Linux with a USB SSD in the port. Stock Linux runs it as an EHCI host (`mxc-ehci.0`) and finds the disk (`usb-storage`).
- **The controller** at `0x10024000` (capability registers at +0x100): ID `0x0042fa05`, HCSPARAMS `0x00010011` (1 port), USBCMD `0x00010005`, PORTSC1 `0x88001205` (ULPI, high speed, enabled, connected, powered), USBMODE 3 (host). USB_CTRL `0x00014020`: the OTG fields (serial interface, bits 30:29; power mask, bit 24) are 0.
- **Its ULPI PHY** is a second SMSC USB3370 (`0424:000d`). OTG Control (ULPI register 0x0a) = `0x67`: DrvVbus and DrvVbusExternal (the PHY switches VBUS), IdPullup, and the D+/D- pull-downs. Function Control `0x40`, Interface Control `0x08`.
- **Pins**, primary function, no pull-ups (U-Boot leaves them GPIOs with pull-ups): NXT PE0, STP PE1, DIR PE2, CLK PE24, DATA7 PE25, DATA0-6 on PC7-PC13. Port C under stock: GIUS `0x0fff801f`, GPR 0, PUEN `0x0fff801f`; port E: DDIR `0x00041142`, DR `0x00040002`, GIUS `0xfcfc0c00`, GPR 0, PUEN `0xfcfbcc18`.
- **PE18** is a GPIO output, high, under stock (an input under U-Boot). touchos sets it so, taking it to be the PHY's enable, as PB25 is host 2's. **But the stock kernel's SPI chip-select table says PE18 is CS1 of `spi0`, the Z-Wave chip's programming port** ("SPI" below); high deselects it. So it is probably not the PHY's enable. Whether the port works without it is not tested.
- **Under touchos:** the kernel does the stock sequence (`setup_usb_otg`), and the usb server's EHCI driver resets the controller and writes OTG Control `0x67` (argument `vbus`). Port changes arrive through the controller's interrupt. A USB SSD enumerates at high speed and reads at 24–28 MB/s (64 KiB commands, Bulk-Only Transport). Disks are only read (touchos's mass-storage code has no write command).
- The OTG controller and host 2 share the USB block's page: the usb server has the first, the Wi-Fi server the second, both with the same page as their capability. Both run at once.

## Random numbers (SAHARA2)
The i.MX27's only hardware RNG is in the SAHARA2 crypto engine (`0x10025000`, IRQ 59, version 3). It has no RNGA, and SCC/RTIC are no random sources. The stock kernel doesn't use it. touchos's kernel does (`kernel/src/board/imx/rng/sahara.rs`), polled, after Freescale's BSP (`drivers/mxc/security/sahara2`):
- **Clocks:** PCCR0 bit 7 and PCCR1 bit 13; both are off under U-Boot and the stock kernel.
- **Start-up:** reset in batch mode, control `0x00080000`, one random number, then auto-reseed (bit 7). Generating once first is the BSP's workaround for a hardware bug.
- **One descriptor per request:** `{0xB18C0000, n, phys(out), 0, 0, 0}` (RNG generate, LLO).
- **Completion:** on the Toon, only the first descriptor after a reset ends in state DONE; later ones go straight back to IDLE. Completion is therefore taken from CDAR (the descriptor fetched last; two descriptor slots in turn) plus "not busy".
- **Measured:** 256 bytes at boot, 1030 of 2048 bits set. 32 fresh bytes are mixed into the pool on every `RANDOM` request.

## Ethernet
FEC (`0x1002b000`, IRQ 50) in MII mode, with a 100 Mbit/s PHY. Stock Linux calls it "Generic PHY" at MII address 1. touchos's scan finds it at MDIO address 16, ID `0x0015f450`. The MAC that U-Boot leaves in PALR/PAUR starts with `00:0f:11`.
- **Pins** (Linux's values, read over JTAG; U-Boot leaves them on their primary functions):
  - PD0–PD16 carry the MII signals through the GPIO module (GIUS set, AIN for outputs, AOUT for inputs), except MDIO on PD8, which is an alternate function (GPR);
  - **TX_EN is PF23** (GPIO mode, output from AIN), not a port D pin. Without it the FEC "sends" (descriptors complete, EIR shows TXF) but nothing reaches the wire, while receiving works;
  - the PHY's reset is PA25, active low.
- **Measured** (a switched LAN): the first DHCP DISCOVER after link-up is lost every time, so the 4 s retry is what counts. TLS to Home Assistant takes about 0.55 s, certificate verified, on the ARM926 at 400 MHz (rustls with the RustCrypto provider; embedded-tls took 1.1–1.2 s). The web page answers in 30–40 ms.
- **A cable re-plug can be simulated over JTAG:** write the PHY's BMCR through MMFR. `mww phys 0x1002b040 0x58020800` powers it down (link down); `0x58021200` restarts autonegotiation (link up).

## Other
- **SPI** (`spi_imx.0`, `0x1000e000`, IRQ 16):
  - the stock kernel's chip selects (the `spi_imx` platform data): **CS0 = PD28, CS1 = PE18**, both GPIOs. CSPI1's own pins are PD29 (SCLK), PD30 (MISO) and PD31 (MOSI); U-Boot leaves PD28–PD31 as GPIO inputs, and they read high. CSPI1's clocks are off under U-Boot (PCCR0 bit 31 ipg, PCCR1 bit 9 per2);
  - `spi0.0` is declared as an at25 EEPROM (an AT25640B, 32 KiB), but **nothing answers there** (tested over JTAG, read-only: RDSR and READ only). MISO stays 1 for every bit in every attempt: CSPI1 and bit-banged GPIO; the Z-Wave chip running and in reset; PF11 as left and driven low as the flasher does. Pins and CS were seen to change in SSR. The at25 driver's boot line (`32 KByte at25640B eeprom`) comes from the board file, not from the chip, so the EEPROM is probably **not fitted**, like the second ADT7410 and the TMP431. Nobody has checked the board by eye yet;
  - **careful on this bus:** RESET_N low (PF14) puts the ZW0301 into SPI-programming mode on the same SCLK/MOSI/MISO. It ignores everything until the 4-byte Programming Enable (`AC 53 AA 55`);
  - `spi0.1` is the Z-Wave chip's **programming** path, not its protocol (see "Z-Wave" below).
- **UARTs:**
  - `ttymxc0` (UART1, `0x1000a000`): the console;
  - `ttymxc1` (UART2, `0x1000b000`, IRQ 19): the boiler link (OpenTherm through the Ketelmodule). Stock `happ_thermstat` opens it at **4800** baud (`qmf_hardware.xml`);
  - `ttymxc2` (UART3, `0x1000c000`, IRQ 18): Z-Wave, at 115200 (below).
- **UART2 under touchos:** PE6/PE7 are on their UART function (GIUS E bits 6/7 clear) and the RX line idles high. Its register clock (PCCR1 bit 30) is off under U-Boot: the control registers still take writes, but UBIR/UBMR read back 0 until the kernel ungates it. `servers/boiler` runs it receive-only like UART1 (UCR2 `0x4023`: TXEN clear; UCR3 `0x704`; UFCR divider 2; UBIR 15, UBMR 103 × 115200 / rate − 1), trying 4800 baud first, then other rates. With no boiler behind the Ketelmodule, nothing arrives.
- **Z-Wave** (from the stock rootfs and kernel, analysed offline, and from the chip itself):
  - **The chip** is a ZW0301 ("Serial Z-Wave controller on JP29") running Sigma's Serial API firmware `4.55-serialapi_controller_static_norep_noflirs_nomr_ZW030x_EU.bin`, a static controller on the EU band.
  - **The protocol** runs on `ttymxc2` at 115200. `hdrv_zwave` opens the port named in `qmf_hardware.xml` and passes `B115200` to `cfsetospeed` (disassembly of `serial_port_open`).
  - **`zwave300.ko` is only for flashing the chip.** It exposes the chip's 32 KiB flash as an MTD partition (`zwave300`) over `spi0.1`. The firmware package's postinst loads it, compares or writes the image, and unloads it again. It backs up the at25 EEPROM before and restores it after, since the EEPROM shares the bus. In normal operation the module isn't loaded.
  - **The programming GPIOs** are five entries in the SPI device's platform data. The `zwave300` `spi_board_info` (bus 0, CS 1, 1 MHz) in the stock kernel image lists **PF14 (RESET_N), PD28 (CSPI1_SS0), PD29 (SCLK), PD31 (MOSI), PF11**. Programming mode sets PD28 high, PD29/PD31/PF11 low, and PF14 low (RESET_N, which puts the ZW0301 into SPI programming). Leaving it sets PF14 high and PD28 low, so the chip runs again.
  - **Under U-Boot the chip is held in reset:** PF14 is on its primary function and reads low (GIUS F bit 14 clear, SSR bit 14 = 0), and PE8–PE11 are GPIOs (GIUS E `0xfcfc0f00`; stock Linux has `0xfcfc0c00`, with PE8/PE9 on UART3). The kernel (`setup_zwave_uart` in `kernel/src/board/imx/toon.rs`) puts PE8/PE9 on UART3, ungates UART3's clock (PCCR1 bit 29), and makes PF14 a GPIO output: low for 20 ms, then high.
  - **What it answers:** "Z-Wave 3.67", library static controller; Serial API firmware 3.94, 33 functions; Serial API 5, chip type 3, version 1. The controller keeps its network (home ID and nodes) across power cycles, so a second-hand Toon still holds the previous owner's network. SetDefault (`zwave=reset-pair`) gives a new home ID, with the controller as node 1.
  - **The antenna:** without it the chip hears a button press from close by, but not reliably.
  - **An Eneco meter adapter** (`HAE_METER_v2`) reports: a routing slave, generic class meter (0x31:0x02), always listening. Manufacturer 0x0128, product type 0, product 0. Version Report: library type 3, protocol 3.67, application 1.35. Its command classes: Application Status (0x22), Meter Table Config/Monitor/Push (0x3c–0x3e), CRC-16 Encapsulation (0x56), Multi Channel (0x60), Configuration (0x70), Manufacturer Specific (0x72), Firmware Update Meta Data (0x7a), Version (0x86), Time Parameters (0x8b), Powerlevel (0x73). In the stock software it has Multi Channel endpoints for gas (1) and electricity (2).
  - **Pairing it:** every short press is heard during an exclusion, but presses in inclusion-only mode (short, after a power cycle, long) are not. After an exclusion it **announces itself again by itself** (within a second), so exclusion followed by inclusion needs **one press** (`zwave=pair`). Gets sent within a second of it joining go unanswered (acknowledged, no report); seconds later they are answered.
- **Audio:** SSI at `0x10010000`.
- **Timer:** "i.MX Timer Tick", IRQ 25 (GPT1, clocksource `mxc_timer1`).

## Interrupts (stock Linux)
| IRQ | Device |
|---|---|
| 1 | I²C (`imx-i2c.1`) |
| 12 | I²C (`imx-i2c.0`) |
| 16 | SPI |
| 18 / 19 / 20 | UART3 / UART2 / UART1 |
| 25 | timer |
| 29 | NAND |
| 50 | FEC |
| 55 / 56 | EHCI (internal / external) |
| 61 | LCDC |
| 244 | TSC2007 (a GPIO interrupt) |

Under touchos, drivers claim the AITC lines of their controllers (`IRQ_CLAIM`): the I²C buses (12, 1), GPIO (8), the FEC (50), the two EHCIs (55, 56).

## Watchdog and resets
- **i.MX27 WDOG** (`0x10002000`): U-Boot leaves it off (WCR `0x0030`). Stock Linux's `imx2-wdt` uses a 60 s timeout.
- **`hcb_watchdog`**, a stock app respawned from inittab, runs with `monitorQt=1`. Without a screen, the stock GUI probably fails, which would explain spontaneous reboots of a bare main board under the stock firmware.
- **touchos arms it** (60 s, WDBG set so a JTAG halt pauses it), and init pets it. With `test_fail=init`, the Toon restarts into U-Boot about 60 s after the last pet. WRSR then reads `0x0002` (timeout).
- **WRSR reads `0x0010`** after a power-on, after an SRST from the JTAG header, *and* after an unexplained reset in U-Boot (in the middle of a long JTAG dump). So SRST seems to feed the board's power-on reset, and the register cannot tell the causes apart.
- **SRST pull:** the unexplained reset happened with the Raspberry Pi's default pull-down on its SRST pin (GPIO 24). With a pull-up, long dumps run clean (`tools/toon/pi4-jtag.cfg`). The cause is not known.

## Driver status
What touchos drives on the Toon today:
- UART1 (console, as U-Boot sets it up), UART2 (boiler link, receive-only), UART3 (Z-Wave Serial API);
- the GPT timer, the AITC interrupt controller and the GPIO interrupts;
- the watchdog;
- the LCDC (taken over as U-Boot leaves it) and PWM1 (backlight);
- both I²C buses: TSC2007 touch, ISL1208 RTC (read only), ADT7410 temperature (logged);
- the FEC (Ethernet);
- the NFC (NAND: boot, A/B copies, settings);
- EHCI host 2 with the RT5370 (Wi-Fi), and the OTG controller as a host (USB disks, read-only);
- SAHARA2's RNG.

Not driven: the TLV320AIC3111 codec and the SSI (audio), the SPI bus, the debug LEDs, the LCDC's contrast PWM (LPCCR).
