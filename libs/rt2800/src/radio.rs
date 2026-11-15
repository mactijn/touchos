//! The RT5370 as the station's `Radio`: a soft-MAC. The host scans
//! (passively, `host_scan`) and fills the duration field; the chip tunes,
//! sends frames at the rate we pick, ACKs frames for us and retries ours.
//!
//! CCMP ([`KeyMode::HardwareTx`]): the chip encrypts the unicast data
//! frames we send, with the pairwise key in its tables and the station's
//! packet number (`Rt2800::set_pairwise_key`). Received frames are
//! decrypted by the host. The chip could do that too, but it then strips
//! the CCMP header and MIC (seen on the Toon: the body starts with the
//! LLC header), so the packet number is gone and with it the
//! replay check; Linux's rt2800 lives without one, we don't. Group frames
//! carry the group key, which never goes into the chip.
//!
//! Rates (see touchos_wlan::rates): management frames and the
//! unprotected EAPOL frames of the handshake at the BSS's lowest basic
//! rate (1 Mbit/s on most 2.4 GHz networks), as mac80211 sends them, so
//! they reach the access point wherever its beacons reach us. Data starts
//! at 24 Mbit/s (or the fastest OFDM rate below it the access point has);
//! from there AMRR moves it between the access point's OFDM rates by the
//! chip's TX statistics, as run(4) does. Within a frame's retries the
//! chip steps down by itself (`Rt2800::join` sets the fallback).

use ::log::{debug, info, warn};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use touchos_rt::irq::Irq;
use touchos_rt::{exec, time};
use touchos_usb::{Device, HostController};

use crate::regs::RXD_DEC;
use crate::{Join, Rt2800, TxCounters};
use touchos_wlan::amrr::{Amrr, Step};
use touchos_wlan::radio::{Caps, KeyMode, Radio, RxCrypto, RxFrame, TxKey};
use touchos_wlan::rates::{self, Mbit};
use touchos_wlan::scan::{CHANNELS_2GHZ, Network, host_scan};
use touchos_wlan::{Error, Mac, Result};


/// Listening time per channel in the scan (beacons come every ~102 ms).
const DWELL_MS: u64 = 110;
/// How long to wait for a management frame's TX result.
const MGMT_RESULT_MS: u64 = 50;
/// How often to read the TX statistics for the rate control while
/// sending data (run(4) reads them every second).
const AMRR_EVERY_MS: u64 = 1000;
/// How often to sum them up in the log.
const STATS_EVERY_MS: u64 = 30_000;

/// The chip on its USB host controller `H` (the Toon's EHCI).
pub struct Rt5370<H: HostController> {
    hc: H,
    dev: Device<H>,
    chip: Rt2800,
    mgmt_rate: u8,
    /// The data rate, chosen from the TX statistics.
    amrr: Amrr,
    next_tick: u64,
    dead: bool,
    /// The chip's TX statistics since the join.
    since_join: TxCounters,
    /// What became of the last management frame (None: no result).
    last_mgmt: Option<TxCounters>,
    /// The statistics window while sending data, and when it ends (0:
    /// no data sent since the join).
    window: Window,
    window_end: u64,
    /// The first window after the join is logged however it went.
    first_window: bool,
    /// The controller's interrupt (else `recv` is polled).
    irq: Option<Irq>,
}

impl<H: HostController> Rt5370<H> {
    pub fn new(hc: H, dev: Device<H>, chip: Rt2800) -> Rt5370<H> {
        Rt5370 {
            hc,
            dev,
            chip,
            mgmt_rate: 2,
            amrr: Amrr::new(rates::data_rates(&[]), 2),
            next_tick: 0,
            dead: false,
            since_join: TxCounters::default(),
            last_mgmt: None,
            window: Window::new(2),
            window_end: 0,
            first_window: true,
            irq: None,
        }
    }

    /// Wake the station through the controller's interrupt (see
    /// `Radio::event_bit`).
    pub fn set_irq(&mut self, irq: Irq) {
        self.hc.enable_interrupts();
        self.irq = Some(irq);
    }

    /// Completed transfers: sent frames reaped, received ones of our BSS.
    fn recv_polled(&mut self) -> Vec<RxFrame> {
        if !self.chip.tx_poll(&mut self.hc, &mut self.dev) && !self.dead {
            warn!("rt2800: a transmit transfer has been stuck for 5 s; giving up on the chip");
            self.dead = true;
        }
        // Joined: only frames of our BSS (from it, or to it: a group
        // frame relayed by the access point has the BSSID as address 2).
        let bssid = self.chip.joined().map(|j| j.bssid);
        let channel = self.chip.channel;
        let mut out = Vec::new();
        self.chip.poll(&mut self.hc, &mut self.dev, |f, dbm| {
            let d = f.data;
            if let Some(b) = bssid
                && (d.len() < 22 || (d[10..16] != b && d[16..22] != b))
            {
                return;
            }
            // The chip doesn't decrypt (see `Rt2800::set_pairwise_key`): a
            // frame it did anyway has lost its CCMP header, and with it
            // the replay check.
            if f.flags & RXD_DEC != 0 {
                return;
            }
            out.push(RxFrame { data: d.to_vec(), rssi: dbm, channel, crypto: RxCrypto::None });
        });
        out
    }

    /// An error from the chip: log it, and give up on the chip if it no
    /// longer answers (the station then stops, and we start over).
    fn fail(&mut self, e: crate::Error) -> Error {
        warn!("rt2800: {e}");
        if matches!(e, crate::Error::Usb(..) | crate::Error::Timeout(..)) {
            self.dead = true;
        }
        Error::Hw("RT5370 failed")
    }

    async fn send(&mut self, frame: &[u8], rate: u8, ccmp: Option<[u8; 8]>) -> Result<()> {
        // Unicast (all we send: to the access point) waits for an ACK.
        let ack = frame.len() >= 10 && frame[4] & 1 == 0;
        let r = self.chip.send(&mut self.hc, &mut self.dev, frame, rate, ack, ccmp).await;
        r.map_err(|e| self.fail(e))
    }

    async fn counters(&mut self) -> Option<TxCounters> {
        let r = self.chip.tx_counters(&mut self.hc, &mut self.dev).await;
        let c = r.map_err(|e| self.fail(e)).ok()?;
        self.since_join.add(c);
        self.window.tx.add(c);
        Some(c)
    }

    /// Wait for the management frame just queued to be sent or given up
    /// on, from the TX statistics (the access point's answer, if it comes,
    /// waits in the receive queue meanwhile).
    async fn mgmt_result(&mut self, subtype: u8) {
        let end = time::now_ms() + MGMT_RESULT_MS;
        let mut got = TxCounters::default();
        while time::now_ms() < end {
            exec::sleep_ms(5).await;
            let Some(c) = self.counters().await else { return };
            got.add(c);
            if got.ok + got.failed > 0 {
                break;
            }
        }
        debug!(
            "rt2800: management frame {subtype:#04x}: {} ({} retries)",
            match (got.ok, got.failed) {
                (0, 0) => "no TX result yet",
                (_, 0) => "ACKed",
                _ => "NOT ACKed",
            },
            got.retries
        );
        self.last_mgmt = Some(got);
    }

    /// While sending data, about once a second (as run(4)'s
    /// `run_calibrate_cb`): the chip's TX statistics drive the rate
    /// control, and every [`STATS_EVERY_MS`] a summary goes to the log.
    async fn tx_tick(&mut self) {
        let now = time::now_ms();
        if now < self.next_tick {
            return;
        }
        self.next_tick = now + AMRR_EVERY_MS;
        let Some(c) = self.counters().await else { return };
        if self.window_end == 0 {
            // The first data frame since the join: what the counters held
            // were the handshake's frames, at the management rate.
            self.new_window(now);
            return;
        }
        self.amrr.add(c.ok, c.failed, c.retries);
        let (tx, retries) = self.amrr.counts();
        let old = self.amrr.rate();
        if let Some(step) = self.amrr.choose() {
            let new = self.amrr.rate();
            debug!("rt2800: data rate {} -> {} Mbit/s ({retries} retries in {tx} transmissions)", Mbit(old), Mbit(new));
            self.window.note(step, new);
        }
        if now >= self.window_end {
            self.log_window();
            self.new_window(now);
        }
    }

    fn new_window(&mut self, now: u64) {
        self.window = Window::new(self.amrr.rate());
        self.window_end = now + STATS_EVERY_MS;
    }

    /// The statistics window that ended: logged when frames got no ACK,
    /// when the rate moved, and the first one after the join.
    fn log_window(&mut self) {
        let w = &self.window;
        let usb = self.chip.tx_stats().map_or(0, |s| s.usb_errors);
        let line = format!(
            "rt2800: TX at {} Mbit/s, last {} s: {} ok, {} failed (no ACK), {} retries, rate {}-{} Mbit/s ({} up, {} down); {} USB errors so far",
            Mbit(self.amrr.rate()),
            STATS_EVERY_MS / 1000,
            w.tx.ok,
            w.tx.failed,
            w.tx.retries,
            Mbit(w.slowest),
            Mbit(w.fastest),
            w.ups,
            w.downs,
            usb
        );
        if w.tx.failed > 0 {
            warn!("{line}");
        } else if self.first_window || w.ups + w.downs > 0 {
            info!("{line}");
        }
        self.first_window = false;
    }
}

/// The CCMP header (PN, key id, ExtIV) for a frame the chip encrypts.
fn ccmp_header(pn: u64, key_id: u8) -> [u8; 8] {
    let p = pn.to_le_bytes();
    [p[0], p[1], 0, 0x20 | key_id << 6, p[2], p[3], p[4], p[5]]
}

/// The TX statistics of one log period.
struct Window {
    tx: TxCounters,
    /// The data rates used, and the steps between them.
    slowest: u8,
    fastest: u8,
    ups: u32,
    downs: u32,
}

impl Window {
    fn new(rate: u8) -> Window {
        Window { tx: TxCounters::default(), slowest: rate, fastest: rate, ups: 0, downs: 0 }
    }

    fn note(&mut self, step: Step, rate: u8) {
        match step {
            Step::Up => self.ups += 1,
            Step::Down => self.downs += 1,
        }
        self.slowest = self.slowest.min(rate);
        self.fastest = self.fastest.max(rate);
    }
}

impl<H: HostController> Radio for Rt5370<H> {
    fn mac(&self) -> [u8; 6] {
        self.chip.cal.mac
    }

    fn caps(&self) -> Caps {
        Caps { keys: KeyMode::HardwareTx, host_duration: true }
    }

    fn dead(&self) -> bool {
        self.dead
    }

    async fn scan(&mut self) -> Result<Vec<Network>> {
        let t = time::now_ms();
        let nets = host_scan(self, &CHANNELS_2GHZ, DWELL_MS).await?;
        let s = self.chip.stats();
        info!(
            "rt2800: scan of channels 1-13: {} networks in {} ms ({} frames, {} CRC errors, {} USB errors so far)",
            nets.len(),
            time::now_ms() - t,
            s.frames,
            s.crc_errors,
            s.usb_errors
        );
        for n in &nets {
            debug!("rt2800: {} {} ch {} {} dBm {}", n.ssid, Mac(&n.bssid), n.channel, n.rssi, n.security);
        }
        Ok(nets)
    }

    async fn set_channel(&mut self, channel: u8) -> Result<()> {
        let r = self.chip.set_channel(&mut self.hc, &mut self.dev, channel).await;
        r.map_err(|e| self.fail(e))
    }

    async fn join(&mut self, bss: &Network) -> Result<()> {
        let j = Join {
            bssid: bss.bssid,
            channel: bss.channel,
            beacon_interval: bss.beacon_interval,
            short_preamble: bss.capability & 0x20 != 0,
            short_slot: bss.capability & 0x400 != 0,
            ofdm: bss.rates.iter().any(|&r| rates::is_ofdm(r)),
        };
        // The station computes durations with the same management rate.
        self.mgmt_rate = rates::lowest_basic(&bss.rates, bss.channel);
        let data = rates::data_rates(&bss.rates);
        let (slowest, fastest) = (data[0], data[data.len() - 1]);
        self.amrr = Amrr::new(data, rates::data_rate(&bss.rates));
        let r = self.chip.join(&mut self.hc, &mut self.dev, &j).await;
        r.map_err(|e| self.fail(e))?;
        self.since_join = TxCounters::default();
        self.last_mgmt = None;
        self.window_end = 0;
        self.next_tick = 0;
        self.first_window = true;
        info!(
            "rt2800: joining {} on channel {}: management frames at {} Mbit/s, data at {} Mbit/s (rate control {}-{}), {} preamble, {} slot",
            Mac(&j.bssid),
            j.channel,
            Mbit(self.mgmt_rate),
            Mbit(self.amrr.rate()),
            Mbit(slowest),
            Mbit(fastest),
            if j.short_preamble { "short" } else { "long" },
            if j.short_slot { "short" } else { "long" }
        );
        Ok(())
    }

    async fn associated(&mut self, _aid: u16) -> Result<()> {
        let r = self.chip.associated(&mut self.hc, &mut self.dev).await;
        r.map_err(|e| self.fail(e))?;
        info!("rt2800: TSF synchronised to the access point's beacons, link LED on");
        Ok(())
    }

    async fn send_mgmt(&mut self, frame: &[u8]) -> Result<()> {
        self.send(frame, self.mgmt_rate, None).await?;
        self.mgmt_result(frame[0]).await;
        Ok(())
    }

    async fn send_data(&mut self, frame: &[u8], key: Option<TxKey<'_>>) -> Result<()> {
        // Unprotected data frames are the handshake's EAPOL frames: at the
        // management rate, as mac80211 sends them.
        let protected = key.is_some() || (frame.len() > 1 && frame[1] & 0x40 != 0);
        let rate = if protected { self.amrr.rate() } else { self.mgmt_rate };
        let ccmp = key.map(|k| ccmp_header(k.pn, k.key_id));
        self.send(frame, rate, ccmp).await?;
        if protected {
            self.tx_tick().await;
        }
        Ok(())
    }

    fn event_bit(&self) -> usize {
        self.irq.as_ref().map_or(0, Irq::bit)
    }

    fn recv(&mut self) -> Vec<RxFrame> {
        // Status first, then the transfers: one that completes after this
        // look interrupts again.
        if self.irq.is_some() {
            self.hc.clear_interrupts();
        }
        let out = self.recv_polled();
        if let Some(i) = &self.irq {
            i.ack();
        }
        out
    }

    async fn install_ptk(&mut self, tk: &[u8; 16]) -> Result<()> {
        // Encrypting only: see the module's notes on receiving.
        let r = self.chip.set_pairwise_key(&mut self.hc, &mut self.dev, tk).await;
        r.map_err(|e| self.fail(e))?;
        info!("rt2800: pairwise key in the chip: it encrypts what we send");
        Ok(())
    }

    fn tx_report(&self) -> String {
        let last = match self.last_mgmt {
            None => String::from("no result"),
            Some(c) if c.ok + c.failed == 0 => format!("no result after {MGMT_RESULT_MS} ms ({} retries so far)", c.retries),
            Some(c) if c.failed == 0 => format!("ACKed after {} retries", c.retries),
            Some(c) => format!("NOT ACKed ({} retries)", c.retries),
        };
        let s = self.since_join;
        let t = self.chip.tx_stats().unwrap_or_default();
        format!(
            "last frame: {last}; since joining: {} ok, {} failed, {} retries; {} frames queued, {} USB errors",
            s.ok, s.failed, s.retries, t.frames, t.usb_errors
        )
    }

    /// Leave the BSS (see `Rt2800::leave`) and drop what was received
    /// meanwhile: the next scan starts from an empty queue.
    async fn reset(&mut self) -> Result<()> {
        let left = self.chip.joined().map(|j| j.bssid);
        let r = self.chip.leave(&mut self.hc, &mut self.dev).await;
        r.map_err(|e| self.fail(e))?;
        let dropped = self.recv().len();
        if let Some(b) = left {
            info!("rt2800: left {}, ready to scan ({dropped} received frames dropped)", Mac(&b));
        }
        Ok(())
    }
}
