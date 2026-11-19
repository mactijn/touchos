//! The chip: bring-up (firmware, calibration, MAC/BBP/RF), channels, the
//! station set-up, sending and receiving. (On touchos only: it talks to
//! the chip over USB.)

use alloc::vec::Vec;
use core::fmt;

use touchos_rt::{exec, time};
use touchos_usb::{Device, HostController, UsbError};
use touchos_wlan::Mac;
use touchos_wlan::rates::Rate;

use crate::io::{Io, delay};
use crate::regs::*;
use crate::rom::Calib;
use crate::rxframe::{Frame, Stats};
use crate::tx::TxStats;
use crate::{fw, rom, rx, tx};

#[derive(Debug, Clone, Copy)]
pub enum Error {
    /// A vendor request (request, wIndex) failed after retries.
    Usb(u8, u16, UsbError),
    Timeout(&'static str),
    Firmware(&'static str),
    Chip(&'static str),
    /// A MAC/BBP this driver has no init path for (the chip id).
    Unsupported(u16),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Usb(req, idx, e) => write!(f, "vendor request {req} (index {idx:#06x}): {e}"),
            Error::Timeout(what) => write!(f, "timeout: {what}"),
            Error::Firmware(what) => write!(f, "firmware: {what}"),
            Error::Chip(what) => write!(f, "{what}"),
            Error::Unsupported(v) => write!(f, "MAC/BBP RT{v:04X} is not supported (only the RT5390 family)"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The chip, initialised and receiving.
pub struct Rt2800 {
    /// From MAC_CSR0 (ASIC_VER_ID): 0x5390 for the RT5370.
    pub mac_ver: u16,
    pub mac_rev: u16,
    pub cal: Calib,
    /// The channel the radio is on.
    pub channel: u8,
    rx: rx::Rx,
    /// The transmit side; None when brought up receive only.
    tx: Option<tx::Tx>,
    /// The BSS joined ([`Rt2800::join`]).
    bss: Option<Join>,
    /// The next frame's sequence number (see `tx::Tx::send`).
    seq: u16,
    /// The access point's pairwise key is in the chip
    /// ([`Rt2800::set_pairwise_key`]).
    pairwise_key: bool,
    /// We are the access point ([`Rt2800::start_ap`]).
    ap: bool,
}

/// What [`Rt2800::join`] needs to know about the BSS.
#[derive(Clone, Copy, Debug)]
pub struct Join {
    pub bssid: [u8; 6],
    pub channel: u8,
    /// In TU (1024 µs).
    pub beacon_interval: u16,
    /// The BSS allows short preambles (capability bit 5): our CCK frames
    /// and the chip's ACKs use them.
    pub short_preamble: bool,
    /// The BSS uses the short slot time (capability bit 10).
    pub short_slot: bool,
    /// The BSS has OFDM rates (802.11g), not only CCK (802.11b).
    pub ofdm: bool,
    /// 802.11b stations are about (the ERP element's Use_Protection).
    pub erp_protection: bool,
    /// Joining as an 802.11n station.
    pub ht: Option<HtJoin>,
}

/// What the chip needs for HT frames to the BSS.
#[derive(Clone, Copy, Debug)]
pub struct HtJoin {
    /// Send with the short guard interval (the access point receives it).
    pub sgi: bool,
    /// The HT Operation element's protection mode (0-3).
    pub protection: u8,
}

/// The chip's TX statistics since they were last read (the registers
/// clear when read).
#[derive(Default, Clone, Copy, Debug)]
pub struct TxCounters {
    /// Frames sent and ACKed (or not needing an ACK).
    pub ok: u32,
    /// Frames given up on: no ACK after all retries.
    pub failed: u32,
    /// Retransmissions.
    pub retries: u32,
    /// Beacons sent (as an access point).
    pub beacons: u32,
}

impl TxCounters {
    /// Wrapping: a running total since bring-up wraps after years.
    pub fn add(&mut self, o: TxCounters) {
        self.ok = self.ok.wrapping_add(o.ok);
        self.failed = self.failed.wrapping_add(o.failed);
        self.retries = self.retries.wrapping_add(o.retries);
        self.beacons = self.beacons.wrapping_add(o.beacons);
    }
}

/// How long `leave` waits for TX transfers in flight (a 1.5 KiB frame
/// takes well under a millisecond over high-speed USB).
const DRAIN_MS: u64 = 200;

/// Milliseconds since `t`, for the init log.
fn since(t: u64) -> u64 {
    time::now_ms() - t
}

/// The receive filter of a station: errors, control frames, duplicates
/// and unicast frames for other stations dropped (OpenBSD's). Beacons of
/// every network still arrive (for the scan).
const STATION_FILTER: u32 = DROP_CRC_ERR
    | DROP_PHY_ERR
    | DROP_UC_NOME
    | DROP_DUPL
    | DROP_CTS
    | DROP_BA
    | DROP_ACK
    | DROP_VER_ERR
    | DROP_CTRL_RSV
    | DROP_CFACK
    | DROP_CFEND
    | DROP_RTS
    | DROP_PSPOLL;

/// Joined: frames of other networks are dropped too (their beacons are
/// most of what arrives: on the Toon 83 wake-ups a second, 25 of them
/// ours). Multicast passes: the chip can only take all of it or none, and
/// mDNS needs 224.0.0.251; the station drops the other groups
/// (`Hdr::wanted_group`).
const JOINED_FILTER: u32 = STATION_FILTER | DROP_NOT_MY_BSSD;

/// An access point (the setup network): errors, control frames,
/// duplicates and unicast frames for others dropped. Not
/// DROP_NOT_MY_BSSD: probe requests name the wildcard BSSID.
const AP_FILTER: u32 = STATION_FILTER;

/// Receive only: everything but errors and control frames, so beacons and
/// probe responses of every network arrive (probe responses to other
/// stations too: they name hidden networks).
const RECEIVE_ONLY_FILTER: u32 = DROP_CRC_ERR
    | DROP_PHY_ERR
    | DROP_VER_ERR
    | DROP_DUPL
    | DROP_CTS
    | DROP_ACK
    | DROP_RTS
    | DROP_PSPOLL
    | DROP_BA
    | DROP_BAR
    | DROP_CFACK
    | DROP_CFEND
    | DROP_CTRL_RSV;

impl Rt2800 {
    /// Bring the chip up and start receiving on channel 1 (`run_attach`
    /// and `run_init`): chip id, ROM, firmware, MAC, BBP, RF, channel,
    /// RX, and TX with `transmit`. `fw` is linux-firmware's `rt3071.bin`.
    pub async fn init<H: HostController>(hc: &mut H, dev: &mut Device<H>, fw: &[u8], transmit: bool) -> Result<Rt2800> {
        let t0 = time::now_ms();
        // The RX endpoint: the interface's bulk IN (0x81). TX: the first
        // bulk OUT (0x01), the one OpenBSD sends management frames and
        // best-effort data on (its `txq[EDCA_AC_BE]`; it takes 0x02-0x04
        // for the other WMM queues, 0x05-0x06 stay unused). Receive only,
        // no OUT endpoint is configured: nothing can be sent on one.
        let bulk = |want_in: bool| {
            dev.config.as_ref().and_then(|c| {
                c.interfaces
                    .iter()
                    .flat_map(|i| i.endpoints.iter())
                    .find(|e| e.transfer_type() == 2 && e.is_in() == want_in)
                    .copied()
            })
        };
        let ep_in = bulk(true).ok_or(Error::Chip("no bulk IN endpoint"))?;
        let ep_out = match transmit {
            true => Some(bulk(false).ok_or(Error::Chip("no bulk OUT endpoint"))?),
            false => None,
        };
        let eps: Vec<_> = core::iter::once(ep_in).chain(ep_out).collect();
        dev.configure_endpoints(hc, &eps).await.map_err(|e| Error::Usb(0, 0, e))?;
        let mut io = Io::new(hc, dev);

        // The chip id (run_attach): wait until it reads sensibly.
        let mut ver = 0;
        for _ in 0..100 {
            ver = io.read(ASIC_VER_ID).await?;
            if ver != 0 && ver != 0xffff_ffff {
                break;
            }
            delay(10).await;
        }
        if ver == 0 || ver == 0xffff_ffff {
            return Err(Error::Timeout("chip id (MAC_CSR0)"));
        }
        let (mac_ver, mac_rev) = ((ver >> 16) as u16, ver as u16);
        ::log::info!("rt2800: MAC/BBP RT{mac_ver:04X} rev {mac_rev:#06x} (MAC_CSR0 {ver:#010x})");
        if mac_ver != 0x5390 {
            return Err(Error::Unsupported(mac_ver));
        }

        // The ROM (run_read_eeprom).
        let t = time::now_ms();
        // rt2800usb asks whether the chip runs its firmware from ROM
        // ("autorun", DEVICE_MODE IN 0x11) and then skips the upload. The
        // Toon's module never answers that request (~5 s of timeouts with
        // the retries), and OpenBSD doesn't ask: always upload.
        let autorun = false;
        let mut rom = rom::Rom::detect(&mut io, autorun).await?;
        let cal = Calib::read(&mut io, &mut rom).await?;
        ::log::info!(
            "rt2800: {} v{}.{}, RF{:04X} {}T{}R, address {}, freq offset {:#04x}, LNA {:#04x}, RSSI offsets {:?}, TX power {:?}{} ({} ms)",
            if cal.efuse { "efuse" } else { "EEPROM" },
            cal.version & 0xff,
            cal.version >> 8,
            cal.rf_rev,
            cal.ntx,
            cal.nrx,
            Mac(&cal.mac),
            cal.freq,
            cal.lna0,
            cal.rssi_2ghz,
            cal.txpow1,
            if cal.ext_2ghz_lna { ", external LNA" } else { "" },
            since(t)
        );
        if cal.rf_rev != RF_5370 && cal.rf_rev != RF_5390 {
            ::log::warn!("rt2800: RF {:#06x} is not an RF5370; using the RF5370 path anyway", cal.rf_rev);
        }

        // The firmware (run_init -> run_load_microcode).
        let t = time::now_ms();
        let l = fw::load(&mut io, fw, autorun).await?;
        ::log::info!(
            "rt2800: firmware: {}, MCU ready after {} ms ({} ms)",
            match (l.autorun, l.verified, l.fallback) {
                (true, _, _) => "autorun (in ROM), nothing uploaded",
                (false, Some(true), false) => "CRC ok, uploaded, read back ok",
                (false, Some(true), true) => "CRC ok, uploaded with 16-bit writes, read back ok",
                (false, None, _) => "CRC ok, uploaded, not read back",
                (false, Some(false), _) => "CRC ok, uploaded, READ BACK DIFFERS",
            },
            l.ready_ms,
            since(t)
        );

        let tx = ep_out.map(|e| tx::Tx::new(e.address, e.max_packet));
        let mut chip = Rt2800 { mac_ver, mac_rev, cal, channel: 0, rx: rx::Rx::new(ep_in.address), tx, bss: None, seq: 0, pairwise_key: false, ap: false };
        let t = time::now_ms();
        chip.init_mac(&mut io).await?;
        ::log::info!("rt2800: MAC set up ({} ms)", since(t));
        let t = time::now_ms();
        chip.init_bbp(&mut io).await?;
        ::log::info!("rt2800: BBP set up ({} ms)", since(t));
        let t = time::now_ms();
        chip.init_rf(&mut io).await?;
        ::log::info!("rt2800: RF set up ({} ms)", since(t));
        let t = time::now_ms();
        chip.set_chan(&mut io, 1).await?;
        ::log::info!("rt2800: channel 1 ({} ms)", since(t));
        // Radio LED on (run_set_leds).
        io.mcu_cmd(MCU_CMD_LEDS, LED_RADIO | (chip.cal.leds & 0x7f) as u16).await?;
        chip.rx.start(io.hc, io.dev);
        chip.enable_txrx(&mut io).await?;
        let mac_sys = io.read(MAC_SYS_CTRL).await?;
        let on = |b: bool| if b { "on" } else { "off" };
        if let Some(ep) = chip.tx.as_ref().map(|t| t.ep) {
            // Read back what makes the chip transmit, for the log.
            let dma = io.read(WPDMA_GLO_CFG).await?;
            let pins = io.read(TX_PIN_CFG).await?;
            let rsp = io.read(AUTO_RSP_CFG).await?;
            ::log::info!(
                "rt2800: receiving and transmitting (MAC_SYS_CTRL {mac_sys:#x}: RX {}, TX {}; TX DMA {}; PA {} (TX_PIN_CFG {pins:#x}); ACK responder {} (AUTO_RSP_CFG {rsp:#x}); frames out on endpoint {ep:#04x}); init took {} ms, {} control transfers",
                on(mac_sys & MAC_RX_EN != 0),
                on(mac_sys & MAC_TX_EN != 0),
                on(dma & TX_DMA_EN != 0),
                on(pins & PA_PE_G0_EN != 0),
                on(rsp & AUTO_RSP_EN != 0),
                since(t0),
                io.transfers
            );
        } else {
            ::log::info!(
                "rt2800: receiving (MAC_SYS_CTRL {mac_sys:#x}: RX {}, TX {}); init took {} ms, {} control transfers",
                on(mac_sys & MAC_RX_EN != 0),
                if mac_sys & MAC_TX_EN != 0 { "ON?!" } else { "off" },
                since(t0),
                io.transfers
            );
            if mac_sys & MAC_TX_EN != 0 {
                // Can't happen (nothing sets it receive only); make sure anyway.
                io.write(MAC_SYS_CTRL, mac_sys & !MAC_TX_EN).await?;
            }
        }
        Ok(chip)
    }

    /// The MAC (the first half of `run_init`, up to the BBP).
    async fn init_mac<H: HostController>(&mut self, io: &mut Io<'_, H>) -> Result<()> {
        let m = &self.cal.mac;
        io.write(MAC_ADDR_DW0, u32::from_le_bytes([m[0], m[1], m[2], m[3]])).await?;
        io.write(MAC_ADDR_DW1, m[4] as u32 | (m[5] as u32) << 8 | 0xff << 16).await?;

        let v = io.wait(WPDMA_GLO_CFG, TX_DMA_BUSY | RX_DMA_BUSY, 0, 100, 1000, "DMA idle").await?;
        io.write(WPDMA_GLO_CFG, (v & 0xff0) | TX_WB_DDONE).await?;
        // PME_OEN off: a high-current issue.
        let v = io.read(SYS_CTRL).await?;
        io.write(SYS_CTRL, v & !PME_OEN).await?;

        io.write(MAC_SYS_CTRL, BBP_HRST | MAC_SRST).await?;
        io.write(USB_DMA_CFG, 0).await?;
        io.device_mode(MODE_RESET).await?;
        io.write(MAC_SYS_CTRL, 0).await?;

        for (r, &p) in self.cal.txpow20mhz.iter().enumerate() {
            if p != 0xffff_ffff {
                io.write(tx_pwr_cfg(r as u16), p).await?;
            }
        }
        for &(reg, val) in DEF_MAC {
            io.write(reg, val).await?;
        }
        if self.tx.is_none() {
            // Receive only: no automatic ACK/CTS (not in OpenBSD; the MAC's
            // TX is off anyway). To transmit, DEF_MAC's value stays: the
            // responder on, so the chip ACKs the frames addressed to us.
            io.write(AUTO_RSP_CFG, 0).await?;
        }
        io.write(WMM_AIFSN_CFG, 0x00002273).await?;
        io.write(WMM_CWMIN_CFG, 0x00002344).await?;
        io.write(WMM_CWMAX_CFG, 0x000034aa).await?;
        // RT5390: PA_PE assertion delay 1 µs (units of 0.25 µs).
        io.write(TX_SW_CFG0, 4 << DLY_PAPE_EN_SHIFT | 4).await?;

        io.wait(MAC_STATUS_REG, RX_STATUS_BUSY | TX_STATUS_BUSY, 0, 100, 1000, "MAC idle").await?;
        // Clear the host-to-MCU mailbox.
        io.write(H2M_BBPAGENT, 0).await?;
        io.write(H2M_MAILBOX, 0).await?;
        delay(1000).await;
        Ok(())
    }

    /// The BBP (`run_bbp_init`, `run_rt5390_bbp_init`), then the rest of
    /// `run_init` up to the RF.
    async fn init_bbp<H: HostController>(&mut self, io: &mut Io<'_, H>) -> Result<()> {
        let mut awake = false;
        for _ in 0..20 {
            let b = io.bbp_read(0).await?;
            if b != 0 && b != 0xff {
                awake = true;
                break;
            }
        }
        if !awake {
            return Err(Error::Timeout("BBP wake-up"));
        }
        // run_rt5390_bbp_init
        let b = io.bbp_read(105).await?;
        if self.cal.nrx > 1 {
            io.bbp_write(105, b | BBP105_MLD).await?;
        }
        let b = io.bbp_read(4).await?;
        io.bbp_write(4, b | BBP4_MAC_IF_CTRL).await?; // avoids data loss and CRC errors
        for &(r, v) in RT5390_DEF_BBP {
            io.bbp_write(r, v).await?;
        }
        let b = io.bbp_read(152).await?;
        io.bbp_write(152, b | 0x80).await?;
        io.bbp_write(154, 0).await?; // no hardware antenna diversity
        io.bbp_write(142, 1).await?; // the RX CCK/OFDM frequency offset report
        io.bbp_write(143, 57).await?;
        // back in run_bbp_init: RT3070 and later
        io.bbp_write(79, 0x13).await?;
        io.bbp_write(80, 0x05).await?;
        io.bbp_write(81, 0x33).await?;

        // run_init: TSF sync off, the key tables cleared.
        let v = io.read(BCN_TIME_CFG).await?;
        io.write(BCN_TIME_CFG, v & !(BCN_TX_EN | TSF_TIMER_EN | TBTT_TIMER_EN)).await?;
        io.zero_region(WCID_ENTRY0, 512).await?; // RX WCID search table (256 × 8 bytes)
        io.zero_region(WCID_ATTR0, 8 * 32).await?;
        io.zero_region(SKEY0, 8 * 32).await?;
        io.zero_region(SKEY_MODE_0_7, 4).await?;
        let v = io.read(US_CYC_CNT).await?;
        io.write(US_CYC_CNT, (v & !0xff) | 0x1e).await?;
        if self.mac_rev != 0x0101 {
            io.write(TXOP_CTRL_CFG, 0x0000583f).await?;
        }
        io.write(WMM_TXOP0_CFG, 0).await?;
        io.write(WMM_TXOP1_CFG, 48 << 16 | 96).await?;
        // The vendor's BBP values from the ROM (OpenBSD writes the first 8).
        for &(r, v) in &self.cal.bbp[..8] {
            if r != 0 && r != 0xff {
                io.bbp_write(r, v).await?;
            }
        }
        // The main antenna (run_set_rx_antenna(sc, 0), RF5370: BBP 152).
        let b = io.bbp_read(152).await?;
        io.bbp_write(152, b | 0x80).await?;
        // The LEDs' modes to the MCU.
        for (i, &l) in self.cal.led.iter().enumerate() {
            io.mcu_cmd(MCU_CMD_LED1 + i as u8, l).await?;
        }
        Ok(())
    }

    /// The RF (`run_rt5390_rf_init`), the chains, `run_rt5390_rf_setup`.
    async fn init_rf<H: HostController>(&mut self, io: &mut Io<'_, H>) -> Result<()> {
        let rev = self.mac_rev;
        // Toggle RF R2's resistor calibration.
        let rf = io.rf_read(2).await?;
        io.rf_write(2, rf | RF2_RESCAL).await?;
        delay(10).await;
        io.rf_write(2, rf & !RF2_RESCAL).await?;
        for &(r, v) in RT5390_DEF_RF {
            io.rf_write(r, v).await?;
        }
        if rev >= 0x0502 {
            // RT5390F and later
            for (r, v) in [(6, 0xe0), (25, 0x80), (46, 0x73), (53, 0x00), (56, 0x42), (61, 0xd1)] {
                io.rf_write(r, v).await?;
            }
        }
        if rev < 0x0211 {
            io.rf_write(27, 0x3).await?;
        }
        let v = io.read(OPT_14).await?;
        io.write(OPT_14, v | 1).await?;

        // Disable the RX and TX chains the module doesn't have.
        let mut b3 = io.bbp_read(3).await? & !(1 << 3 | 1 << 4);
        if self.cal.nrx == 2 {
            b3 |= 1 << 3;
        }
        io.bbp_write(3, b3).await?;
        let mut b1 = io.bbp_read(1).await?;
        if self.cal.ntx == 1 {
            b1 &= !(1 << 3 | 1 << 4);
        }
        io.bbp_write(1, b1).await?;

        // run_rt5390_rf_setup
        if rev >= 0x0211 {
            io.bbp_write(103, 0xc0).await?; // DC filter
            let b = io.bbp_read(31).await?;
            io.bbp_write(31, b & !0x03).await?; // power consumption
        }
        let mut b = io.bbp_read(138).await?;
        if self.cal.ntx == 1 {
            b |= 0x20; // DAC1 off
        }
        if self.cal.nrx == 1 {
            b &= !0x02; // ADC1 off
        }
        io.bbp_write(138, b).await?;
        let rf = io.rf_read(38).await?;
        io.rf_write(38, rf & !RF38_RX_LO1).await?;
        let rf = io.rf_read(39).await?;
        io.rf_write(39, rf & !RF39_RX_LO2).await?;
        let b = io.bbp_read(4).await?;
        io.bbp_write(4, b | BBP4_MAC_IF_CTRL).await?;
        let rf = io.rf_read(30).await?;
        io.rf_write(30, (rf & !0x18) | 0x10).await?;
        io.write(TX_SW_CFG1, 0).await?;
        let cfg2 = if rev < 0x0211 { if self.cal.patch_dac { 0x2c } else { 0x0f } } else { 0 };
        io.write(TX_SW_CFG2, cfg2).await?;
        Ok(())
    }

    /// Tune to a 2.4 GHz channel (1-14): `run_set_chan` with
    /// `run_rt5390_set_chan` and `run_select_chan_group(sc, 0)`.
    async fn set_chan<H: HostController>(&mut self, io: &mut Io<'_, H>, chan: u8) -> Result<()> {
        if !(1..=14).contains(&chan) {
            return Err(Error::Chip("not a 2.4 GHz channel"));
        }
        let i = chan as usize - 1;
        let (n, r, k) = FREQS_2GHZ[i];
        io.rf_write(8, n).await?;
        io.rf_write(9, k & 0x0f).await?;
        let rf = io.rf_read(11).await?;
        io.rf_write(11, (rf & !0x03) | (r & 0x03)).await?;
        // TX power (RF R49, at most 0x27).
        let rf = io.rf_read(49).await?;
        let p = (self.cal.txpow1[i] as u8 & 0x3f).min(0x27);
        io.rf_write(49, (rf & !0x3f) | p).await?;
        let rf = io.rf_read(1).await?;
        io.rf_write(1, rf | RF1_RF_BLOCK | RF1_PLL_PD | RF1_RX0_PD | RF1_TX0_PD).await?;
        let rf = io.rf_read(2).await?;
        io.rf_write(2, rf | 0x80).await?;
        delay(10).await;
        io.rf_write(2, rf & 0x7f).await?;
        // The crystal's frequency offset, through the MCU (run_adjust_freq_offset).
        let old = io.rf_read(17).await?;
        let new = ((old & !0x7f) | (self.cal.freq & 0x7f)).min(0x5f);
        if new != old {
            io.mcu_cmd(MCU_CMD_FREQ_OFFSET, (old as u16) << 8 | new as u16).await?;
        }
        if self.mac_rev >= 0x0502 {
            // RT5390F
            io.rf_write(55, if chan <= 11 { 0x43 } else { 0x23 }).await?;
            io.rf_write(59, if chan <= 11 { 0x0f } else if chan == 12 { 0x0d } else { 0x0b }).await?;
        } else {
            io.rf_write(55, 0x44).await?;
            io.rf_write(59, 0x8f).await?;
        }
        let rf = io.rf_read(3).await?;
        io.rf_write(3, rf | RF3_VCOCAL).await?;

        // run_select_chan_group(sc, 0): the 2.4 GHz band.
        let lna = self.cal.lna0;
        for r in [62, 63, 64] {
            io.bbp_write(r, 0x37u8.wrapping_sub(lna)).await?;
        }
        io.bbp_write(75, if self.cal.ext_2ghz_lna { 0x52 } else { 0x50 }).await?;
        let v = io.read(TX_BAND_CFG).await?;
        io.write(TX_BAND_CFG, (v & !(BAND_SEL_5G_N | BAND_SEL_5G_P)) | BAND_SEL_5G_N).await?;
        let mut pins = RFTR_EN | TRSW_EN | LNA_PE_G0_EN | LNA_PE_A0_EN;
        if self.cal.nrx > 1 {
            pins |= LNA_PE_G1_EN | LNA_PE_A1_EN;
        }
        if self.tx.is_some() {
            pins |= PA_PE_G0_EN;
            if self.cal.ntx > 1 {
                pins |= PA_PE_G1_EN;
            }
        }
        io.write(TX_PIN_CFG, pins).await?;
        // The initial AGC gain (BBP 66).
        io.bbp_write(66, 0x1cu8.wrapping_add(lna.wrapping_mul(2))).await?;
        delay(1000).await;
        self.channel = chan;
        Ok(())
    }

    /// Start receiving, and transmitting unless receive only
    /// (`run_txrx_enable`): DMA, USB bulk in (with aggregation) and out,
    /// the RX filter, MAC_SYS_CTRL.
    async fn enable_txrx<H: HostController>(&mut self, io: &mut Io<'_, H>) -> Result<()> {
        let transmit = self.tx.is_some();
        // OpenBSD has MAC_SYS_CTRL = TX_EN while it waits for the DMA.
        io.write(MAC_SYS_CTRL, if transmit { MAC_TX_EN } else { 0 }).await?;
        let v = io.wait(WPDMA_GLO_CFG, TX_DMA_BUSY | RX_DMA_BUSY, 0, 200, 1000, "DMA idle").await?;
        delay(50).await;
        let tx_dma = if transmit { TX_DMA_EN } else { 0 };
        io.write(WPDMA_GLO_CFG, v | RX_DMA_EN | tx_dma | TX_WB_DDONE).await?;
        // Bulk-in aggregation: a 128 × 33 ns timeout, at most 2 KiB.
        let usb_tx = if transmit { USB_TX_EN } else { 0 };
        io.write(USB_DMA_CFG, usb_tx | USB_RX_EN | USB_RX_AGG_EN | usb_rx_agg_to(128) | usb_rx_agg_lmt(2)).await?;
        let filter = if transmit { STATION_FILTER } else { RECEIVE_ONLY_FILTER };
        io.write(RX_FILTR_CFG, filter).await?;
        // Receive only, MAC_TX_EN (bit 2) stays 0.
        io.write(MAC_SYS_CTRL, MAC_RX_EN | if transmit { MAC_TX_EN } else { 0 }).await
    }

    /// Tune to `chan` (1-14).
    pub async fn set_channel<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, chan: u8) -> Result<()> {
        let mut io = Io::new(hc, dev);
        self.set_chan(&mut io, chan).await
    }

    /// Handle received frames: `f` gets each good one with its signal in
    /// dBm. Call often (every few ms); the transfers are queued again.
    pub fn poll<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, mut f: impl FnMut(&Frame, i8)) -> usize {
        let cal = &self.cal;
        self.rx.poll(hc, dev, |fr| f(fr, rssi_dbm(cal, fr.rssi)))
    }

    pub fn stats(&self) -> Stats {
        self.rx.stats
    }

    /// The driver's TX counters (None: receive only).
    pub fn tx_stats(&self) -> Option<TxStats> {
        self.tx.as_ref().map(|t| t.stats)
    }

    /// The BSS joined, if any.
    pub fn joined(&self) -> Option<&Join> {
        self.bss.as_ref()
    }

    /// Get ready to talk to a BSS, as OpenBSD's `run_newstate_cb` does on
    /// the way to RUN: the channel, the BSSID (`run_set_bssid`), the slot
    /// time (`run_updateslot`), the ACK preamble (`run_set_txpreamble`),
    /// the basic rates for ACKs (`run_set_basicrates`) and the rate
    /// fallback on retries (`run_enable_mrr`). Done before authentication
    /// (OpenBSD does it after association; Linux sets the BSSID before
    /// authenticating too). Also clears the TX statistics.
    pub async fn join<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, j: &Join) -> Result<()> {
        let mut io = Io::new(hc, dev);
        self.set_chan(&mut io, j.channel).await?;
        let b = &j.bssid;
        io.write(MAC_BSSID_DW0, u32::from_le_bytes([b[0], b[1], b[2], b[3]])).await?;
        io.write(MAC_BSSID_DW1, b[4] as u32 | (b[5] as u32) << 8).await?;
        io.write(RX_FILTR_CFG, JOINED_FILTER).await?;
        // 802.11g: 1, 2, 5.5, 11, 6, 12, 24 Mbit/s; 802.11b: 1, 2.
        link_cfg(&mut io, j.short_slot, j.short_preamble, if j.ofdm { 0x15f } else { 0x003 }).await?;
        // HT protection (Linux's rt2800_config_ht_opmode, for 20 MHz):
        // RTS/CTS before HT frames while the access point says non-HT
        // stations are about (non-member or mixed mode), at OFDM 24 Mbit/s,
        // or at CCK 11 Mbit/s when 802.11b stations are too. Greenfield
        // (never sent) is protected alike.
        let protect = j.ht.is_some_and(|h| matches!(h.protection, 1 | 3));
        let rate = if j.erp_protection { 0x0003 } else { 0x4004 };
        for reg in [MM20_PROT_CFG, GF20_PROT_CFG] {
            let v = io.read(reg).await? & !(PROT_RATE_MASK | PROT_CTRL_MASK);
            io.write(reg, v | rate | if protect { PROT_CTRL_RTS_CTS } else { 0 }).await?;
        }
        read_tx_counters(&mut io).await?;
        self.bss = Some(*j);
        Ok(())
    }

    /// Become an open 802.11g access point on `chan` with our MAC as the
    /// BSSID, after OpenBSD's rt2860.c (`rt2860_setup_beacon`,
    /// `rt2860_enable_tsf_sync` for HOSTAP; run(4) has no access point
    /// mode): the beacon in slot 0, read back to check it, then the TSF
    /// and TBTT timers with beacons on, syncing with nobody. Short slot,
    /// long preamble; 1, 2, 5.5 and 11 Mbit/s basic (what `ap::Ap`
    /// advertises). [`Rt2800::leave`] ends it.
    pub async fn start_ap<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, chan: u8, beacon: &[u8]) -> Result<()> {
        let slot = crate::txwi::beacon(beacon).ok_or(Error::Chip("beacon too long"))?;
        let mut io = Io::new(hc, dev);
        self.set_chan(&mut io, chan).await?;
        let b = self.cal.mac;
        io.write(MAC_BSSID_DW0, u32::from_le_bytes([b[0], b[1], b[2], b[3]])).await?;
        io.write(MAC_BSSID_DW1, b[4] as u32 | (b[5] as u32) << 8).await?;
        io.write(RX_FILTR_CFG, AP_FILTER).await?;
        link_cfg(&mut io, true, false, 0x00f).await?;
        for reg in [MM20_PROT_CFG, GF20_PROT_CFG] {
            let v = io.read(reg).await? & !(PROT_RATE_MASK | PROT_CTRL_MASK);
            io.write(reg, v | 0x4004).await?;
        }
        // Beacons off while the slot is written.
        let v = io.read(BCN_TIME_CFG).await?;
        io.write(BCN_TIME_CFG, v & !(BCN_TX_EN | TSF_TIMER_EN | TBTT_TIMER_EN)).await?;
        io.zero_region(BCN_BASE0, BCN_SIZE / 4).await?;
        io.write_region(BCN_BASE0, &slot).await?;
        let mut back = alloc::vec![0u8; slot.len()];
        io.read_region(BCN_BASE0, &mut back).await?;
        if back != slot {
            ::log::warn!("rt2800: beacon read-back differs; writing it again with 16-bit writes");
            io.write_region_2(BCN_BASE0, &slot).await?;
            io.read_region(BCN_BASE0, &mut back).await?;
            if back != slot {
                return Err(Error::Chip("the beacon doesn't stay in the chip"));
            }
        }
        let interval = (touchos_wlan::ap::BEACON_INTERVAL as u32 * 16) & BCN_INTVAL_MASK;
        let v = io.read(BCN_TIME_CFG).await?;
        io.write(
            BCN_TIME_CFG,
            (v & !0x1f_ffff) | interval | TSF_TIMER_EN | TBTT_TIMER_EN | TSF_SYNC_MODE_HOSTAP | BCN_TX_EN,
        )
        .await?;
        io.mcu_cmd(MCU_CMD_LEDS, LED_RADIO | LED_LINK_2GHZ | (self.cal.leds & 0x7f) as u16).await?;
        read_tx_counters(&mut io).await?;
        self.ap = true;
        Ok(())
    }

    /// The access point is up ([`Rt2800::start_ap`]).
    pub fn is_ap(&self) -> bool {
        self.ap
    }

    /// Associated: synchronise the TSF with the BSS's beacons
    /// (`run_enable_tsf_sync`) and turn the link LED on.
    pub async fn associated<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> Result<()> {
        let Some(bss) = self.bss else { return Err(Error::Chip("not joined")) };
        let mut io = Io::new(hc, dev);
        let v = io.read(BCN_TIME_CFG).await?;
        let interval = (bss.beacon_interval as u32 * 16) & BCN_INTVAL_MASK;
        io.write(BCN_TIME_CFG, (v & !0x1f_ffff) | interval | TSF_TIMER_EN | TBTT_TIMER_EN | TSF_SYNC_MODE_STA).await?;
        io.mcu_cmd(MCU_CMD_LEDS, LED_RADIO | LED_LINK_2GHZ | (self.cal.leds & 0x7f) as u16).await
    }

    /// Put the access point's pairwise key (CCMP) into the chip for
    /// sending, after OpenBSD's `run_set_key_cb` for a unicast key: the key
    /// in PKEY and the entry's cipher in WCID_ATTR, for entry [`WCID_AP`],
    /// which TXWI names on every frame the chip encrypts. Each such frame
    /// brings its own CCMP header (see `tx::Tx::send`), so the IVEIV table
    /// isn't used.
    ///
    /// Receiving stays the host's: the access point's address is *not*
    /// put in the station table (WCID_ENTRY), which is how the chip finds
    /// the key for a received frame. With it there, the chip decrypts the
    /// access point's unicast frames and strips their CCMP header, packet
    /// number and all (seen on the Toon), and there is no
    /// replay check without it.
    pub async fn set_pairwise_key<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, tk: &[u8; 16]) -> Result<()> {
        if self.bss.is_none() {
            return Err(Error::Chip("not joined"));
        }
        let w = WCID_AP as u16;
        let mut io = Io::new(hc, dev);
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(tk);
        io.write_region_2(PKEY0 + w * 32, &key).await?;
        io.write(WCID_ATTR0 + w * 4, MODE_AES_CCMP << 1 | WCID_ATTR_PKEY).await?;
        self.pairwise_key = true;
        Ok(())
    }

    /// Take the pairwise key out again (leaving, or a new handshake).
    async fn clear_pairwise_key<H: HostController>(&mut self, io: &mut Io<'_, H>) -> Result<()> {
        self.pairwise_key = false;
        let w = WCID_AP as u16;
        io.write(WCID_ATTR0 + w * 4, 0).await?;
        io.write_region_2(PKEY0 + w * 32, &[0; 32]).await
    }

    /// Leave the BSS, back to the state after `init`, ready to scan and
    /// join again: the frames still in flight over USB are let go first
    /// (see `drain_tx`), then TSF sync off, no BSSID, the link LED off, the
    /// TX statistics cleared. The rest needs nothing: the RX filter is the
    /// same for scanning and for a BSS (the station's, set once in
    /// `enable_txrx`), the pairwise key is taken out (group keys never go
    /// into the chip), and `join` sets the slot time, preamble and rates
    /// again.
    pub async fn leave<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> Result<()> {
        // Forgotten first, so a failure below can't leave us half joined.
        self.bss = None;
        self.ap = false;
        self.drain_tx(hc, dev).await;
        let mut io = Io::new(hc, dev);
        if self.pairwise_key {
            self.clear_pairwise_key(&mut io).await?;
        }
        let v = io.read(BCN_TIME_CFG).await?;
        io.write(BCN_TIME_CFG, v & !(BCN_TX_EN | TSF_TIMER_EN | TBTT_TIMER_EN)).await?;
        io.write(MAC_BSSID_DW0, 0).await?;
        io.write(MAC_BSSID_DW1, 0).await?;
        io.write(RX_FILTR_CFG, STATION_FILTER).await?;
        io.mcu_cmd(MCU_CMD_LEDS, LED_RADIO | (self.cal.leds & 0x7f) as u16).await?;
        read_tx_counters(&mut io).await?;
        Ok(())
    }

    /// Wait (briefly) for the TX transfers still in flight to reach the
    /// chip, so none is left queued in the controller across the channel
    /// changes of the next scan. Frames the chip has taken go out to the
    /// old BSS or are given up on after their retries; either is harmless.
    /// A transfer that stays stuck is `tx_poll`'s to report.
    async fn drain_tx<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        let Some(tx) = self.tx.as_mut() else { return };
        let end = time::now_ms() + DRAIN_MS;
        loop {
            if let Some(e) = tx.reap(hc, dev) {
                log_tx_error(tx, e);
            }
            if tx.in_flight() == 0 || time::now_ms() >= end {
                break;
            }
            exec::sleep_ms(1).await;
        }
        if tx.in_flight() > 0 {
            ::log::warn!("rt2800: leaving with {} TX transfers still in flight", tx.in_flight());
        }
    }

    /// Send an 802.11 frame (header and body, no FCS) at `rate` (an HT
    /// rate only once joined as an HT station), waiting while all TX
    /// buffers are in flight. `ack`: it is unicast, the chip waits for the
    /// ACK and retries. We number it (see `txwi::write`): one counter for
    /// every frame, QoS data included (802.11 has one per TID for QoS
    /// data, but without block-ack agreements a receiver only compares a
    /// frame with the one before it from us). `ccmp`: the chip encrypts
    /// it with the pairwise key ([`Rt2800::set_pairwise_key`]) and this
    /// CCMP header.
    #[allow(clippy::too_many_arguments)]
    pub async fn send<H: HostController>(
        &mut self,
        hc: &mut H,
        dev: &mut Device<H>,
        frame: &[u8],
        rate: Rate,
        ack: bool,
        ccmp: Option<[u8; 8]>,
    ) -> Result<()> {
        if ccmp.is_some() && !self.pairwise_key {
            return Err(Error::Chip("no pairwise key in the chip"));
        }
        let short_preamble = self.bss.is_some_and(|b| b.short_preamble);
        let sgi = self.bss.and_then(|b| b.ht).is_some_and(|h| h.sgi);
        let seq = self.seq;
        let Some(tx) = self.tx.as_mut() else { return Err(Error::Chip("brought up receive only")) };
        loop {
            if let Some(e) = tx.reap(hc, dev) {
                log_tx_error(tx, e);
            }
            if tx.stalled {
                clear_halt(hc, dev, tx).await;
            }
            match tx.send(hc, dev, frame, seq, rate, short_preamble, sgi, ack, ccmp) {
                Ok(()) => break,
                Err(tx::TxError::Full) => {
                    if tx.oldest_ms().is_some_and(|ms| ms > tx::TX_TIMEOUT_MS) {
                        return Err(Error::Timeout("TX transfer (the chip takes no frames)"));
                    }
                    exec::sleep_ms(1).await;
                }
                Err(tx::TxError::TooLong) => return Err(Error::Chip("frame too long to send")),
                Err(tx::TxError::Rate) => return Err(Error::Chip("no such TX rate")),
            }
        }
        self.seq = (seq + 1) & 0xfff;
        Ok(())
    }

    /// Check the TX transfers in flight (call now and then while not
    /// sending, so they don't stay pending). Returns false if one has
    /// been stuck for longer than `tx::TX_TIMEOUT_MS`.
    pub fn tx_poll<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> bool {
        let Some(tx) = self.tx.as_mut() else { return true };
        if let Some(e) = tx.reap(hc, dev) {
            log_tx_error(tx, e); // a stall is cleared on the next send
        }
        !tx.oldest_ms().is_some_and(|ms| ms > tx::TX_TIMEOUT_MS)
    }

    /// The chip's TX statistics since the last call (or `join`).
    pub async fn tx_counters<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> Result<TxCounters> {
        read_tx_counters(&mut Io::new(hc, dev)).await
    }

}

/// TX_STA_CNT0..2, which clear when read (`run_calibrate_cb`).
/// What a BSS needs of the MAC, ours or one we join: the slot time
/// (`run_updateslot`), the preamble of our ACKs (`run_set_txpreamble`),
/// the basic rates the chip answers at (`run_set_basicrates`, a bitmap of
/// 1, 2, 5.5, 11, 6, 9, 12, 18, 24... Mbit/s), and the rate fallback on
/// retries (`run_enable_mrr`).
async fn link_cfg<H: HostController>(io: &mut Io<'_, H>, short_slot: bool, short_preamble: bool, basic: u32) -> Result<()> {
    let v = io.read(BKOFF_SLOT_CFG).await?;
    io.write(BKOFF_SLOT_CFG, (v & !SLOT_TIME_MASK) | if short_slot { 9 } else { 20 }).await?;
    let v = io.read(AUTO_RSP_CFG).await?;
    io.write(AUTO_RSP_CFG, if short_preamble { v | CCK_SHORT_EN } else { v & !CCK_SHORT_EN }).await?;
    io.write(LEGACY_BASIC_RATE, basic).await?;
    // On retries the chip steps down a rate each time (TX_RTY_CFG has
    // the automatic fallback on): OFDM 54 -> 48 -> ... -> 6, CCK
    // 11 -> 5.5 -> 2 -> 1. Entries: bit 3 = OFDM, low bits the index.
    let ofdm = |i: u32| 8 | i;
    io.write(
        LG_FBK_CFG0,
        ofdm(6) << 28 | ofdm(5) << 24 | ofdm(4) << 20 | ofdm(3) << 16 | ofdm(2) << 12 | ofdm(1) << 8 | ofdm(0) << 4 | ofdm(0),
    )
    .await?;
    io.write(LG_FBK_CFG1, 2 << 12 | 1 << 8).await?;
    // HT: MCS n -> n - 1 down to MCS 0 (Linux's values; MCS 8-15 for
    // completeness, one stream has none).
    io.write(HT_FBK_CFG0, 0x6543_2100).await?;
    io.write(HT_FBK_CFG1, 0xedcb_a988).await?;
    Ok(())
}

async fn read_tx_counters<H: HostController>(io: &mut Io<'_, H>) -> Result<TxCounters> {
    let mut b = [0u8; 12];
    io.read_region(TX_STA_CNT0, &mut b).await?;
    let w = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    Ok(TxCounters { ok: w(1) & 0xffff, failed: w(0) & 0xffff, retries: w(1) >> 16, beacons: w(0) >> 16 })
}

/// A transmit transfer failed (`run_txeof`): log the first few.
fn log_tx_error(tx: &tx::Tx, e: UsbError) {
    if tx.stats.usb_errors <= 5 || tx.stats.usb_errors % 100 == 0 {
        ::log::warn!("rt2800: transmit transfer failed: {e} ({} so far)", tx.stats.usb_errors);
    }
}

/// The TX endpoint stalled: clear the halt on the chip's side
/// (CLEAR_FEATURE(ENDPOINT_HALT)), as OpenBSD's `run_txeof` does; the
/// controller resets its side when the next transfer is queued.
async fn clear_halt<H: HostController>(hc: &mut H, dev: &mut Device<H>, tx: &mut tx::Tx) {
    tx.stalled = false;
    if let Err(e) = dev.control_out(hc, 0x02, 1, 0, tx.ep as u16).await {
        ::log::warn!("rt2800: clearing the TX endpoint's halt failed: {e}");
    }
}

/// The RXWI RSSI in dBm, 2.4 GHz: -12 - RSSI offset - LNA gain - RSSI.
/// (OpenBSD's `run_rssi2dbm` adds the LNA gain instead; Linux and
/// Ralink's own driver subtract it, which is what a gain should do.)
pub fn rssi_dbm(cal: &Calib, raw: u8) -> i8 {
    if raw == 0 {
        return -128; // no measurement
    }
    let v = -12 - cal.rssi_2ghz[0] as i32 - cal.lna0 as i32 - raw as i8 as i32;
    v.clamp(-128, 0) as i8
}
