//! Scan results: beacons and probe responses -> [`Network`], and a scan for
//! chips that leave scanning to the host.

use alloc::string::String;
use alloc::vec::Vec;

use crate::ieee80211::ies;
use crate::radio::RxFrame;

/// Add `n`, keeping the strongest sighting per BSSID.
pub fn merge(nets: &mut Vec<Network>, n: Network) {
    match nets.iter_mut().find(|x| x.bssid == n.bssid) {
        Some(x) if x.rssi >= n.rssi => {}
        Some(x) => *x = n,
        None => nets.push(n),
    }
}

/// Received frames -> the networks in them: one per BSSID, strongest first.
pub fn collect(frames: impl IntoIterator<Item = RxFrame>) -> Vec<Network> {
    let mut nets: Vec<Network> = Vec::new();
    for f in frames {
        if let Some(n) = parse_beacon(&f) {
            merge(&mut nets, n);
        }
    }
    nets.sort_by(|a, b| b.rssi.cmp(&a.rssi));
    nets
}

/// The access points of network `ssid` (in the order of `nets`).
pub fn for_ssid(nets: &[Network], ssid: &[u8]) -> Vec<Network> {
    nets.iter()
        .filter(|n| n.ssid_raw == ssid)
        .cloned()
        .collect()
}

/// The 2.4 GHz channels a host-driven scan visits: 1-13, as Europe allows
/// them (there is no regulatory-domain setting yet).
pub const CHANNELS_2GHZ: [u8; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];

/// A passive scan driven by the host, for soft-MACs: listen on each channel
/// for `dwell_ms` (a beacon interval is about 100 ms) and collect beacons.
#[cfg(target_os = "none")]
pub async fn host_scan<R: crate::radio::Radio>(
    radio: &mut R,
    channels: &[u8],
    dwell_ms: u64,
) -> crate::Result<Vec<Network>> {
    use touchos_rt::{exec, time};
    let mut nets: Vec<Network> = Vec::new();
    for &ch in channels {
        radio.set_channel(ch).await?;
        let end = time::now_ms() + dwell_ms;
        while time::now_ms() < end {
            for n in radio.recv().iter().filter_map(parse_beacon) {
                merge(&mut nets, n);
            }
            if radio.dead() {
                return Err(crate::Error::Hw("radio died while scanning"));
            }
            exec::sleep_ms(2).await;
        }
    }
    nets.sort_by(|a, b| b.rssi.cmp(&a.rssi));
    Ok(nets)
}

#[derive(Debug, Clone)]
pub struct Network {
    pub ssid: String,
    pub bssid: [u8; 6],
    pub channel: u8,
    pub rssi: i8,
    pub security: &'static str,
    /// Raw SSID bytes (the display `ssid` replaces non-printables).
    pub ssid_raw: Vec<u8>,
    pub capability: u16,
    pub beacon_interval: u16,
    pub dtim_period: u8,
    /// Supported + extended rates, as advertised (500 kbit/s units, 0x80 = basic).
    pub rates: Vec<u8>,
    /// RSN group cipher suite selector (e.g. 00-0f-ac:4 = CCMP).
    pub group_cipher: Option<[u8; 4]>,
    /// Does the RSN element offer pairwise CCMP and AKM PSK?
    pub ccmp_psk: bool,
}

/// Beacon / probe response -> network description.
pub fn parse_beacon(f: &RxFrame) -> Option<Network> {
    let d = &f.data;
    if d.len() < 36 {
        return None;
    }
    let (ftype, subtype) = ((d[0] >> 2) & 3, d[0] >> 4);
    if ftype != 0 || !(subtype == 8 || subtype == 5) {
        return None;
    }
    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&d[16..22]);
    let capability = u16::from_le_bytes([d[34], d[35]]);
    let beacon_interval = u16::from_le_bytes([d[32], d[33]]);
    let (mut ssid_raw, mut rates, mut dtim_period) = (Vec::new(), Vec::new(), 1u8);
    let (mut group_cipher, mut ccmp_psk) = (None, false);
    let mut ssid = String::new();
    let mut channel = f.channel;
    let (mut rsn, mut wpa) = (false, false);
    for (id, body) in ies(&d[36..]) {
        let len = body.len();
        match id {
            1 | 50 => rates.extend_from_slice(body),
            5 if len >= 2 => dtim_period = body[1].max(1), // TIM: count, period, ...
            48 if len >= 8 => {
                rsn = true;
                group_cipher = Some([body[2], body[3], body[4], body[5]]);
                // pairwise list, then AKM list
                let np = u16::from_le_bytes([body[6], body[7]]) as usize;
                let pw = body.get(8..8 + 4 * np).unwrap_or(&[]);
                let ccmp = pw.chunks(4).any(|c| c == [0x00, 0x0f, 0xac, 4]);
                let a = 8 + 4 * np;
                let na = body
                    .get(a..a + 2)
                    .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as usize);
                let akm = body.get(a + 2..a + 2 + 4 * na).unwrap_or(&[]);
                let psk = akm.chunks(4).any(|c| c == [0x00, 0x0f, 0xac, 2]);
                ccmp_psk = ccmp && psk;
            }
            0 => {
                ssid_raw = body.to_vec();
                ssid = body
                    .iter()
                    .map(|&c| {
                        if (0x20..0x7f).contains(&c) {
                            c as char
                        } else {
                            '?'
                        }
                    })
                    .collect()
            }
            3 if len == 1 => channel = body[0],
            48 => rsn = true,
            221 if body.starts_with(&[0x00, 0x50, 0xf2, 0x01]) => wpa = true,
            _ => {}
        }
    }
    let security = if rsn {
        "WPA2"
    } else if wpa {
        "WPA"
    } else if capability & (1 << 4) != 0 {
        "WEP"
    } else {
        "open"
    };
    Some(Network {
        ssid,
        bssid,
        channel,
        rssi: f.rssi,
        security,
        ssid_raw,
        capability,
        beacon_interval,
        dtim_period,
        rates,
        group_cipher,
        ccmp_psk,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::RxCrypto;

    fn beacon(bssid: u8, rssi: i8) -> RxFrame {
        let mut d = vec![0x80, 0, 0, 0];
        d.extend_from_slice(&[0xff; 6]);
        d.extend_from_slice(&[2, 0, 0, 0, 0, bssid]);
        d.extend_from_slice(&[2, 0, 0, 0, 0, bssid]);
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&[0; 8]); // timestamp
        d.extend_from_slice(&100u16.to_le_bytes());
        d.extend_from_slice(&0x0431u16.to_le_bytes()); // ESS, privacy, short preamble, short slot
        d.extend_from_slice(&[0, 4, b'h', b'o', b'm', b'e']);
        d.extend_from_slice(&[1, 4, 0x82, 0x84, 0x8b, 0x96]);
        d.extend_from_slice(&[3, 1, 6]);
        d.extend_from_slice(&[5, 4, 0, 3, 0, 0]);
        d.extend_from_slice(&[48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4]);
        d.extend_from_slice(&[1, 0, 0x00, 0x0f, 0xac, 2, 0, 0]);
        RxFrame {
            data: d,
            rssi,
            channel: 1,
            crypto: RxCrypto::None,
        }
    }

    #[test]
    fn parse_and_collect() {
        let n = parse_beacon(&beacon(1, -50)).unwrap();
        assert_eq!(n.ssid, "home");
        assert_eq!(n.channel, 6); // from the DS parameter set
        assert_eq!(n.dtim_period, 3);
        assert_eq!(n.security, "WPA2");
        assert!(n.ccmp_psk);
        assert_eq!(n.group_cipher, Some([0x00, 0x0f, 0xac, 4]));
        let nets = collect([beacon(1, -70), beacon(2, -60), beacon(1, -40)]);
        assert_eq!(nets.len(), 2);
        assert_eq!((nets[0].bssid[5], nets[0].rssi), (1, -40));
        assert_eq!(for_ssid(&nets, b"home").len(), 2);
        assert!(for_ssid(&nets, b"other").is_empty());
    }
}
