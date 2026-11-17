# Settings and using the panel

## Using the panel

- **Tap** a tile to toggle it: lights, switches, fans, covers, locks, play/pause, scenes and scripts.
- **Drag sideways** on a dimmable light to set its brightness. It is sent when you let go.
- **Thermostats:** use −/+ to change the target temperature by one step.
- **Rooms:** the buttons under the clock are your Home Assistant areas. Tap one to show that room. Hidden entities and config/diagnostic entities are left out.
- **Swipe sideways** outside a light tile to change pages within a room (the dots show the page).
- The screen dims after `dim_after_seconds` without touches, and switches off during `night` hours. A touch on a dark screen only wakes it.

## The settings page

Open `http://<panel-ip>/` in a browser on your network. The panel logs its address at boot (`web: settings page at ...`).

- **Pairing.** Click "Show a code on the panel" and type the 6-digit code from the panel's screen. The browser then stays signed in for `web.session_hours`. Wrong codes lock pairing for a while. A restart of the panel signs every browser out.
- **Settings.** Every setting below, with its help text.
  - Display, dashboard and time changes apply at once.
  - Home Assistant and Wi-Fi changes make those connections restart.
  - The access token and the Wi-Fi password are never shown. Type a new one to replace it; leave the field empty to keep it.
  - Edits in any section are kept until you save or discard them: a bar at the bottom counts them and saves them all at once. Changed sections are marked in the side bar, invalid ones with an error mark.
  - Placeholders in italics are the defaults; an empty field uses the default.
- **Tiles.** Tiles can be added, dragged into order and removed. The entity field suggests Home Assistant's entities by id or name; an empty name uses Home Assistant's.
- **Software.** Installs a signed update (`cargo xtask bundle`) into the copy of the system that isn't running; see [the quick start](quick-start-toon.md#9-updates).
- **System.** Status, the log and a restart. After a restart the page waits until the panel answers again.
- **Plain HTTP.** Anyone on the network can read what you send to the page.

When the panel saves a change, it rewrites only the changed lines of `touchos.toml`, so comments stay. The previous version is kept as a backup. The file lives in the NAND's config ring: each save is a new record, and a power cut while saving leaves the previous one.

Saving can be turned off on the kernel command line (`storage=`):

| `storage=` | Effect |
|---|---|
| `rw` (default) | Changes are saved |
| `ro` | Changes apply until the next restart; a save is a logged dry run (the default for `selftest` boots) |
| `off` | Storage isn't used |

## `touchos.toml`

`config/touchos.toml` on your PC (from `config/touchos.example.toml`) is what `cargo xtask flash --config` writes into the Toon. After that, the copy in the Toon is the one that counts. Change it on the settings page; a later `flash --config` replaces it.

| Setting | Default | What |
|---|---|---|
| `homeassistant.host` | | IP address (or DNS name) of Home Assistant. The panel can't resolve `.local` names. |
| `homeassistant.port` | `8123` | Usually 8123, or 443 behind a TLS proxy. |
| `homeassistant.token` | | A long-lived access token: in Home Assistant, Profile → Security. |
| `homeassistant.tls` | TLS on port 443 only | Connect with TLS (`wss://`). The certificate must chain to a root in Mozilla's root store (`libs/certs/cacerts.txt`), as a browser's would. |
| `wifi.ssid` | | The Wi-Fi network. Only WPA2-PSK with AES is supported. Without it, the panel uses Ethernet. |
| `wifi.psk` | | The Wi-Fi password (8-63 characters). |
| `wifi.ht` | `true` | Connect as an 802.11n (and WMM) device where the access point is one: faster, and needed for an access point in "n only" mode. Off: as an 802.11g device, as touchos did before. |
| `display.brightness` | `80` | Backlight in percent while the panel is in use. |
| `display.dim_brightness` | `15` | Backlight in percent after a while without touches. |
| `display.dim_after_seconds` | `60` | How long without touches before the screen dims. |
| `display.night` | | Switch the screen off when idle during these hours, e.g. `"23:00-07:00"`. A touch wakes it. |
| `display.rotation` | `"auto"` | `"0"`, `"90"`, `"180"`, `"270"`; `"auto"` follows an accelerometer where the board has one (the Toon has none). |
| `display.ui` | `"on"` | `"off"` keeps the text console on screen (for debugging). |
| `time.zone` | `"UTC"` | Where the panel is: an IANA time zone such as `"Europe/Amsterdam"` or `"America/New_York"`, summer time included. A POSIX TZ rule (`"CET-1CEST,M3.5.0,M10.5.0/3"`) also works, for a zone whose rules changed after the build. Replaces `display.utc_offset_minutes` and `display.dst`, which are no longer read. |
| `time.ntp_server` | `"time.google.com"` | Used when the network's own time server (from DHCP) doesn't answer, or there is none. |
| `dashboard.types` | lights, switches, climate, covers, fans, locks, media players, sensors | Which kinds of entities get tiles when no tiles are listed. |
| `dashboard.rooms` | all | Which Home Assistant areas to show, in this order. `"Other"` = entities without an area. |
| `debug.log_host` | | Send the log as UDP lines to `IP:port` (`cargo xtask log` receives them). |
| `debug.log_pin` | `false` | Also write each pairing code to the log, so `cargo xtask pair` can pair without looking at the screen. The log travels over the network unencrypted: for development only. |
| `debug.http_test` | | Fetch this URL once after boot and log the result. |
| `web.enabled` | `true` | Serve the settings page. |
| `web.port` | `80` | TCP port of the settings page. |
| `web.session_hours` | `168` | How long a paired browser stays signed in. |
| `[[tile]]` `entity`, `name` | automatic tiles | Dashboard tiles in this order: a Home Assistant entity id (`light.kitchen`) and an optional label. |

An example:

```toml
[homeassistant]
host = "192.168.1.10"
token = "..."

[wifi]
ssid = "MyNetwork"
psk = "..."

[display]
dim_brightness = 10
night = "23:00-07:00"

[dashboard]
rooms = ["Living room", "Kitchen", "Other"]

[[tile]]
entity = "light.kitchen"
name = "Kitchen"
```
