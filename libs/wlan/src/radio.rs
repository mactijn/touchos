//! What the station needs from a Wi-Fi chip.
//!
//! A chip driver implements [`Radio`]; [`crate::station`] drives it. The
//! split follows what differs between chips: the Intel 3165's firmware scans
//! by itself, fills the duration field and does CCMP for unicast frames with
//! the keys we give it, while a soft-MAC (the RT5370) sends and receives
//! frames on the channel it is tuned to and encrypts the ones we send.
//! [`Caps`] tells the station which of that it has to do itself.

use alloc::string::String;
use alloc::vec::Vec;

use crate::scan::Network;
use crate::{Error, Result};

/// Who does CCMP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    /// The chip encrypts unicast data frames we send (the key comes with each
    /// frame, see [`TxKey`]) and decrypts unicast frames it receives
    /// ([`RxCrypto::Decrypted`]) once [`Radio::install_ptk`] has run.
    /// Group-addressed frames the chip leaves encrypted are decrypted here.
    Hardware,
    /// The chip encrypts unicast data frames we send (as with `Hardware`),
    /// but every received frame is decrypted here. For a chip that strips
    /// the CCMP header when it decrypts (the RT5370): without the packet
    /// number there would be no replay check.
    HardwareTx,
    /// The host does CCMP for everything: frames are sent already encrypted,
    /// and received ones arrive as they were on the air.
    Software,
}

/// What the chip leaves to the host.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub keys: KeyMode,
    /// The duration field of frames we send must be filled in by the host
    /// ([`crate::ieee80211::ack_duration`]).
    pub host_duration: bool,
}

/// A received 802.11 frame (no FCS) and what the chip knows about it.
pub struct RxFrame {
    pub data: Vec<u8>,
    /// Signal strength (dBm).
    pub rssi: i8,
    /// The channel it was received on.
    pub channel: u8,
    pub crypto: RxCrypto,
}

/// What the chip did about encryption on a received frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxCrypto {
    /// Nothing: the frame is as it was on the air (encrypted if Protected
    /// is set).
    None,
    /// Decrypted and the MIC checked by the chip. Protected is still set and
    /// the 8-byte CCMP header (with the PN) is still in place; `mic_len` bytes
    /// of MIC (0 or 8) follow the body.
    Decrypted { mic_len: usize },
    /// The chip tried and failed (bad MIC, no key): drop the frame.
    Failed,
}

/// The pairwise key and packet number for a data frame the chip encrypts
/// ([`KeyMode::Hardware`]).
#[derive(Clone, Copy)]
pub struct TxKey<'a> {
    pub tk: &'a [u8; 16],
    pub pn: u64,
    pub key_id: u8,
}

/// A Wi-Fi chip in station mode. All methods are polled: none blocks for
/// longer than the chip needs to take a command.
pub trait Radio {
    /// Our MAC address.
    fn mac(&self) -> [u8; 6];

    fn caps(&self) -> Caps;

    /// The chip (or its firmware) has died: the station gives up and the
    /// driver starts over or stops.
    fn dead(&self) -> bool;

    /// Look for networks: one entry per BSSID, strongest first. A soft-MAC
    /// can implement it with [`crate::scan::host_scan`].
    async fn scan(&mut self) -> Result<Vec<Network>>;

    /// Tune to a channel and receive everything on it (used by
    /// [`crate::scan::host_scan`]; chips that scan by themselves needn't).
    async fn set_channel(&mut self, _channel: u8) -> Result<()> {
        Err(Error::Hw("this radio can't be tuned by the host"))
    }

    /// Get ready to authenticate with `bss`: its channel, BSSID and rates;
    /// from now on frames from it are received.
    async fn join(&mut self, bss: &Network) -> Result<()>;

    /// The access point accepted our association (AID `aid`); `ht`: as an
    /// 802.11n station (it had `Network::ht`, and took us as a QoS
    /// station), so data may go at HT rates.
    async fn associated(&mut self, aid: u16, ht: bool) -> Result<()>;

    /// Send a management frame (24-byte header + body) to the BSS.
    async fn send_mgmt(&mut self, frame: &[u8]) -> Result<()>;

    /// Send a data frame (header + body: 24 bytes, or 26 for QoS data).
    /// With `key`, Protected is set and the chip adds the CCMP header and
    /// MIC; without, the frame is sent as it is (plain, or already
    /// encrypted by the host).
    async fn send_data(&mut self, frame: &[u8], key: Option<TxKey<'_>>) -> Result<()>;

    /// The frames received since the last call (none: an empty Vec).
    fn recv(&mut self) -> Vec<RxFrame>;

    /// A notification bit (of this process) that is signalled when the
    /// radio has news: a frame received, a transfer done. The station then
    /// sleeps until it (or a frame to send) comes, instead of polling
    /// `recv` every millisecond. 0: none, poll.
    fn event_bit(&self) -> usize {
        0
    }

    /// Install the pairwise key (after message 4 of the handshake has gone).
    async fn install_ptk(&mut self, _tk: &[u8; 16]) -> Result<()> {
        Ok(())
    }

    /// Install a group key (after the handshake, and on every renewal).
    async fn install_gtk(&mut self, _key_id: u8, _gtk: &[u8; 16]) -> Result<()> {
        Ok(())
    }

    /// The chip's recent TX results, for the log when a frame goes unanswered.
    fn tx_report(&self) -> String {
        String::new()
    }

    /// Leave the BSS and get back to the state after bring-up, for a fresh
    /// attempt.
    async fn reset(&mut self) -> Result<()>;
}
