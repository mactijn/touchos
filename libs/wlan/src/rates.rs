//! The legacy (802.11a/b/g) rates, in 500 kbit/s units as they appear in
//! the Supported Rates element (bit 7 set: a basic rate of the BSS), the
//! HT (802.11n) MCS of one spatial stream, and the rates a station sends
//! at.
//!
//! The chip drivers map these to their own encodings (PLCP values, rate
//! indexes); which rates exist and which one to use is decided here, once.

use alloc::vec::Vec;
use core::fmt;

/// The DSSS/CCK rates (802.11b): 1, 2, 5.5, 11 Mbit/s.
pub const CCK: [u8; 4] = [2, 4, 11, 22];
/// The OFDM rates (802.11a/g): 6 to 54 Mbit/s.
pub const OFDM: [u8; 8] = [12, 18, 24, 36, 48, 72, 96, 108];
/// Bit 7 of a Supported Rates entry: a basic rate.
pub const BASIC: u8 = 0x80;

/// 24 Mbit/s: where data frames start. Fast enough for the panel, and
/// robust enough that most links take it (rate control moves from there).
const DATA_START: u8 = 48;

/// A rate without its basic-rate bit.
pub fn value(r: u8) -> u8 {
    r & !BASIC
}

pub fn is_cck(r: u8) -> bool {
    CCK.contains(&value(r))
}

pub fn is_ofdm(r: u8) -> bool {
    OFDM.contains(&value(r))
}

/// One of the legacy rates (not an HT/BSS-membership selector).
pub fn is_legacy(r: u8) -> bool {
    is_cck(r) || is_ofdm(r)
}

/// The lowest rate every station of the band has: 1 Mbit/s on 2.4 GHz, 6
/// Mbit/s on 5 GHz (channels above 14).
pub fn band_lowest(channel: u8) -> u8 {
    if channel > 14 { 12 } else { 2 }
}

/// The BSS's lowest basic legacy rate; the band's lowest if it lists none.
///
/// This is the rate a station sends management frames (and the handshake's
/// unprotected EAPOL frames) at, as mac80211 does: every station of the BSS
/// must receive it, so frames reach the access point wherever its beacons
/// reach us. It is also the rate the access point ACKs at as far as we can
/// tell, which the duration field needs.
pub fn lowest_basic(rates: &[u8], channel: u8) -> u8 {
    rates
        .iter()
        .filter(|&&r| r & BASIC != 0 && is_legacy(r))
        .map(|&r| value(r))
        .min()
        .unwrap_or(band_lowest(channel))
}

/// The rate data frames start at: 24 Mbit/s, or the fastest OFDM rate
/// below it the BSS has; on an 802.11b network its fastest rate.
pub fn data_rate(rates: &[u8]) -> u8 {
    let legacy = || rates.iter().map(|&r| value(r)).filter(|&r| is_legacy(r));
    legacy()
        .filter(|&r| is_ofdm(r) && r <= DATA_START)
        .max()
        .or_else(|| legacy().max())
        .unwrap_or(2)
}

/// The rates rate control may pick for data frames, slowest first: the
/// BSS's OFDM rates (CCK is slower and costs more airtime at every step),
/// or on an 802.11b network its CCK rates. Never empty, and it holds
/// [`data_rate`].
pub fn data_rates(rates: &[u8]) -> Vec<u8> {
    let has = |r: &u8| rates.iter().any(|&x| value(x) == *r);
    let mut set: Vec<u8> = OFDM.iter().copied().filter(has).collect();
    if set.is_empty() {
        set = CCK.iter().copied().filter(has).collect();
    }
    if set.is_empty() {
        set.push(2);
    }
    set
}

/// A rate to send at: legacy (500 kbit/s units, without the basic bit) or
/// an HT MCS (0-7: one spatial stream, 20 MHz). Ordered slowest first
/// within each kind (every legacy rate sorts before every MCS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rate {
    Legacy(u8),
    Mcs(u8),
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Rate::Legacy(r) => write!(f, "{} Mbit/s", Mbit(r)),
            Rate::Mcs(m) => write!(f, "MCS {m}"),
        }
    }
}

/// MCS 4 (39 Mbit/s at 20 MHz, 43.3 with a short guard interval): where HT
/// data starts, the HT counterpart of [`DATA_START`]'s 24 Mbit/s: one step
/// above the 26 Mbit/s of MCS 3.
const HT_DATA_START: u8 = 4;

/// The MCS rate control may pick, slowest first: MCS 0-7 that the access
/// point can receive (`rx_mcs`: bit n = MCS n, the first byte of its HT
/// Capabilities' MCS set). Never empty: an access point that lists none
/// gets MCS 0, which every HT station has.
pub fn ht_rates(rx_mcs: u8) -> Vec<Rate> {
    let v: Vec<Rate> = (0..8).filter(|m| rx_mcs & 1 << m != 0).map(Rate::Mcs).collect();
    if v.is_empty() { alloc::vec![Rate::Mcs(0)] } else { v }
}

/// The MCS HT data starts at.
pub fn ht_data_rate() -> Rate {
    Rate::Mcs(HT_DATA_START)
}

/// A rate in Mbit/s, for the log ("5.5", "24").
pub struct Mbit(pub u8);

impl fmt::Display for Mbit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let r = value(self.0);
        write!(f, "{}{}", r / 2, if r % 2 == 1 { ".5" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typical 802.11g access point: b rates basic, g rates extended.
    const G: [u8; 12] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c];

    #[test]
    fn kinds() {
        assert!(is_cck(0x82) && !is_ofdm(0x82));
        assert!(is_ofdm(0x6c) && !is_cck(0x6c));
        assert!(!is_legacy(0xff)); // the HT PHY membership selector (127 | basic)
    }

    #[test]
    fn lowest_basic_rate() {
        assert_eq!(lowest_basic(&G, 6), 2);
        assert_eq!(lowest_basic(&[0x0c, 0x98, 0x30], 6), 24); // only 12 Mbit/s basic
        assert_eq!(lowest_basic(&[0x0c, 0x18], 6), 2); // none basic: the band's lowest
        assert_eq!(lowest_basic(&[0x0c, 0x18], 36), 12);
        assert_eq!(lowest_basic(&[0xff, 0x96], 1), 22); // the selector is no rate
    }

    #[test]
    fn data_rates_for_data() {
        assert_eq!(data_rate(&G), 48);
        assert_eq!(data_rate(&[0x82, 0x84, 0x0c, 0x12, 0x18, 0x24]), 36); // 18 Mbit/s at most
        assert_eq!(data_rate(&[0x82, 0x84, 0x8b, 0x96]), 22); // 802.11b
        assert_eq!(data_rate(&[]), 2);
        assert_eq!(data_rates(&G), OFDM.to_vec());
        assert_eq!(data_rates(&[0x82, 0x84, 0x8b, 0x96]), CCK.to_vec());
        assert_eq!(data_rates(&[0xff]), [2]);
        for r in [&G[..], &[0x82, 0x0c, 0x24], &[0x82, 0x84], &[]] {
            assert!(data_rates(r).contains(&data_rate(r)));
        }
    }

    #[test]
    fn ht() {
        assert_eq!(ht_rates(0xff), (0..8).map(Rate::Mcs).collect::<Vec<_>>());
        assert_eq!(ht_rates(0x0f), (0..4).map(Rate::Mcs).collect::<Vec<_>>());
        assert_eq!(ht_rates(0), [Rate::Mcs(0)]);
        assert!(Rate::Legacy(108) < Rate::Mcs(0) && Rate::Mcs(1) < Rate::Mcs(2));
        assert_eq!(format!("{}", Rate::Mcs(7)), "MCS 7");
        assert_eq!(format!("{}", Rate::Legacy(11)), "5.5 Mbit/s");
    }

    #[test]
    fn mbit() {
        assert_eq!(format!("{}", Mbit(11)), "5.5");
        assert_eq!(format!("{}", Mbit(0x96)), "11");
        assert_eq!(format!("{}", Mbit(108)), "54");
    }
}
