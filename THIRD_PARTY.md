# Third-party material

touchos itself is under the MIT license (see LICENSE). It contains, or was ported from, the following.

## OpenBSD iwm(4): the Wi-Fi station

`libs/wlan/` (the chip-independent station: the association sequence and the WPA2 handshake) is a Rust port of parts of OpenBSD's `sys/dev/pci/if_iwm.c`, `if_iwmreg.h` and `if_iwmvar.h`. The original notice:

```
Copyright (c) 2014, 2016 genua gmbh <info@genua.de>
  Author: Stefan Sperling <stsp@openbsd.org>
Copyright (c) 2014 Fixup Software Ltd.
Copyright (c) 2017 Stefan Sperling <stsp@openbsd.org>

Permission to use, copy, modify, and distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

OpenBSD's driver is in turn based on BSD-licensed modules of Linux's iwlwifi.

## OpenBSD run(4): the Toon's RT5370 Wi-Fi driver

`libs/rt2800/` is a Rust port of parts of OpenBSD's `sys/dev/usb/if_run.c`, `sys/dev/usb/if_runvar.h` and `sys/dev/ic/rt2860reg.h`. It covers the vendor-request register access, the firmware upload, the efuse/EEPROM reading, the MAC/BBP/RF initialisation and channel switching for the RT5390/RF5370, the receive and transmit paths (`run_tx`, `run_txrx_enable`) and the station set-up (`run_set_bssid`, `run_updateslot`, `run_set_txpreamble`, `run_set_basicrates`, `run_enable_mrr`, `run_enable_tsf_sync`, the TX statistics). Linux's rt2x00 (GPL-2.0) was used only to check facts (register numbers, RT5390 values, the firmware CRC, the TXINFO WIV bit, the chip's sequence-number bug); none of its code was copied. The original notice:

```
Copyright (c) 2008-2010 Damien Bergamini <damien.bergamini@free.fr>
Copyright (c) 2013-2014 Kevin Lo

Permission to use, copy, modify, and distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

`rt2860reg.h` carries the same notice under "Copyright (c) 2007 Damien Bergamini". `libs/wlan/src/amrr.rs` (the RT5370's rate control) is a port of OpenBSD's `sys/net80211/ieee80211_amrr.c`, under the same notice with "Copyright (c) 2006 Damien Bergamini".

## Fonts (`servers/ui/fonts/`)

- **Noto Sans** (Regular, Bold): SIL Open Font License 1.1. See `servers/ui/fonts/LICENSE-NotoSans.txt`.
- **Font Awesome 4** (webfont): SIL Open Font License 1.1. See `servers/ui/fonts/LICENSE-FontAwesome.txt`.

They are rasterized at build time; the atlases are embedded in the UI server.

## CA certificates (`libs/certs/cacerts.der`)

The root certificates TLS trusts: Mozilla's CA root store, as Ubuntu's `ca-certificates` package ships it (the version is in the header of `libs/certs/cacerts.txt`). Mozilla publishes the store (`certdata.txt`) under the Mozilla Public License 2.0; the certificates themselves are public. `cargo xtask cacerts` makes the file from `/etc/ssl/certs/ca-certificates.crt`. It keeps the roots that are valid and that TLS here can use, and stores them unmodified as DER. `libs/certs/tests/chains/` holds public certificate chains of a few web sites, as test data.

## Not in this repository

- **Ralink RT2800 USB firmware** `rt3071.bin` (from linux-firmware; the same bytes as the upper 4 KiB of `rt2870.bin`). Copyright (c) 2007 Ralink Technology Corporation; linux-firmware's `LICENCE.ralink-firmware.txt` allows redistribution of the unmodified binary with its notice and disclaimer. It is kept out of git; see docs/quick-start-toon.md. The Toon build puts it into the initrd.

Rust crate dependencies are listed in `Cargo.lock`, under their own licenses.

## ToonRooter: the Toon's OpenOCD board config (`tools/toon/ed20.cfg`)

`tools/toon/ed20.cfg` is ToonRooter's `assets/boards/ed20.cfg` (https://github.com/martenjacobs/ToonRooter, commit 71c1037), ported to OpenOCD 0.12 with a few additions. ToonRooter ships it under the MIT license; see `tools/toon/LICENSE-ToonRooter.txt` (Copyright (c) 2018 Marten Jacobs). Its own header says it derives from OpenOCD's `imx27lnst.cfg` and Quby's U-Boot `lowlevel_init.S`, which are GPL-2.0. It is a separate OpenOCD script, used only on the debug host and not built into touchos; treat it under those terms, not under touchos's MIT license.

The U-Boot passwords `tools/toon/toonconsole.py` types to stop the Toon's autoboot are public through ToonRooter.

## Trademarks

touchos is not affiliated with Eneco, Quby, Prodrive or Home Assistant. Their names are used only to say which hardware and software it works with.
