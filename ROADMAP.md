# touchos: roadmap

Ideas and planned work, roughly in order. Hardware details: [docs/hardware-toon.md](docs/hardware-toon.md).

## The boiler link (OpenTherm)
The Toon's actual job as a thermostat: talk to the boiler through the Ketelmodule, on UART2 (`ttymxc1`, `0x1000b000`, IRQ 19).

- **Today:** `servers/boiler` listens on UART2, receive-only (the transmitter is never enabled, its pin not touched). It logs bytes and line errors per rate (4800 first, the stock thermostat's rate, then other rates when most characters have errors), the first bytes in hex, and OpenTherm frames if the lines are in the OpenTherm Gateway's text format. `libs/opentherm` parses and checks frames and decodes the data IDs a boiler's state is read from.
- **Open:** what the Ketelmodule actually sends: rate, framing, and how OpenTherm frames are wrapped.
- **Steps:**
  1. With a boiler behind the Ketelmodule: record what arrives, and decode it.
  2. The boiler's state (flow and return temperature, modulation, flame, pressure) as Home Assistant sensors, through the bridges (see "Home Assistant bridges").
  3. Sending (setpoints) only after reading works well, and with care: it controls the heating. First find out what the Ketelmodule and the boiler do when touchos stops talking.

All three: [#3](https://github.com/mactijn/touchos/issues/3).

## The thermostat
touchos runs the heating itself and offers it to Home Assistant as a `climate` entity. The heating keeps working when Home Assistant or the network is down, and the panel's own dashboard controls it directly.

- **Today:** the board's two ADT7410 sensors (I2C1, `0x48`, `0x49`) are read every 5 minutes and only logged (`servers/platform`). They sit next to the processor, backlight and Wi-Fi chip, so they read above the room temperature.
- **Steps:**
  1. The room temperature: read both sensors often, find which one follows the room best, and add an offset and smoothing ([#18](https://github.com/mactijn/touchos/issues/18)).
  2. `servers/thermostat`, the control loop ([#19](https://github.com/mactijn/touchos/issues/19)):
     - target, mode (heat/off) and presets (comfort/eco/away), kept across reboots;
     - on/off with hysteresis first, then a PI controller on the boiler's CH setpoint;
     - frost protection;
     - a dashboard tile that works without Home Assistant;
     - a dry run that logs what it would send until the boiler link can send (step 3 above).
  3. Later: a weekly schedule on the panel. Until then, Home Assistant automations can switch presets.

## Z-Wave: local and passthrough modes
The ZW0301 controller runs on UART3 at 115200 (`servers/zwave`, `libs/zwave`). It can make a network of its own, pair a device (`zwave=pair`, `reset-pair`, `include` on the boot command line), ping, and ask Manufacturer Specific and Version. What's missing is the data.

- **Why:** an Eneco meter adapter on the electricity and gas meters is the Toon's other main source of data, and the radio can serve other Z-Wave devices in the house too.
- **Two modes** (`zwave.mode`). The network lives in the chip, so switching keeps the paired devices.
  - **local:** touchos is the controller and the application: the meter adapter, switches and plugs, dimmers and sensors. Optionally exposed to Home Assistant through the bridges (see "Home Assistant bridges").
  - **passthrough:** the Serial API, bridged byte for byte over TCP, so Home Assistant's Z-Wave JS drives the radio (`tcp://<panel>:<port>`), for one allowed IP.
- **Open:**
  - Whether Z-Wave JS accepts a 300-series controller (Serial API 4.55, Z-Wave 3.67); it officially targets 500 series and later.
  - What the meter adapter sends unprompted once it's on a meter (Meter Table Monitor/Push, Meter, possibly CRC-16 encapsulated). Stock `hdrv_zwave` has debug info and is the reference.
- **Steps:**
  1. The mode setting and the TCP passthrough; settles the Z-Wave JS question ([#14](https://github.com/mactijn/touchos/issues/14)).
  2. Local mode ([#15](https://github.com/mactijn/touchos/issues/15)):
     - command classes, including the meter adapter's reports and its gas and electricity endpoints;
     - the shared entity model the bridges read;
     - a node cache;
     - pairing from the settings page instead of a boot switch. Untested so far: the 5 s wait before asking a new node.
  3. Later: security (S0/S2), needed for locks.

## Home Assistant bridges
Nothing goes from the panel to Home Assistant yet: `servers/ha` only mirrors Home Assistant's states for the dashboard. The bridges publish the panel's own devices. They read one shared entity model (`libs/proto/src/entity.rs`): Z-Wave devices in local mode, the room temperature, the thermostat and the boiler's sensors. Each source writes its own board and takes commands on its own ring.

- **Steps:**
  1. MQTT, with Home Assistant's MQTT discovery; needs a broker ([#16](https://github.com/mactijn/touchos/issues/16)).
  2. The ESPHome native API, so no broker is needed. Until mDNS ([#24](https://github.com/mactijn/touchos/issues/24)), the panel is added by IP ([#17](https://github.com/mactijn/touchos/issues/17)).

## First setup and factory reset
A new panel needs its settings flashed with `cargo xtask flash --config`, or a cable and its IP: without `wifi.ssid` it has no network, and its IP is never shown on screen.

- **Steps:**
  1. A setup Wi-Fi network: `touchos-XXXX`, open, only while unconfigured; the on-screen PIN pairing protects the settings. It needs an AP mode in `libs/wlan`/`libs/rt2800` (beacons, probe/auth/assoc responses, FromDS framing), and a DHCP server, catch-all DNS and captive-portal redirects in `net` ([#23](https://github.com/mactijn/touchos/issues/23)).
  2. mDNS: `touchos-xxxx.local`, `_http._tcp`, later `_esphomelib._tcp` for the ESPHome bridge. Needs multicast in smoltcp and on the Wi-Fi chip (`DROP_MC`, see "Fewer wake-ups") ([#24](https://github.com/mactijn/touchos/issues/24)).
  3. An on-boarding wizard in the settings page: pair, pick a Wi-Fi network from a scan, time zone, Home Assistant (optional; skipping it picks the thermostat app), Z-Wave mode ([#25](https://github.com/mactijn/touchos/issues/25)).
  4. A factory reset: erase the whole config ring (older records keep the old PSK and token otherwise) and SetDefault on the Z-Wave chip, done at boot so a power cut can't leave it halfway. From the System page, or by holding the screen during boot ([#22](https://github.com/mactijn/touchos/issues/22)).

## Apps and weather
The panel runs one app at a time (`ui.app`): the Home Assistant dashboard, a thermostat, and the setup screen while unconfigured. They live in one `ui` process: the screen, input, dimming, the PIN and the power menu must work the same whatever is in front, the fonts are ~800 KB per binary, and the ARM926 has only 31 FCSE pids.

- **Today:** `servers/ui` is the dashboard, with the system parts mixed in. A weather entity reaches it, but `libs/hass` keeps only its state (and its temperature, by accident, as `target_temp`), and Font Awesome 4 has no rain, fog or hail icons.
- **Steps:**
  1. Split `servers/ui` into a shell (screen, input, dimming, overlays, the frame loop) and apps behind an `App` trait. Apps reach the rest of the system only through a `Cx` of board reads and ring pushes, so one could move out of process later ([#30](https://github.com/mactijn/touchos/issues/30)).
  2. The weather from Home Assistant: the weather fields in the HA snapshot, the outside temperature and a condition icon in the header next to the clock, a weather tile, and a weather icon font ([#27](https://github.com/mactijn/touchos/issues/27)).
  3. One `weather` board from a new `servers/weather`. It picks the source (Home Assistant's entity, or the internet through Open-Meteo), so the UI doesn't care. The internet source needs an HTTP client, and its server the "cacerts" capability from init, like `ha` ([#31](https://github.com/mactijn/touchos/issues/31)).
  4. The thermostat app (`ui.app = "thermostat"`): the thermostat, a clock and the weather, no Home Assistant needed. Depends on the thermostat ([#19](https://github.com/mactijn/touchos/issues/19)) ([#28](https://github.com/mactijn/touchos/issues/28)).

## Time zones
- **Today:** a fixed UTC offset (`display.utc_offset_minutes`) plus EU summer time or none (`display.dst`, `libs/civil`). Right for most of Europe, wrong for North America, the southern hemisphere, and every rule change.
- **Steps:** `time.zone` as an IANA name, from a table of each zone's current POSIX TZ rule generated from tzdata (`cargo xtask tzdata`), with a POSIX TZ parser in `libs/civil`; `auto` takes Home Assistant's zone; the old fields stay as a fallback; a zone picker on the settings page ([#29](https://github.com/mactijn/touchos/issues/29)).

## Monitoring
The kernel's figures (switches, idle, free memory, the busiest threads) are only in the log, and no syscall lets user space read them.

- **Steps:**
  1. A stats syscall (CPU time and switches per thread, memory per process, IRQ counts), metrics boards for the servers' counters, and `/metrics` in the Prometheus format, opt-in and without a session ([#20](https://github.com/mactijn/touchos/issues/20)).
  2. A process overview on the System page: CPU, switches/s, memory and restarts per process ([#21](https://github.com/mactijn/touchos/issues/21)).

## TLS
- **Today:** TLS 1.3 (AES-128-GCM) with the certificate checked against Mozilla's root store (`libs/certs/cacerts.der`, shared by init as the "cacerts" capability). The chain itself is checked by embedded-tls, which rejects some valid certificates: more than 7 RDNs in a name (some EV certificates), more than 12 extensions, a host after the first 3 SAN names, and a P-384 key signing with SHA-256 (3 roots).
- **Steps:** check the chain ourselves, with the DER reader of `libs/certs`, and require `cA` in basicConstraints for every certificate above the leaf ([#32](https://github.com/mactijn/touchos/issues/32)).

## Wi-Fi follow-ups
The RT5370 runs as a WPA2 station; these are the loose ends.

- **The reconnect path on the hardware:** beacon loss, leaving the BSS, the rescan back-off (10, 20, 40, 60 s) and the rejoin are built and host-tested, but not yet run on the Toon. Test it with a test-only switch that makes the Wi-Fi server ignore received frames for about 2 minutes after joining.
- **Retries at 12–16 % at every rate:** rate-independent, so collisions or a chip timing setting (EXP_ACK_TIME, XIFS) rather than the signal. Compare the timing registers with OpenBSD's run(4) init; try RTS/CTS.
- **Throughput:** uploads run at about 216 KB/s, a third of a download (about 700 KB/s). Neither CCMP nor process switches turned out to be the limit for downloads. Received frames are decrypted in software: the chip can decrypt them, but it strips the packet number, so there would be no replay check. Find where the time goes first.

## 802.11n (HT)
The RT5370 connects as a legacy 802.11g station.

- **Why:** an access point in "n only" mode refuses a legacy station. HT also gives higher rates and better range.
- **What it needs:**
  - HT capability IEs in the association request;
  - QoS data frames;
  - HT rates: the TXWI PHYMODE/MCS fields, and rate control that knows them;
  - optionally A-MPDU/block-ack.

## Fewer wake-ups
Drivers get interrupts (`IRQ_CLAIM`/`IRQ_BIND`/`IRQ_ACK`) and servers wait for events instead of polling on timers; idle, the Toon runs about 50 process switches a second. The kernel logs who was switched to most every minute (`runs/s:`). What's left:

- `wifi` is still the biggest: every frame of our network (beacons, broadcasts, our own traffic) is a wake-up.
- The Wi-Fi chip drops multicast while joined (DROP_MC). IPv6 or mDNS ([#24](https://github.com/mactijn/touchos/issues/24)) would need it back, with a multicast filter.
- `net` ships the kernel log over UDP from its loop, so log lines can leave up to 500 ms late.
- The `web` and `ntp` loops still sleep in short steps while they wait (rare).

## NAND follow-ups
touchos boots from NAND with two copies (A/B), saves settings there, and takes updates from the settings page.

- **Bad blocks that appear later:** a failed program or erase isn't marked bad yet; the write just fails. Mark the block (as U-Boot and Linux do) and move on to the next good one.
- **Faster updates:** an update takes about 45 s, mostly the upload over Wi-Fi (see "Wi-Fi follow-ups").

## USB
The OTG port runs as a host with read-only USB disks; failing devices are detached and enumerated again with a back-off.

- **Hot-plug with a real unplug** on the OTG port: the code path exists, but has only been exercised with a simulated failure (`test_fail=usb`).
- **Writing to a USB disk:** the mass-storage code has no write command. A use: a USB disk as an update source.
- **Interrupt endpoints on the EHCI:** for keyboards and other HID devices.

## UI animations
Smooth transitions in the dashboard: pressing a tile, switching rooms, the dimming of the screen, the power menu appearing.

- **Today:** software rendering; each frame redraws the whole canvas and copies it to the framebuffer. On the Toon a frame takes about 67 ms, about 110 ms with Wi-Fi up.
- **Steps:**
  1. Draw only what changed (dirty rectangles), and copy only those parts to the framebuffer.
  2. A frame clock in the dashboard: animations by time (e.g. 150–250 ms with easing), with frames only while something moves.
  3. The first animations: the tile press (scale/colour), the room switch (slide), the screen dimming (fade).
  4. Measure frame times (`ui: frame in N ms` in the log) and keep a budget.

## Boot logo
From power-on until the dashboard's first frame, the screen shows the stock U-Boot splash: a BMP in NAND at `0x180000`, an area touchos never writes.

- **Open:** the BMP format U-Boot 2010.09 accepts, how it finds the image, and what it does with a broken one.
- **Steps:** dump and document the stock splash; `cargo xtask splash <png>` (with `--dump`/`--restore`) writing a narrow, explicit splash region and nothing else below `0x300000`; later an upload on the settings page ([#26](https://github.com/mactijn/touchos/issues/26)).

## Audio
Sound for the panel: alerts, a doorbell chime, button feedback. One `audio` server.

- **Hardware:** a TLV320AIC3111 codec on I2C2 (`0x18`) plus the SSI (`0x10010000`).
- **Open:**
  - The codec only answers once PB25/PF13 are high (set for USB host 2), so one of those pins probably also releases it from reset. Which one?
  - Stock Linux binds no driver to the codec, so there is no set-up to copy: it has to come from the datasheet.
  - The speaker amplifier's enable pin is unknown.
- **Steps:**
  1. The `audio` server: codec set-up over I²C, the SSI with DMA, one test tone.
  2. Play short sounds (WAV/PCM from the initrd) on request: "play" events from the dashboard and Home Assistant.
  3. Volume in the settings, and a mute during night hours.

## Smaller hardware items
- **The SPI EEPROM:** declared at `spi0.0` (CS0 = PD28), but nothing answers there, so it is probably not fitted. Look at the board for an 8-pin chip marked 25640; if one is there, find what cuts it off.
- **PE18:** touchos sets it high for the external USB port, but the stock kernel says it is CS1 of `spi0`, the Z-Wave chip's programming port. Check whether the USB port works without it.
- **The debug LEDs:** `leds-gpio` on GPIO port F (from `ed20.cfg`); their function is unknown.
- **The LCD's contrast PWM** (LPCCR) stays at U-Boot's value; it could be a setting.

## Kernel limits on the ARM926
The FCSE keeps process switches from flushing the caches, at a price:

- A process's heap is at most about 19 MiB (its memory lies below 32 MiB). None comes close yet.
- FCSE pids run out at 31 user spaces; after that a space falls back to flushing on every switch.
