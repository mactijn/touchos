# touchos: roadmap

Ideas and planned work, roughly in order. Hardware details: [docs/hardware-toon.md](docs/hardware-toon.md).

## The boiler link (OpenTherm)
The Toon's actual job as a thermostat: talk to the boiler through the Ketelmodule, on UART2 (`ttymxc1`, `0x1000b000`, IRQ 19).

- **Today:** `servers/boiler` listens on UART2, receive-only (the transmitter is never enabled, its pin not touched). It logs bytes and line errors per rate (4800 first, the stock thermostat's rate, then other rates when most characters have errors), the first bytes in hex, and OpenTherm frames if the lines are in the OpenTherm Gateway's text format. `libs/opentherm` parses and checks frames and decodes the data IDs a boiler's state is read from.
- **Open:** what the Ketelmodule actually sends: rate, framing, and how OpenTherm frames are wrapped.
- **Steps:**
  1. With a boiler behind the Ketelmodule: record what arrives, and decode it.
  2. The boiler's state (flow and return temperature, modulation, flame, pressure) as Home Assistant sensors.
  3. Sending (setpoints) only after reading works well, and with care: it controls the heating.

## Z-Wave: the meter adapter's readings
The ZW0301 controller runs on UART3 at 115200 (`servers/zwave`, `libs/zwave`). It can make a network of its own, pair a device (`zwave=pair`, `reset-pair`, `include` on the boot command line), ping, and ask Manufacturer Specific and Version. What's missing is the data.

- **Why:** an Eneco meter adapter on the electricity and gas meters is the Toon's other main source of data.
- **Open:** what the adapter sends unprompted once it's on a meter (Meter Table Monitor/Push, Meter, possibly CRC-16 encapsulated). Stock `hdrv_zwave` has debug info and is the reference.
- **Steps:**
  1. Decode the adapter's reports, and its gas and electricity endpoints (Multi Channel).
  2. Publish them as Home Assistant sensors.
  3. Pairing from the settings page instead of a boot switch. Untested so far: the 5 s wait before asking a new node.
  4. Later: other device types (Switch Binary, Sensor Multilevel) and security (S0/S2).

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
- The Wi-Fi chip drops multicast while joined (DROP_MC). IPv6 or mDNS would need it back, with a multicast filter.
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
- **The ADT7410 temperature** (I2C1, `0x49`) is read but only logged: show it on the dashboard or report it to Home Assistant.
- **The SPI EEPROM:** declared at `spi0.0` (CS0 = PD28), but nothing answers there, so it is probably not fitted. Look at the board for an 8-pin chip marked 25640; if one is there, find what cuts it off.
- **PE18:** touchos sets it high for the external USB port, but the stock kernel says it is CS1 of `spi0`, the Z-Wave chip's programming port. Check whether the USB port works without it.
- **The debug LEDs:** `leds-gpio` on GPIO port F (from `ed20.cfg`); their function is unknown.
- **The LCD's contrast PWM** (LPCCR) stays at U-Boot's value; it could be a setting.

## Kernel limits on the ARM926
The FCSE keeps process switches from flushing the caches, at a price:

- A process's heap is at most about 19 MiB (its memory lies below 32 MiB). None comes close yet.
- FCSE pids run out at 31 user spaces; after that a space falls back to flushing on every switch.
