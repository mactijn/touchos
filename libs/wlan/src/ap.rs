//! An open access point for the panel's setup network (`wifi.setup_ap`):
//! beacons and probe responses, open-system authentication, association,
//! and data frames to and from Ethernet. 802.11g without encryption, QoS or
//! power save; a few stations at most (a phone or two). The settings page
//! is protected by its own pairing, not by the network.
//!
//! Sans-IO, like `touchos_mdns`: [`Ap::on_frame`] takes what the radio
//! received and returns what to send and what to hand to the network
//! stack; [`Ap::from_eth`] frames what the stack sends; [`Ap::poll`]
//! drops stations that went quiet. [`serve`] runs it on a radio.

use alloc::vec::Vec;

use crate::ieee80211::{self as w, Hdr};

/// Beacon interval in TU (1024 µs).
pub const BEACON_INTERVAL: u16 = 100;
/// At most this many stations at once.
pub const MAX_STATIONS: usize = 8;
/// A station that sent nothing for this long is dropped (with a
/// deauthentication). Phones send at least a null frame now and then.
pub const INACTIVE_MS: u64 = 5 * 60_000;

/// 802.11b rates basic (every station can receive them: group frames go at
/// 1 Mbit/s), then 802.11g's.
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];

/// Capability: ESS, short slot time.
const CAPABILITY: u16 = 0x0001 | 0x0400;

// Status and reason codes (802.11-2020 9.4.1.7, 9.4.1.9).
const STATUS_OK: u16 = 0;
const STATUS_UNSPECIFIED: u16 = 1;
const STATUS_ALGORITHM: u16 = 13;
const STATUS_FULL: u16 = 17;
const REASON_INACTIVE: u16 = 4;
const REASON_NOT_AUTHENTICATED: u16 = 6;
const REASON_NOT_ASSOCIATED: u16 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Authenticated,
    Associated,
}

#[derive(Debug, Clone, Copy)]
struct Station {
    mac: [u8; 6],
    aid: u16,
    state: State,
    last_ms: u64,
}

/// What [`Ap`] wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// An 802.11 frame for the air (management or data).
    Air(Vec<u8>),
    /// An Ethernet frame for the network stack.
    Eth(Vec<u8>),
}

/// Something worth a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Associated([u8; 6], u16),
    Left([u8; 6]),
    Full([u8; 6]),
}

pub struct Ap {
    bssid: [u8; 6],
    ssid: Vec<u8>,
    channel: u8,
    stations: Vec<Station>,
    events: Vec<Event>,
}

impl Ap {
    pub fn new(bssid: [u8; 6], ssid: &[u8], channel: u8) -> Ap {
        Ap { bssid, ssid: ssid.into(), channel, stations: Vec::new(), events: Vec::new() }
    }

    pub fn bssid(&self) -> [u8; 6] {
        self.bssid
    }

    /// Stations associated now.
    pub fn associated(&self) -> usize {
        self.stations.iter().filter(|s| s.state == State::Associated).count()
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    /// The beacon (the radio fills in its timestamp and sends it every
    /// [`BEACON_INTERVAL`]). Its TIM says every beacon is a DTIM and
    /// nothing is buffered: there is no power save here.
    pub fn beacon(&self) -> Vec<u8> {
        let mut f = w::mgmt_header(w::FC0_MGMT_BEACON, &w::BROADCAST, &self.bssid, &self.bssid);
        self.fixed_and_ies(&mut f, true);
        f
    }

    fn probe_response(&self, to: &[u8; 6]) -> Vec<u8> {
        let mut f = w::mgmt_header(w::FC0_MGMT_PROBE_RESP, to, &self.bssid, &self.bssid);
        self.fixed_and_ies(&mut f, false);
        f
    }

    /// Timestamp (0: the radio's), interval, capability, then SSID, rates,
    /// DS parameter set, (TIM,) ERP and extended rates.
    fn fixed_and_ies(&self, f: &mut Vec<u8>, tim: bool) {
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&BEACON_INTERVAL.to_le_bytes());
        f.extend_from_slice(&CAPABILITY.to_le_bytes());
        f.extend_from_slice(&[0, self.ssid.len() as u8]);
        f.extend_from_slice(&self.ssid);
        f.extend_from_slice(&[1, RATES.len() as u8]);
        f.extend_from_slice(&RATES);
        f.extend_from_slice(&[3, 1, self.channel]);
        if tim {
            // DTIM count 0, period 1, bitmap control 0, one empty byte.
            f.extend_from_slice(&[5, 4, 0, 1, 0, 0]);
        }
        f.extend_from_slice(&[42, 1, 0]); // ERP: no protection needed
        f.extend_from_slice(&[50, EXT_RATES.len() as u8]);
        f.extend_from_slice(&EXT_RATES);
    }

    fn mgmt(&self, fc0: u8, to: &[u8; 6], body: &[u8]) -> Vec<u8> {
        let mut f = w::mgmt_header(fc0, to, &self.bssid, &self.bssid);
        f.extend_from_slice(body);
        f
    }

    fn deauth(&self, to: &[u8; 6], reason: u16) -> Out {
        Out::Air(self.mgmt(w::FC0_MGMT_DEAUTH, to, &reason.to_le_bytes()))
    }

    fn station(&mut self, mac: &[u8; 6]) -> Option<&mut Station> {
        self.stations.iter_mut().find(|s| s.mac == *mac)
    }

    /// A frame the radio received.
    pub fn on_frame(&mut self, frame: &[u8], now_ms: u64) -> Vec<Out> {
        let mut out = Vec::new();
        let Some(h) = Hdr::parse(frame) else { return out };
        let to_us = h.addr1 == self.bssid;
        if let Some(st) = self.station(&h.addr2) {
            st.last_ms = now_ms;
        }
        match h.mgmt_subtype() {
            Some(w::FC0_MGMT_PROBE_REQ) => {
                // To us or to everyone, for our SSID or any.
                if (to_us || h.group()) && self.wants_us(&frame[h.len..]) {
                    out.push(Out::Air(self.probe_response(&h.addr2)));
                }
            }
            Some(w::FC0_MGMT_AUTH) if to_us => out.extend(self.auth(&h, frame, now_ms)),
            Some(w::FC0_MGMT_ASSOC_REQ) | Some(w::FC0_MGMT_REASSOC_REQ) if to_us => {
                out.extend(self.assoc(&h, frame, now_ms));
            }
            Some(w::FC0_MGMT_DISASSOC) if to_us => {
                if let Some(st) = self.station(&h.addr2) {
                    st.state = State::Authenticated;
                    let mac = st.mac;
                    self.events.push(Event::Left(mac));
                }
            }
            Some(w::FC0_MGMT_DEAUTH) if to_us => self.forget(&h.addr2),
            Some(_) => {}
            None if h.is_data() && to_us && h.fc1 & 3 == w::FC1_TO_DS => {
                match self.station(&h.addr2).map(|s| s.state) {
                    Some(State::Associated) => {
                        // Null frames (power-management notices) have no
                        // LLC header and give nothing.
                        out.extend(w::data_80211_to_eth(frame, 0, 0).into_iter().map(Out::Eth));
                    }
                    // A data frame from a station we don't know as
                    // associated: tell it, so it joins again.
                    _ => out.push(self.deauth(&h.addr2, REASON_NOT_ASSOCIATED)),
                }
            }
            None => {}
        }
        out
    }

    /// A probe request's SSID element is empty (any network) or ours.
    fn wants_us(&self, body: &[u8]) -> bool {
        match w::ies(body).find(|(id, _)| *id == 0) {
            Some((_, ssid)) => ssid.is_empty() || ssid == self.ssid.as_slice(),
            None => true,
        }
    }

    fn auth(&mut self, h: &Hdr, frame: &[u8], now_ms: u64) -> Option<Out> {
        let b = frame.get(h.len..h.len + 6)?;
        let (alg, seq) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
        if seq != 1 {
            return None;
        }
        let status = if alg != 0 {
            STATUS_ALGORITHM
        } else if self.station(&h.addr2).is_some() {
            // Again (it lost our answer, or starts over): it is
            // authenticated, not associated any more.
            let st = self.station(&h.addr2)?;
            if st.state == State::Associated {
                st.state = State::Authenticated;
            }
            STATUS_OK
        } else if self.stations.len() < MAX_STATIONS {
            self.stations.push(Station { mac: h.addr2, aid: 0, state: State::Authenticated, last_ms: now_ms });
            STATUS_OK
        } else {
            self.events.push(Event::Full(h.addr2));
            STATUS_UNSPECIFIED
        };
        let mut body = Vec::with_capacity(6);
        body.extend_from_slice(&alg.to_le_bytes());
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&status.to_le_bytes());
        Some(Out::Air(self.mgmt(w::FC0_MGMT_AUTH, &h.addr2, &body)))
    }

    fn assoc(&mut self, h: &Hdr, frame: &[u8], now_ms: u64) -> Option<Out> {
        let reassoc = h.mgmt_subtype() == Some(w::FC0_MGMT_REASSOC_REQ);
        // Capability, listen interval (and the current AP for a
        // reassociation), then the elements.
        let ies_at = h.len + if reassoc { 10 } else { 4 };
        let ssid_ok = w::ies(frame.get(ies_at..)?).any(|(id, s)| id == 0 && s == self.ssid.as_slice());
        let Some(st) = self.stations.iter().position(|s| s.mac == h.addr2) else {
            return Some(self.deauth(&h.addr2, REASON_NOT_AUTHENTICATED));
        };
        let aid = match self.stations[st].aid {
            0 => (1..=MAX_STATIONS as u16).find(|a| self.stations.iter().all(|s| s.aid != *a)),
            aid => Some(aid),
        };
        let (status, aid) = match (ssid_ok, aid) {
            (false, _) => (STATUS_UNSPECIFIED, 0),
            (true, None) => (STATUS_FULL, 0),
            (true, Some(aid)) => {
                let s = &mut self.stations[st];
                s.aid = aid;
                s.state = State::Associated;
                s.last_ms = now_ms;
                self.events.push(Event::Associated(h.addr2, aid));
                (STATUS_OK, aid)
            }
        };
        let mut body = Vec::with_capacity(32);
        body.extend_from_slice(&CAPABILITY.to_le_bytes());
        body.extend_from_slice(&status.to_le_bytes());
        body.extend_from_slice(&(aid | 0xc000 * (aid != 0) as u16).to_le_bytes());
        body.extend_from_slice(&[1, RATES.len() as u8]);
        body.extend_from_slice(&RATES);
        body.extend_from_slice(&[50, EXT_RATES.len() as u8]);
        body.extend_from_slice(&EXT_RATES);
        let fc0 = if reassoc { w::FC0_MGMT_REASSOC_RESP } else { w::FC0_MGMT_ASSOC_RESP };
        Some(Out::Air(self.mgmt(fc0, &h.addr2, &body)))
    }

    fn forget(&mut self, mac: &[u8; 6]) {
        let before = self.stations.len();
        self.stations.retain(|s| s.mac != *mac);
        if self.stations.len() != before {
            self.events.push(Event::Left(*mac));
        }
    }

    /// An Ethernet frame from the network stack, framed for the air: to a
    /// group, or to an associated station (anything else has nowhere to
    /// go).
    pub fn from_eth(&self, eth: &[u8]) -> Option<Vec<u8>> {
        let da: [u8; 6] = eth.get(..6)?.try_into().ok()?;
        let reachable =
            da[0] & 1 != 0 || self.stations.iter().any(|s| s.mac == da && s.state == State::Associated);
        if !reachable {
            return None;
        }
        w::eth_to_80211_from_ds(eth, &self.bssid)
    }

    /// Drop the stations that went quiet; returns their deauthentications.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Out> {
        let quiet: Vec<[u8; 6]> =
            self.stations.iter().filter(|s| now_ms.saturating_sub(s.last_ms) >= INACTIVE_MS).map(|s| s.mac).collect();
        let mut out = Vec::new();
        for mac in quiet {
            out.push(self.deauth(&mac, REASON_INACTIVE));
            self.forget(&mac);
        }
        out
    }

    /// When [`Ap::poll`] has something to do next.
    pub fn poll_at(&self) -> Option<u64> {
        self.stations.iter().map(|s| s.last_ms + INACTIVE_MS).min()
    }

    /// Deauthentications for every station (the access point goes away).
    pub fn goodbye(&mut self) -> Vec<Out> {
        let out = self.stations.iter().map(|s| self.deauth(&s.mac, w::REASON_LEAVING)).collect();
        self.stations.clear();
        out
    }
}

#[cfg(target_os = "none")]
mod run {
    use super::*;
    use crate::radio::Radio;
    use crate::{Error, Result};
    use ::log::{info, warn};
    use touchos_drv::link;
    use touchos_rt::{exec, time};

    /// How long the loop sleeps when nothing wakes it (the radio's
    /// interrupt or a frame from `net` do).
    const IDLE_WAIT_MS: u64 = 100;

    /// Run the setup network on `r` until `stop()` says so (checked about
    /// every [`IDLE_WAIT_MS`]); `ssid` is announced to `net` with the
    /// link. Returns Err when the radio fails.
    pub async fn serve<R: Radio>(
        r: &mut R,
        ssid: touchos_proto::net::HostName,
        channel: u8,
        mut stop: impl FnMut(&Ap) -> bool,
    ) -> Result<()> {
        let mut ap = Ap::new(r.mac(), ssid.as_str().as_bytes(), channel);
        r.start_ap(channel, &ap.beacon()).await?;
        info!("setup network {} up on channel {channel} (open; http://192.168.4.1)", ssid.as_str());
        let stale = core::iter::from_fn(link::pop_tx).count();
        if stale > 0 {
            ::log::debug!("dropped {stale} frames queued before the setup network");
        }
        link::up_ap(ap.bssid(), ssid);
        let result = pump(r, &mut ap, &mut stop).await;
        for f in ap.goodbye() {
            if let Out::Air(f) = f {
                let _ = r.send_ap(&f).await;
            }
        }
        link::down();
        info!("setup network {} down", ssid.as_str());
        result
    }

    async fn pump<R: Radio>(r: &mut R, ap: &mut Ap, stop: &mut impl FnMut(&Ap) -> bool) -> Result<()> {
        loop {
            if stop(ap) {
                return Ok(());
            }
            let frames = r.recv();
            if r.dead() {
                return Err(Error::Hw("the radio stopped working"));
            }
            let now = time::now_ms();
            let mut out = Vec::new();
            for f in frames {
                out.extend(ap.on_frame(&f.data, now));
            }
            out.extend(ap.poll(now));
            for e in ap.take_events() {
                match e {
                    Event::Associated(m, aid) => info!("setup network: {} joined (AID {aid})", crate::Mac(&m)),
                    Event::Left(m) => info!("setup network: {} left", crate::Mac(&m)),
                    Event::Full(m) => warn!("setup network: {} refused, {MAX_STATIONS} stations already", crate::Mac(&m)),
                }
            }
            let mut sent = 0;
            for o in out {
                match o {
                    Out::Air(f) => r.send_ap(&f).await?,
                    Out::Eth(e) => link::deliver(e),
                }
                sent += 1;
            }
            while sent < 32 {
                let Some(eth) = link::pop_tx() else { break };
                if let Some(f) = ap.from_eth(&eth) {
                    r.send_ap(&f).await?;
                }
                sent += 1;
            }
            if sent == 0 {
                match r.event_bit() {
                    0 => exec::sleep_ms(1).await,
                    bit => {
                        exec::wait_bits_or_ms(bit | crate::station::tx_bit(), IDLE_WAIT_MS).await;
                    }
                }
            } else {
                exec::yield_now().await;
            }
        }
    }
}

#[cfg(target_os = "none")]
pub use run::serve;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::{RxCrypto, RxFrame};
    use crate::scan;
    use alloc::vec;

    const BSSID: [u8; 6] = [0x00, 0x0c, 0x43, 0x11, 0x22, 0x33];
    const PHONE: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];
    const SSID: &[u8] = b"touchos-112233";

    fn ap() -> Ap {
        Ap::new(BSSID, SSID, 6)
    }

    fn frame(fc0: u8, fc1: u8, a1: [u8; 6], a2: [u8; 6], a3: [u8; 6], body: &[u8]) -> Vec<u8> {
        let mut f = vec![fc0, fc1, 0, 0];
        f.extend_from_slice(&a1);
        f.extend_from_slice(&a2);
        f.extend_from_slice(&a3);
        f.extend_from_slice(&[0x10, 0]);
        f.extend_from_slice(body);
        f
    }

    fn probe(ssid: &[u8]) -> Vec<u8> {
        let mut body = vec![0, ssid.len() as u8];
        body.extend_from_slice(ssid);
        body.extend_from_slice(&[1, 4, 0x02, 0x04, 0x0b, 0x16]);
        frame(w::FC0_MGMT_PROBE_REQ, 0, w::BROADCAST, PHONE, w::BROADCAST, &body)
    }

    fn auth(alg: u16) -> Vec<u8> {
        let mut body = alg.to_le_bytes().to_vec();
        body.extend_from_slice(&[1, 0, 0, 0]);
        frame(w::FC0_MGMT_AUTH, 0, BSSID, PHONE, BSSID, &body)
    }

    fn assoc(ssid: &[u8]) -> Vec<u8> {
        let mut body = vec![0x21, 0x04, 10, 0, 0, ssid.len() as u8];
        body.extend_from_slice(ssid);
        body.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
        frame(w::FC0_MGMT_ASSOC_REQ, 0, BSSID, PHONE, BSSID, &body)
    }

    fn air(out: &[Out]) -> Vec<&Vec<u8>> {
        out.iter().filter_map(|o| if let Out::Air(f) = o { Some(f) } else { None }).collect()
    }

    /// Our IPv4 ARP request from the phone, as an Ethernet frame.
    fn arp_eth(from: [u8; 6]) -> Vec<u8> {
        let mut e = w::BROADCAST.to_vec();
        e.extend_from_slice(&from);
        e.extend_from_slice(&[0x08, 0x06]);
        e.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 1]);
        e.extend_from_slice(&from);
        e.extend_from_slice(&[192, 168, 4, 10, 0, 0, 0, 0, 0, 0, 192, 168, 4, 1]);
        e
    }

    fn join(a: &mut Ap) {
        assert_eq!(air(&a.on_frame(&auth(0), 0)).len(), 1);
        assert_eq!(air(&a.on_frame(&assoc(SSID), 1)).len(), 1);
    }

    #[test]
    fn beacon_parses_as_an_open_network() {
        let a = ap();
        let b = a.beacon();
        assert_eq!(&b[..2], &[w::FC0_MGMT_BEACON, 0]);
        assert_eq!(&b[4..10], &w::BROADCAST);
        assert_eq!(&b[10..16], &BSSID);
        let n = scan::parse_beacon(&RxFrame { data: b, rssi: -40, channel: 6, crypto: RxCrypto::None }).unwrap();
        assert_eq!(n.ssid, "touchos-112233");
        assert_eq!(n.bssid, BSSID);
        assert_eq!(n.channel, 6);
        assert_eq!(n.beacon_interval, BEACON_INTERVAL);
        assert!(!n.ccmp_psk);
        assert_eq!(n.rates.len(), 12);
        assert!(n.ht.is_none() && !n.wmm && !n.erp_protection);
        assert_eq!(n.capability & 0x0010, 0, "no privacy bit");
    }

    #[test]
    fn answers_probes_for_us_or_anyone() {
        let mut a = ap();
        for ssid in [&b""[..], SSID] {
            let out = a.on_frame(&probe(ssid), 0);
            let f = air(&out);
            assert_eq!(f.len(), 1, "{ssid:?}");
            assert_eq!(f[0][0], w::FC0_MGMT_PROBE_RESP);
            assert_eq!(&f[0][4..10], &PHONE);
            let n = scan::parse_beacon(&RxFrame { data: f[0].clone(), rssi: 0, channel: 6, crypto: RxCrypto::None }).unwrap();
            assert_eq!(n.ssid.as_bytes(), SSID);
        }
        assert!(a.on_frame(&probe(b"other-network"), 0).is_empty());
    }

    #[test]
    fn open_authentication_and_association() {
        let mut a = ap();
        let out = a.on_frame(&auth(0), 0);
        let f = air(&out)[0];
        assert_eq!(f[0], w::FC0_MGMT_AUTH);
        assert_eq!(&f[4..10], &PHONE);
        assert_eq!(&f[24..30], &[0, 0, 2, 0, 0, 0], "open, sequence 2, success");

        let out = a.on_frame(&assoc(SSID), 1);
        let f = air(&out)[0];
        assert_eq!(f[0], w::FC0_MGMT_ASSOC_RESP);
        assert_eq!(w::assoc_response(f), Some((0, 1)));
        assert_eq!(&f[28..30], &[1, 0xc0], "the AID's top bits are set");
        assert_eq!(a.associated(), 1);
        assert_eq!(a.take_events(), [Event::Associated(PHONE, 1)]);
    }

    #[test]
    fn refuses_what_it_cant_do() {
        let mut a = ap();
        // Shared-key authentication.
        let out = a.on_frame(&auth(1), 0);
        assert_eq!(&air(&out)[0][28..30], &STATUS_ALGORITHM.to_le_bytes());
        // Association without authentication: deauthenticated.
        let mut b = ap();
        let out = b.on_frame(&assoc(SSID), 0);
        assert_eq!(air(&out)[0][0], w::FC0_MGMT_DEAUTH);
        assert_eq!(w::reason(air(&out)[0]), Some(REASON_NOT_AUTHENTICATED));
        // The wrong SSID.
        let mut c = ap();
        c.on_frame(&auth(0), 0);
        let out = c.on_frame(&assoc(b"other"), 0);
        assert_eq!(w::assoc_response(air(&out)[0]), Some((STATUS_UNSPECIFIED, 0)));
        assert_eq!(c.associated(), 0);
    }

    #[test]
    fn full() {
        let mut a = ap();
        for i in 0..MAX_STATIONS as u8 {
            let mut f = auth(0);
            f[15] = i;
            assert_eq!(&air(&a.on_frame(&f, 0))[0][28..30], &[0, 0]);
        }
        let mut f = auth(0);
        f[15] = 0xff;
        assert_eq!(&air(&a.on_frame(&f, 0))[0][28..30], &STATUS_UNSPECIFIED.to_le_bytes());
        assert_eq!(a.take_events().last(), Some(&Event::Full([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xff])));
    }

    #[test]
    fn data_both_ways() {
        let mut a = ap();
        // Before associating: no data in; a deauthentication out.
        let mut body = vec![0xaa, 0xaa, 0x03, 0, 0, 0];
        body.extend_from_slice(&arp_eth(PHONE)[12..]);
        let data = frame(w::FC0_DATA, w::FC1_TO_DS, BSSID, PHONE, w::BROADCAST, &body);
        let out = a.on_frame(&data, 0);
        assert!(matches!(&out[..], [Out::Air(f)] if f[0] == w::FC0_MGMT_DEAUTH));
        assert!(a.from_eth(&{
            let mut e = PHONE.to_vec();
            e.extend_from_slice(&BSSID);
            e.extend_from_slice(&[0x08, 0x06, 1, 2]);
            e
        })
        .is_none());

        join(&mut a);
        assert_eq!(a.on_frame(&data, 2), [Out::Eth(arp_eth(PHONE))]);

        // Our answer: from-DS, to the phone, from us.
        let mut reply = PHONE.to_vec();
        reply.extend_from_slice(&BSSID);
        reply.extend_from_slice(&[0x08, 0x06, 1, 2, 3]);
        let f = a.from_eth(&reply).unwrap();
        let h = Hdr::parse(&f).unwrap();
        assert_eq!((h.fc0, h.fc1), (w::FC0_DATA, w::FC1_FROM_DS));
        assert_eq!((h.addr1, h.addr2, h.addr3), (PHONE, BSSID, BSSID));
        assert_eq!(w::data_80211_to_eth(&f, 0, 0), [reply.clone()]);
        // Broadcast always goes out; another station's unicast doesn't.
        assert!(a.from_eth(&arp_eth(BSSID)).is_some());
        let mut other = reply.clone();
        other[5] = 0x99;
        assert!(a.from_eth(&other).is_none());
        // Null data: no Ethernet frame.
        assert!(a.on_frame(&frame(0x48, w::FC1_TO_DS | 0x10, BSSID, PHONE, BSSID, &[]), 3).is_empty());
    }

    #[test]
    fn leaving_and_going_quiet() {
        let mut a = ap();
        join(&mut a);
        a.take_events();
        a.on_frame(&frame(w::FC0_MGMT_DISASSOC, 0, BSSID, PHONE, BSSID, &[8, 0]), 10);
        assert_eq!(a.associated(), 0);
        // Back without a new authentication.
        assert_eq!(w::assoc_response(air(&a.on_frame(&assoc(SSID), 11))[0]), Some((0, 1)));
        a.on_frame(&frame(w::FC0_MGMT_DEAUTH, 0, BSSID, PHONE, BSSID, &[3, 0]), 12);
        assert!(a.stations.is_empty());

        let mut b = ap();
        join(&mut b);
        assert_eq!(b.poll_at(), Some(1 + INACTIVE_MS));
        assert!(b.poll(INACTIVE_MS).is_empty());
        let out = b.poll(1 + INACTIVE_MS);
        assert_eq!(w::reason(air(&out)[0]), Some(REASON_INACTIVE));
        assert_eq!((b.associated(), b.poll_at()), (0, None));

        let mut c = ap();
        join(&mut c);
        assert_eq!(air(&c.goodbye()).len(), 1);
        assert_eq!(c.associated(), 0);
    }

    #[test]
    fn reassociation_keeps_the_aid() {
        let mut a = ap();
        join(&mut a);
        let mut body = vec![0x21, 0x04, 10, 0];
        body.extend_from_slice(&BSSID);
        body.extend_from_slice(&[0, SSID.len() as u8]);
        body.extend_from_slice(SSID);
        let f = frame(w::FC0_MGMT_REASSOC_REQ, 0, BSSID, PHONE, BSSID, &body);
        let out = a.on_frame(&f, 5);
        assert_eq!(air(&out)[0][0], w::FC0_MGMT_REASSOC_RESP);
        assert_eq!(w::assoc_response(air(&out)[0]), Some((0, 1)));
    }
}
