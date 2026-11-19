//! Station mode: find the configured network, join it (open authentication,
//! association, the WPA2 4-way handshake) and pump data frames between the
//! radio and the network stack. The association sequence follows OpenBSD's iwm
//! (ISC license, see THIRD_PARTY.md); the chip-specific steps are behind
//! [`Radio`].

use ::log::{debug, info, warn};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use touchos_drv::link;
use touchos_rt::{exec, time};

use crate::backoff::Backoff;
use crate::ieee80211::{self as w, Hdr};
use crate::keys::{self, Handshake, Keys, RxKeys};
use crate::radio::{Counters, KeyMode, Radio, RxFrame, TxKey};
use crate::scan::{self, Network};
use crate::{Error, Result, ccmp, rates, wpa};

/// Run the WPA2/CCMP test vectors and log the result.
pub fn log_self_test() -> bool {
    let t0 = time::now_ms();
    let ok = crate::self_test();
    info!(
        "WPA2 crypto self-test {} ({} ms)",
        if ok { "passed" } else { "FAILED" },
        time::now_ms() - t0
    );
    ok
}

/// Encrypt 64 KiB in 1500-byte frames and log the rate. On a radio that
/// leaves CCMP to us (a soft-MAC) every frame goes through it, so this
/// bounds the link.
fn log_ccmp_speed() {
    const TOTAL: usize = 64 * 1024;
    const FRAME: usize = 1500;
    let t0 = time::now_us();
    let n = ccmp::bench(TOTAL, FRAME);
    let us = (time::now_us() - t0).max(1);
    info!(
        "CCMP: {} KiB in {n} frames of {FRAME} bytes encrypted in {} ms ({} KiB/s)",
        TOTAL / 1024,
        us / 1000,
        TOTAL as u64 * 1_000_000 / 1024 / us
    );
}

/// What the station reports for the metrics board (the program publishes
/// it; see [`snapshot`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub associated: bool,
    /// Connections made after the first.
    pub reconnects: u32,
    /// The access point's signal at join, and its channel (0: none yet).
    pub rssi: i8,
    pub channel: u8,
    pub radio: Counters,
}

/// The last [`Snapshot`], as atomics: the station writes, the program's
/// publishing task reads (the same thread, between awaits).
static SNAP: [AtomicU32; 14] = [const { AtomicU32::new(0) }; 14];
static JOINS: AtomicU32 = AtomicU32::new(0);
/// The next time the radio's counters are copied into [`SNAP`].
static SNAP_AT: AtomicU32 = AtomicU32::new(0);

/// Copy the radio's counters (at most once a second, unless `now`).
fn note<R: Radio>(r: &R, force: bool) {
    let t = (time::now_ms() / 1000) as u32;
    if !force && t < SNAP_AT.load(Ordering::Relaxed) {
        return;
    }
    SNAP_AT.store(t + 1, Ordering::Relaxed);
    let c = r.counters();
    let v = [
        c.rx_frames,
        c.rx_crc_errors,
        c.rx_malformed,
        c.rx_usb_errors,
        c.tx_frames,
        c.tx_usb_errors,
        c.tx_ok,
        c.tx_failed,
        c.tx_retries,
        c.rate_kbps,
    ];
    for (a, v) in SNAP.iter().zip(v) {
        a.store(v, Ordering::Relaxed);
    }
}

fn note_bss(bss: &Network) {
    SNAP[10].store(bss.rssi as u8 as u32, Ordering::Relaxed);
    SNAP[11].store(bss.channel as u32, Ordering::Relaxed);
}

/// The station's figures for the metrics board.
pub fn snapshot() -> Snapshot {
    let g = |i: usize| SNAP[i].load(Ordering::Relaxed);
    Snapshot {
        associated: CONNECTED.load(Ordering::Relaxed),
        reconnects: JOINS.load(Ordering::Relaxed).saturating_sub(1),
        rssi: g(10) as u8 as i8,
        channel: g(11) as u8,
        radio: Counters {
            rx_frames: g(0),
            rx_crc_errors: g(1),
            rx_malformed: g(2),
            rx_usb_errors: g(3),
            tx_frames: g(4),
            tx_usb_errors: g(5),
            tx_ok: g(6),
            tx_failed: g(7),
            tx_retries: g(8),
            rate_kbps: g(9),
        },
    }
}

/// The wait before the first rescan when the network isn't there, and
/// before the first new attempt after a failed or lost connection.
const NOT_FOUND_WAIT_MS: u64 = 10_000;
const RETRY_WAIT_MS: u64 = 3000;

/// Scan, join the best access point of `ssid`, stay connected while it
/// lasts; then start over, waiting longer after each failure in a row.
/// Returns only when the radio dies (or there is nothing configured).
/// `ht`: associate as an 802.11n (and WMM) station where the access point
/// is one; else always as a legacy 802.11g station.
pub async fn serve<R: Radio>(r: &mut R, ssid: Option<&str>, psk: Option<&str>, ht: bool) -> Result<()> {
    let (Some(ssid), Some(psk)) = (ssid, psk) else {
        warn!("no [wifi] ssid/psk in touchos.toml; not connecting");
        return Ok(());
    };
    if r.caps().keys == KeyMode::Software {
        log_ccmp_speed();
    }
    let mut failures = Backoff::default();
    loop {
        if LEAVING.load(Ordering::SeqCst) {
            return Ok(());
        }
        let nets = r.scan().await?;
        let mut candidates = scan::for_ssid(&nets, ssid.as_bytes());
        info!(
            "scan: {} networks, {} access points for the configured SSID",
            nets.len(),
            candidates.len()
        );
        candidates.retain(|n| n.ccmp_psk);
        let Some(mut bss) = candidates.first().cloned() else {
            let wait = failures.next(NOT_FOUND_WAIT_MS);
            warn!("configured network not found (or not WPA2-PSK/CCMP); rescanning in {} s", wait / 1000);
            exec::sleep_ms(wait).await;
            continue;
        };
        // What we ask for: HT needs a QoS association (802.11 HT STAs are
        // QoS STAs); a legacy association asks for neither.
        if !ht {
            bss.ht = None;
            bss.wmm = false;
        }
        if !bss.wmm {
            bss.ht = None;
        }
        info!(
            "best access point: channel {}, {} dBm, {}",
            bss.channel,
            bss.rssi,
            match (bss.ht.is_some(), bss.wmm) {
                (true, _) => "802.11n",
                (false, true) => "802.11g with WMM",
                (false, false) => "802.11g",
            }
        );
        note_bss(&bss);
        match connect(r, bss, psk).await {
            Ok(mut station) => {
                failures = Backoff::default();
                JOINS.fetch_add(1, Ordering::Relaxed);
                if run(r, &mut station).await == Ended::Leaving {
                    return Ok(());
                }
            }
            Err(e) => warn!("connect failed: {e}"),
        }
        note(r, true);
        if r.dead() {
            return Err(Error::Hw("the radio stopped working"));
        }
        // Start over cleanly: reset the radio before the next attempt.
        let wait = failures.next(RETRY_WAIT_MS);
        if wait > RETRY_WAIT_MS {
            info!("trying again in {} s", wait / 1000);
        }
        exec::sleep_ms(wait).await;
        r.reset().await?;
    }
}

pub struct Station {
    pub mac: [u8; 6],
    pub bss: Network,
    pub aid: u16,
    mode: KeyMode,
    host_duration: bool,
    /// A QoS (WMM) association: data goes as QoS data.
    qos: bool,
    /// From the 4-way handshake on.
    keys: Option<Keys>,
    tx_pn: u64,
    rx: RxKeys,
}

impl Station {
    fn from_bss_to_me(&self, h: &Hdr) -> bool {
        h.addr1 == self.mac && h.addr2 == self.bss.bssid
    }

    /// The rate the access point ACKs at, as far as we can tell: its lowest
    /// basic rate. Exact for management frames (sent at that rate); for data
    /// frames sent faster it overestimates the duration, which is safe.
    fn ack_rate(&self) -> u8 {
        rates::lowest_basic(&self.bss.rates, self.bss.channel)
    }

    fn fill_duration(&self, frame: &mut [u8]) {
        if self.host_duration {
            let short = self.bss.capability & 0x20 != 0;
            w::set_duration(frame, w::ack_duration(self.ack_rate(), short));
        }
    }
}

// ---- frame I/O ----

async fn send_mgmt<R: Radio>(r: &mut R, s: &Station, frame: &[u8]) -> Result<()> {
    let mut f = frame.to_vec();
    s.fill_duration(&mut f);
    r.send_mgmt(&f).await
}

/// Send an Ethernet frame to the AP as a data frame; encrypted once the PTK is installed.
async fn send_eth<R: Radio>(r: &mut R, s: &mut Station, eth: &[u8], encrypt: bool) -> Result<()> {
    let mut f = w::eth_to_80211(eth, &s.mac, &s.bss.bssid, encrypt, s.qos)
        .ok_or(Error::Hw("bad Ethernet frame"))?;
    s.fill_duration(&mut f);
    if !encrypt {
        return r.send_data(&f, None).await;
    }
    let tk = s.keys.as_ref().ok_or(Error::Hw("no pairwise key yet"))?.tk;
    s.tx_pn += 1;
    match s.mode {
        KeyMode::Hardware | KeyMode::HardwareTx => {
            let key = TxKey {
                tk: &tk,
                pn: s.tx_pn,
                key_id: 0,
            };
            r.send_data(&f, Some(key)).await
        }
        KeyMode::Software => {
            let h = w::header_len(f[0], f[1]);
            let c = ccmp::encrypt(&tk, &f[..h], s.tx_pn, 0, &f[h..]);
            r.send_data(&c, None).await
        }
    }
}

/// Wait for a frame matching `pred`. (Other frames received in the same
/// batch are dropped.)
async fn wait_frame<R: Radio>(
    r: &mut R,
    ms: u64,
    pred: impl Fn(&RxFrame) -> bool,
) -> Option<RxFrame> {
    let end = time::now_ms() + ms;
    loop {
        let found = r.recv().into_iter().find(|f| pred(f));
        if found.is_some() {
            return found;
        }
        if time::now_ms() > end || r.dead() {
            return None;
        }
        exec::sleep_ms(2).await;
    }
}

/// A data frame from the access point, to us or to a group we want
/// ([`Hdr::wanted_group`]), as Ethernet frames (several from an A-MSDU; see
/// [`RxKeys::open`]). Other groups are dropped before they cost a decryption.
fn rx_data(s: &mut Station, f: &RxFrame, h: &Hdr) -> Vec<Vec<u8>> {
    let to_us = h.addr1 == s.mac || (h.group() && h.wanted_group(s.mac));
    if !h.is_data() || h.addr2 != s.bss.bssid || !to_us {
        return Vec::new();
    }
    s.rx.open(&f.data, h, f.crypto)
}

// ---- connection ----

pub async fn connect<R: Radio>(r: &mut R, bss: Network, passphrase: &str) -> Result<Station> {
    let caps = r.caps();
    let mut s = Station {
        mac: r.mac(),
        bss,
        aid: 0,
        mode: caps.keys,
        host_duration: caps.host_duration,
        qos: false,
        keys: None,
        tx_pn: 0,
        rx: RxKeys::new(caps.keys),
    };
    let t0 = time::now_ms();
    let pmk = wpa::pmk(passphrase, &s.bss.ssid_raw);
    info!(
        "PSK derived in {} ms; joining {:02x?} on channel {}",
        time::now_ms() - t0,
        s.bss.bssid,
        s.bss.channel
    );
    r.join(&s.bss).await?;
    let established: Result<()> = async {
    // 802.11 open-system authentication.
    let mut authed = false;
    for attempt in 1..=3 {
        send_mgmt(r, &s, &w::auth_request(&s.mac, &s.bss.bssid)).await?;
        // (A frame too short for its status is not an answer.)
        if let Some(f) = wait_frame(r, 300, |f| {
            Hdr::parse(&f.data).is_some_and(|h| {
                h.mgmt_subtype() == Some(w::FC0_MGMT_AUTH) && s.from_bss_to_me(&h)
            }) && w::auth_status(&f.data).is_some()
        })
        .await
        {
            let st = w::auth_status(&f.data).unwrap_or(u16::MAX);
            if st != 0 {
                warn!("authentication refused (status {st})");
                return Err(Error::Hw("authentication refused"));
            }
            authed = true;
            break;
        }
        warn!(
            "no authentication response (attempt {attempt}); tx results {}",
            r.tx_report()
        );
    }
    if !authed {
        return Err(Error::Timeout("authentication"));
    }
    info!("authenticated");

    // Association.
    let group = s.bss.group_cipher.unwrap_or([0x00, 0x0f, 0xac, 4]);
    let rsn = wpa::rsn_ie(group);
    let req = w::assoc_request(&s.mac, &s.bss, &rsn);
    send_mgmt(r, &s, &req).await?;
    let resp = wait_frame(r, 1000, |f| {
        Hdr::parse(&f.data).is_some_and(|h| {
            h.mgmt_subtype() == Some(w::FC0_MGMT_ASSOC_RESP) && s.from_bss_to_me(&h)
        }) && w::assoc_response(&f.data).is_some()
    })
    .await
    .ok_or(Error::Timeout("association response"))?;
    let (st, aid) = w::assoc_response(&resp.data).unwrap_or((u16::MAX, 0));
    if st != 0 {
        warn!("association refused (status {st})");
        return Err(Error::Hw("association refused"));
    }
    s.aid = aid;
    // The access point takes us as a QoS station if its answer has the
    // WMM Parameter element; without QoS there is no HT either.
    s.qos = s.bss.wmm && w::has_wmm_param(w::assoc_response_ies(&resp.data));
    if s.bss.wmm && !s.qos {
        warn!("the access point didn't take us as a QoS station: associated as 802.11g");
    }
    let ht = s.qos && s.bss.ht.is_some();
    info!(
        "associated, AID {} ({})",
        s.aid,
        if ht { "802.11n" } else if s.qos { "WMM" } else { "legacy" }
    );
    r.associated(s.aid, ht).await?;

    // WPA2 4-way handshake (EAPOL-Key frames as plain data frames).
    let mut snonce = [0u8; 32];
    touchos_rt::rand::fill(&mut snonce);
    let mut hs = Handshake::new(pmk, s.bss.bssid, s.mac, snonce, rsn);
    let m1 = wait_eapol(r, &mut s, 3000, Handshake::is_message1)
        .await
        .ok_or(Error::Timeout("EAPOL message 1"))?;
    let m2 = hs.message2(&wpa::parse_key_frame(&m1).unwrap());
    let e = eapol_eth(&s, &m2);
    send_eth(r, &mut s, &e, false).await?;
    info!("4-way handshake: message 2 sent");

    let m3 = wait_eapol(r, &mut s, 3000, |k| hs.is_message3(k))
        .await
        .ok_or(Error::Timeout("EAPOL message 3 (wrong passphrase?)"))?;
    let (m4, keys) = hs.message4(&wpa::parse_key_frame(&m3).unwrap())?;
    if let Some(g) = &keys.gtk {
        debug!("group key id {}, RSC {}", g.id, g.rsc);
    }
    let e = eapol_eth(&s, &m4);
    send_eth(r, &mut s, &e, false).await?;
    // Give message 4 time to leave before keys change.
    exec::sleep_ms(20).await;
    r.install_ptk(&keys.tk).await?;
    if let Some(g) = &keys.gtk {
        r.install_gtk(g.id, &g.key).await?;
    }
    s.rx.install(&keys);
    info!(
        "WPA2 handshake complete (group key {})",
        if keys.gtk.is_some() { "ok" } else { "missing" }
    );
    s.keys = Some(keys);
    Ok(())
    }
    .await;
    if let Err(e) = established {
        // The access point may count us as authenticated or associated;
        // some ignore our next attempt until they have forgotten us.
        if !r.dead() {
            goodbye(r, &s).await;
        }
        return Err(e);
    }
    Ok(s)
}

/// Tell the access point we are gone (see [`w::deauth`]). Best effort:
/// whether it arrives changes nothing on our side.
async fn goodbye<R: Radio>(r: &mut R, s: &Station) {
    match send_mgmt(r, s, &w::deauth(&s.mac, &s.bss.bssid, w::REASON_LEAVING)).await {
        Ok(()) => info!("deauthentication sent to {:02x?}", s.bss.bssid),
        Err(e) => debug!("deauthentication: {e}"),
    }
}

/// Set by [`leave`]: the program is about to exit.
static LEAVING: AtomicBool = AtomicBool::new(false);
/// Connected: in [`run`], until the goodbye is sent.
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// Before the program exits (the settings changed): the connected station
/// says goodbye to the access point (it notices within [`IDLE_WAIT_MS`])
/// and stops. Waits at most `max_ms` for that.
pub async fn leave(max_ms: u64) {
    LEAVING.store(true, Ordering::SeqCst);
    let end = time::now_ms() + max_ms;
    while CONNECTED.load(Ordering::SeqCst) && time::now_ms() < end {
        exec::sleep_ms(10).await;
    }
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The access point disconnected us.
    ByAccessPoint,
    /// Nothing heard from the access point for a while.
    Lost,
    /// The radio stopped working.
    RadioDead,
    /// [`leave`] asked us to.
    Leaving,
}

fn eapol_eth(s: &Station, eapol: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(14 + eapol.len());
    e.extend_from_slice(&s.bss.bssid);
    e.extend_from_slice(&s.mac);
    e.extend_from_slice(&[0x88, 0x8e]);
    e.extend_from_slice(eapol);
    e
}

/// Wait for an EAPOL-Key frame from the AP (unprotected, or decrypted later in RUN).
async fn wait_eapol<R: Radio>(
    r: &mut R,
    s: &mut Station,
    ms: u64,
    accept: impl Fn(&wpa::KeyFrame) -> bool,
) -> Option<Vec<u8>> {
    let end = time::now_ms() + ms;
    loop {
        let f = wait_frame(r, end.saturating_sub(time::now_ms()).max(1), |f| {
            Hdr::parse(&f.data).is_some_and(|h| h.is_data() && s.from_bss_to_me(&h))
        })
        .await?;
        let h = Hdr::parse(&f.data)?;
        for eth in rx_data(s, &f, &h) {
            if !w::is_eapol(&eth) {
                continue;
            }
            let eapol = eth[14..].to_vec();
            if let Some(k) = wpa::parse_key_frame(&eapol)
                && accept(&k)
            {
                return Some(eapol);
            }
        }
        if time::now_ms() > end {
            return None;
        }
    }
}

/// Connected: move frames between the radio and the network stack until the
/// link drops. Returns when we are disconnected, and why; when we left
/// (lost the access point, or [`leave`]), the access point has been told.
pub async fn run<R: Radio>(r: &mut R, s: &mut Station) -> Ended {
    CONNECTED.store(true, Ordering::SeqCst);
    let ended = pump(r, s).await;
    if matches!(ended, Ended::Lost | Ended::Leaving) {
        goodbye(r, s).await;
    }
    CONNECTED.store(false, Ordering::SeqCst);
    link::down();
    ended
}

async fn pump<R: Radio>(r: &mut R, s: &mut Station) -> Ended {
    // What `net` queued while we were not connected is stale: drop it
    // rather than send it over the new link.
    let stale = core::iter::from_fn(link::pop_tx).count();
    if stale > 0 {
        debug!("dropped {stale} frames queued while disconnected");
    }
    link::up(s.mac);
    let mut last_rx = time::now_ms();
    loop {
        let frames = r.recv();
        if r.dead() {
            warn!("the radio stopped working");
            return Ended::RadioDead;
        }
        note(r, false);
        if LEAVING.load(Ordering::SeqCst) {
            return Ended::Leaving;
        }
        for f in frames {
            let Some(h) = Hdr::parse(&f.data) else { continue };
            if h.addr2 == s.bss.bssid {
                last_rx = time::now_ms();
            }
            match h.mgmt_subtype() {
                Some(w::FC0_MGMT_DEAUTH) | Some(w::FC0_MGMT_DISASSOC) if s.from_bss_to_me(&h) => {
                    match w::reason(&f.data) {
                        Some(r) => warn!("disconnected by the access point (reason {r})"),
                        None => warn!("disconnected by the access point (no reason code)"),
                    }
                    return Ended::ByAccessPoint;
                }
                Some(w::FC0_MGMT_ACTION) if s.from_bss_to_me(&h) => {
                    refuse_block_ack(r, s, &f.data).await;
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            for eth in rx_data(s, &f, &h) {
                if w::is_eapol(&eth) {
                    handle_rekey(r, s, &eth[14..]).await;
                    continue;
                }
                link::deliver(eth);
            }
        }
        let mut sent = 0;
        while sent < 16 {
            let Some(eth) = link::pop_tx() else {
                break;
            };
            if let Err(e) = send_eth(r, s, &eth, true).await {
                warn!("tx: {e}");
                break;
            }
            sent += 1;
        }
        if time::now_ms() - last_rx > 10_000 {
            warn!("no frames from the access point for 10 s; reconnecting");
            return Ended::Lost;
        }
        if sent == 0 {
            match r.event_bit() {
                0 => exec::sleep_ms(1).await,
                // Until the radio has news or the net server a frame to send
                // (the access point's beacons come ten times a second).
                bit => {
                    exec::wait_bits_or_ms(bit | tx_bit(), IDLE_WAIT_MS).await;
                }
            }
        } else {
            exec::yield_now().await;
        }
    }
}

/// The longest the connected station sleeps when the radio wakes it.
const IDLE_WAIT_MS: u64 = 100;

/// Our bit for "the net server queued a frame" (registered once).
fn tx_bit() -> usize {
    static BIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    match BIT.load(core::sync::atomic::Ordering::Relaxed) {
        0 => {
            let b = exec::alloc_bit();
            link::wait_tx(b);
            BIT.store(b, core::sync::atomic::Ordering::Relaxed);
            b
        }
        b => b,
    }
}

/// An ADDBA request (an 802.11n access point that wants to send
/// A-MPDUs): declined, so frames keep coming one at a time and in order.
async fn refuse_block_ack<R: Radio>(r: &mut R, s: &Station, frame: &[u8]) {
    let Some(req) = w::addba_request(frame) else { return };
    debug!("block ack for TID {} refused", req.tid());
    if let Err(e) = send_mgmt(r, s, &w::addba_refusal(&s.mac, &s.bss.bssid, &req)).await {
        warn!("ADDBA response: {e}");
    }
}

/// Group key handshake (message 1 of 2) while connected.
async fn handle_rekey<R: Radio>(r: &mut R, s: &mut Station, eapol: &[u8]) {
    let (Some(k), Some(keys)) = (wpa::parse_key_frame(eapol), s.keys.as_ref()) else {
        return;
    };
    let Some((gtk, reply)) = keys::group_message(keys, &k) else {
        return;
    };
    s.rx.set_gtk(&gtk);
    let e = eapol_eth(s, &reply);
    let _ = send_eth(r, s, &e, true).await;
    if let Err(e) = r.install_gtk(gtk.id, &gtk.key).await {
        warn!("group key install: {e}");
    }
    info!("group key renewed (key id {})", gtk.id);
}
