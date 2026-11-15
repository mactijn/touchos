//! The Z-Wave Serial API: how a host talks to a Z-Wave controller chip
//! over a UART (on the Toon, a ZW0301 with Sigma's Serial API 4.55
//! static-controller firmware on UART3, 115200 8N1).
//!
//! A data frame:
//!
//! | byte | |
//! |---|---|
//! | 0 | SOF (0x01) |
//! | 1 | LEN: the bytes after it, checksum included |
//! | 2 | type: 0x00 request, 0x01 response |
//! | 3 | function ID ([`func`]) |
//! | 4.. | data |
//! | last | checksum: 0xFF XOR every byte from LEN to the last data byte |
//!
//! Each side acknowledges each data frame with a single byte: ACK (0x06),
//! NAK (0x15, bad checksum) or CAN (0x18, a collision). An unacknowledged
//! frame is sent again (up to 3 times). The host asks with requests; the
//! chip answers some with a response and sends requests of its own
//! (reports from other nodes, [`func::APPLICATION_COMMAND_HANDLER`]).
//!
//! Two kinds of commands, kept apart:
//! - [`Query`]: questions to the chip or, through it, to other nodes
//!   ([`NodeCmd`]: a ping, and Get commands). They change nothing.
//! - [`Manage`]: network management, which changes the chip's network:
//!   a reset to a fresh network, adding and removing a node. These get no
//!   response frame, only callbacks ([`AddRemove`]).

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub const SOF: u8 = 0x01;
pub const ACK: u8 = 0x06;
pub const NAK: u8 = 0x15;
pub const CAN: u8 = 0x18;

/// Function IDs.
pub mod func {
    pub const GET_INIT_DATA: u8 = 0x02;
    pub const APPLICATION_COMMAND_HANDLER: u8 = 0x04;
    pub const GET_CONTROLLER_CAPABILITIES: u8 = 0x05;
    pub const SERIAL_API_GET_CAPABILITIES: u8 = 0x07;
    pub const SEND_DATA: u8 = 0x13;
    pub const GET_VERSION: u8 = 0x15;
    pub const MEMORY_GET_ID: u8 = 0x20;
    pub const GET_NODE_PROTOCOL_INFO: u8 = 0x41;
    pub const SET_DEFAULT: u8 = 0x42;
    pub const ADD_NODE_TO_NETWORK: u8 = 0x4a;
    pub const REMOVE_NODE_FROM_NETWORK: u8 = 0x4b;
    pub const APPLICATION_UPDATE: u8 = 0x49;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Request,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub func: u8,
    pub data: Vec<u8>,
}

/// The checksum over LEN, type, function and data.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0xff, |c, b| c ^ b)
}

impl Frame {
    pub fn request(func: u8, data: &[u8]) -> Frame {
        Frame { kind: Kind::Request, func, data: data.to_vec() }
    }

    /// The bytes on the line, SOF to checksum.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 5);
        out.push(SOF);
        out.push(self.data.len() as u8 + 3);
        out.push(match self.kind {
            Kind::Request => 0x00,
            Kind::Response => 0x01,
        });
        out.push(self.func);
        out.extend_from_slice(&self.data);
        out.push(checksum(&out[1..]));
        out
    }
}

/// What the receiver made of the bytes so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Ack,
    Nak,
    Can,
    /// A data frame with a good checksum: acknowledge it with ACK.
    Frame(Frame),
    /// A data frame with a bad checksum (or an unknown type): answer NAK.
    Bad,
    /// A byte outside a frame that isn't SOF/ACK/NAK/CAN.
    Stray(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Len,
    /// Collecting LEN bytes (type .. checksum).
    Body(usize),
}

/// Takes the line's bytes one at a time. A frame cut off midway is left
/// with [`reset`](Self::reset) (the Serial API's rule: 1.5 s between
/// bytes at most; the caller keeps the time).
pub struct Receiver {
    state: State,
    buf: Vec<u8>,
}

impl Default for Receiver {
    fn default() -> Self {
        Receiver::new()
    }
}

impl Receiver {
    pub fn new() -> Receiver {
        Receiver { state: State::Idle, buf: Vec::new() }
    }

    /// In the middle of a frame?
    pub fn busy(&self) -> bool {
        self.state != State::Idle
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.buf.clear();
    }

    pub fn push(&mut self, b: u8) -> Option<Event> {
        match self.state {
            State::Idle => match b {
                SOF => {
                    self.state = State::Len;
                    None
                }
                ACK => Some(Event::Ack),
                NAK => Some(Event::Nak),
                CAN => Some(Event::Can),
                _ => Some(Event::Stray(b)),
            },
            State::Len => {
                if b < 3 {
                    // Too short to be a frame: not one.
                    self.state = State::Idle;
                    return Some(Event::Bad);
                }
                self.buf.clear();
                self.buf.push(b);
                self.state = State::Body(b as usize);
                None
            }
            State::Body(len) => {
                self.buf.push(b);
                if self.buf.len() < len + 1 {
                    return None;
                }
                self.state = State::Idle;
                let (body, sum) = self.buf.split_at(len);
                let kind = match body[1] {
                    0x00 => Kind::Request,
                    0x01 => Kind::Response,
                    _ => return Some(Event::Bad),
                };
                if checksum(body) != sum[0] {
                    return Some(Event::Bad);
                }
                Some(Event::Frame(Frame { kind, func: body[2], data: body[3..].to_vec() }))
            }
        }
    }
}

/// The questions the host may ask. None of them changes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    Version,
    MemoryId,
    Capabilities,
    InitData,
    ControllerCapabilities,
    NodeProtocolInfo(u8),
    /// Send `cmd` to `node` (ZW_SendData). The chip answers with a
    /// response (queued or not), then a request with `callback` and the
    /// transmit status ([`TxStatus`]).
    Send { node: u8, cmd: NodeCmd, callback: u8 },
}

/// Transmit options: the node must acknowledge; routes may be used.
const TX_ACK_AUTO_ROUTE: u8 = 0x01 | 0x04;

/// Commands to other nodes. None of them changes anything on the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCmd {
    /// An empty frame: is the node there? (The answer is the transmit
    /// status.)
    NoOperation,
    /// Manufacturer Specific Get: the node answers with a report.
    ManufacturerSpecificGet,
    /// Version Get: the protocol and application versions.
    VersionGet,
}

impl NodeCmd {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            NodeCmd::NoOperation => &[0x00],
            NodeCmd::ManufacturerSpecificGet => &[0x72, 0x04],
            NodeCmd::VersionGet => &[0x86, 0x11],
        }
    }
}

/// The transmit status in ZW_SendData's callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxStatus {
    /// The node acknowledged.
    Ok,
    NoAck,
    Fail,
    Other(u8),
}

impl TxStatus {
    pub fn from_byte(b: u8) -> TxStatus {
        match b {
            0 => TxStatus::Ok,
            1 => TxStatus::NoAck,
            2 => TxStatus::Fail,
            b => TxStatus::Other(b),
        }
    }
}

/// Version Report (0x86 0x12): the node's library type, protocol version
/// and application version (V1 fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeVersion {
    pub library: u8,
    pub protocol: (u8, u8),
    pub application: (u8, u8),
}

pub fn version_report(c: &Command) -> Option<NodeVersion> {
    if c.class != 0x86 || c.cmd != Some(0x12) || c.params.len() < 5 {
        return None;
    }
    let p = &c.params;
    Some(NodeVersion { library: p[0], protocol: (p[1], p[2]), application: (p[3], p[4]) })
}

/// Network management: these change the chip's network. The chip answers
/// each with callbacks (requests with the same function and `callback`),
/// not with a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manage {
    /// Forget the whole network: a new home ID, the chip node 1. One
    /// callback when done.
    SetDefault { callback: u8 },
    /// Add the next node that asks to join (its button pressed):
    /// callbacks with [`AddRemoveStatus`].
    AddNodeStart { callback: u8 },
    /// End adding: after PROTOCOL_DONE (a callback with DONE follows), or
    /// to give up (callback 0: no callback).
    AddNodeStop { callback: u8 },
    /// Remove the next node whose button is pressed, whichever network it
    /// is in (it goes back to its factory state).
    RemoveNodeStart { callback: u8 },
    RemoveNodeStop { callback: u8 },
}

/// Add/remove modes: any node, at full power; stop.
const MODE_ANY_HIGH_POWER: u8 = 0x01 | 0x80;
const MODE_STOP: u8 = 0x05;

impl Manage {
    pub fn func(self) -> u8 {
        match self {
            Manage::SetDefault { .. } => func::SET_DEFAULT,
            Manage::AddNodeStart { .. } | Manage::AddNodeStop { .. } => func::ADD_NODE_TO_NETWORK,
            Manage::RemoveNodeStart { .. } | Manage::RemoveNodeStop { .. } => func::REMOVE_NODE_FROM_NETWORK,
        }
    }

    pub fn callback(self) -> u8 {
        match self {
            Manage::SetDefault { callback }
            | Manage::AddNodeStart { callback }
            | Manage::AddNodeStop { callback }
            | Manage::RemoveNodeStart { callback }
            | Manage::RemoveNodeStop { callback } => callback,
        }
    }

    pub fn frame(self) -> Frame {
        let cb = self.callback();
        let data = match self {
            Manage::SetDefault { .. } => alloc::vec![cb],
            Manage::AddNodeStart { .. } | Manage::RemoveNodeStart { .. } => alloc::vec![MODE_ANY_HIGH_POWER, cb],
            Manage::AddNodeStop { .. } | Manage::RemoveNodeStop { .. } => alloc::vec![MODE_STOP, cb],
        };
        Frame { kind: Kind::Request, func: self.func(), data }
    }
}

/// The status in an add/remove callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddRemoveStatus {
    /// Waiting for a node's button.
    Ready,
    NodeFound,
    /// A slave (or controller) is being added/removed; its node ID and
    /// node info are in the callback.
    Slave,
    Controller,
    /// Adding: the protocol part is done; now stop ([`Manage::AddNodeStop`]).
    ProtocolDone,
    Done,
    Failed,
    Other(u8),
}

/// An add/remove callback: the status, the node (its ID in our network
/// when adding; when removing, the ID it had in its old one, 0 if none),
/// and its device classes and command classes when given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddRemove {
    pub callback: u8,
    pub status: AddRemoveStatus,
    pub node: u8,
    pub classes: Option<(u8, u8, u8)>,
    pub command_classes: Vec<u8>,
}

impl AddRemove {
    /// From a request frame of AddNodeToNetwork or RemoveNodeFromNetwork.
    pub fn parse(f: &Frame) -> Option<AddRemove> {
        if f.kind != Kind::Request || !matches!(f.func, func::ADD_NODE_TO_NETWORK | func::REMOVE_NODE_FROM_NETWORK) {
            return None;
        }
        let d = &f.data;
        let status = match *d.get(1)? {
            1 => AddRemoveStatus::Ready,
            2 => AddRemoveStatus::NodeFound,
            3 => AddRemoveStatus::Slave,
            4 => AddRemoveStatus::Controller,
            5 => AddRemoveStatus::ProtocolDone,
            6 => AddRemoveStatus::Done,
            7 => AddRemoveStatus::Failed,
            b => AddRemoveStatus::Other(b),
        };
        let info = d.get(4..4 + *d.get(3).unwrap_or(&0) as usize).unwrap_or(&[]);
        Some(AddRemove {
            callback: d[0],
            status,
            node: d.get(2).copied().unwrap_or(0),
            classes: (info.len() >= 3).then(|| (info[0], info[1], info[2])),
            command_classes: info.get(3..).unwrap_or(&[]).to_vec(),
        })
    }
}

/// A SendData callback (a request from the chip): (callback ID, status).
pub fn send_callback(f: &Frame) -> Option<(u8, TxStatus)> {
    if f.kind != Kind::Request || f.func != func::SEND_DATA {
        return None;
    }
    Some((*f.data.first()?, TxStatus::from_byte(*f.data.get(1)?)))
}

/// Manufacturer Specific Report (0x72 0x05): manufacturer, product type,
/// product ID, from the command's parameters.
pub fn manufacturer_report(c: &Command) -> Option<(u16, u16, u16)> {
    if c.class != 0x72 || c.cmd != Some(0x05) || c.params.len() < 6 {
        return None;
    }
    let p = &c.params;
    Some((u16::from_be_bytes([p[0], p[1]]), u16::from_be_bytes([p[2], p[3]]), u16::from_be_bytes([p[4], p[5]])))
}

impl Query {
    pub fn func(self) -> u8 {
        match self {
            Query::Version => func::GET_VERSION,
            Query::MemoryId => func::MEMORY_GET_ID,
            Query::Capabilities => func::SERIAL_API_GET_CAPABILITIES,
            Query::InitData => func::GET_INIT_DATA,
            Query::ControllerCapabilities => func::GET_CONTROLLER_CAPABILITIES,
            Query::NodeProtocolInfo(_) => func::GET_NODE_PROTOCOL_INFO,
            Query::Send { .. } => func::SEND_DATA,
        }
    }

    pub fn frame(self) -> Frame {
        match self {
            Query::NodeProtocolInfo(node) => Frame::request(self.func(), &[node]),
            Query::Send { node, cmd, callback } => {
                let c = cmd.bytes();
                let mut d = Vec::with_capacity(c.len() + 4);
                d.push(node);
                d.push(c.len() as u8);
                d.extend_from_slice(c);
                d.push(TX_ACK_AUTO_ROUTE);
                d.push(callback);
                Frame { kind: Kind::Request, func: self.func(), data: d }
            }
            _ => Frame::request(self.func(), &[]),
        }
    }
}

/// GetVersion: "Z-Wave 2.78" and the library type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub text: String,
    pub library: u8,
}

impl Version {
    pub fn parse(d: &[u8]) -> Option<Version> {
        let end = d.iter().position(|&b| b == 0)?;
        let text = core::str::from_utf8(&d[..end]).ok()?.into();
        Some(Version { text, library: *d.get(end + 1)? })
    }

    pub fn library_name(&self) -> &'static str {
        match self.library {
            1 => "static controller",
            2 => "controller",
            3 => "enhanced slave",
            4 => "slave",
            5 => "installer",
            6 => "routing slave",
            7 => "bridge controller",
            8 => "device under test",
            _ => "unknown",
        }
    }
}

/// MemoryGetId: the network's home ID and the chip's own node ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryId {
    pub home_id: u32,
    pub node_id: u8,
}

impl MemoryId {
    pub fn parse(d: &[u8]) -> Option<MemoryId> {
        let id: [u8; 4] = d.get(..4)?.try_into().ok()?;
        Some(MemoryId { home_id: u32::from_be_bytes(id), node_id: *d.get(4)? })
    }
}

/// SerialApiGetCapabilities: the firmware's version and maker, and which
/// function IDs it supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub app_version: u8,
    pub app_revision: u8,
    pub manufacturer: u16,
    pub product_type: u16,
    pub product_id: u16,
    funcs: [u8; 32],
}

impl Capabilities {
    pub fn parse(d: &[u8]) -> Option<Capabilities> {
        let u16_at = |i: usize| Some(u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]));
        let mut funcs = [0; 32];
        let have = d.get(8..)?;
        let n = have.len().min(32);
        funcs[..n].copy_from_slice(&have[..n]);
        Some(Capabilities {
            app_version: *d.first()?,
            app_revision: *d.get(1)?,
            manufacturer: u16_at(2)?,
            product_type: u16_at(4)?,
            product_id: u16_at(6)?,
            funcs,
        })
    }

    /// Does the firmware support function `f`? (Bit `f - 1` of the mask.)
    pub fn supports(&self, f: u8) -> bool {
        let i = f.wrapping_sub(1) as usize;
        f != 0 && self.funcs[i / 8] & 1 << (i % 8) != 0
    }

    /// How many function IDs it supports.
    pub fn count(&self) -> u32 {
        self.funcs.iter().map(|b| b.count_ones()).sum()
    }
}

/// SerialApiGetInitData: the API version, the chip's role, the nodes in
/// its network, and the chip type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitData {
    pub api_version: u8,
    pub caps: u8,
    pub nodes: Vec<u8>,
    pub chip_type: Option<u8>,
    pub chip_version: Option<u8>,
}

impl InitData {
    pub const SLAVE_API: u8 = 0x01;
    pub const TIMER_FUNCTIONS: u8 = 0x02;
    pub const SECONDARY: u8 = 0x04;
    pub const SIS: u8 = 0x08;

    pub fn parse(d: &[u8]) -> Option<InitData> {
        let n = *d.get(2)? as usize;
        let bitmap = d.get(3..3 + n)?;
        let nodes = (0..n * 8).filter(|&i| bitmap[i / 8] & 1 << (i % 8) != 0).map(|i| i as u8 + 1).collect();
        Some(InitData {
            api_version: *d.first()?,
            caps: *d.get(1)?,
            nodes,
            chip_type: d.get(3 + n).copied(),
            chip_version: d.get(4 + n).copied(),
        })
    }
}

/// GetControllerCapabilities: the controller's role in its network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerCaps(pub u8);

impl ControllerCaps {
    pub fn parse(d: &[u8]) -> Option<ControllerCaps> {
        d.first().map(|&b| ControllerCaps(b))
    }
    pub fn secondary(self) -> bool {
        self.0 & 0x01 != 0
    }
    pub fn on_other_network(self) -> bool {
        self.0 & 0x02 != 0
    }
    pub fn sis_present(self) -> bool {
        self.0 & 0x04 != 0
    }
    pub fn real_primary(self) -> bool {
        self.0 & 0x08 != 0
    }
    pub fn suc(self) -> bool {
        self.0 & 0x10 != 0
    }
}

/// GetNodeProtocolInfo: what the controller remembers of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeInfo {
    pub capability: u8,
    pub security: u8,
    pub basic: u8,
    pub generic: u8,
    pub specific: u8,
}

impl NodeInfo {
    pub fn parse(d: &[u8]) -> Option<NodeInfo> {
        let d: &[u8; 6] = d.get(..6)?.try_into().ok()?;
        Some(NodeInfo { capability: d[0], security: d[1], basic: d[3], generic: d[4], specific: d[5] })
    }
    /// Basic class 0: the controller doesn't know this node.
    pub fn known(&self) -> bool {
        self.basic != 0
    }
    /// Always listening (mains powered), as opposed to asleep between
    /// wake-ups.
    pub fn listening(&self) -> bool {
        self.capability & 0x80 != 0
    }
    pub fn routing(&self) -> bool {
        self.capability & 0x40 != 0
    }
    /// Frequently listening (FLiRS: wakes on a beam).
    pub fn flirs(&self) -> bool {
        self.security & 0x60 != 0
    }
    pub fn protocol_version(&self) -> u8 {
        (self.capability & 0x07) + 1
    }
}

/// A report from another node (ApplicationCommandHandler, a request from
/// the chip): the source and the command (class, command, parameters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub rx_status: u8,
    pub node: u8,
    pub class: u8,
    pub cmd: Option<u8>,
    pub params: Vec<u8>,
}

impl Command {
    pub fn parse(d: &[u8]) -> Option<Command> {
        let n = *d.get(2)? as usize;
        let c = d.get(3..3 + n)?;
        Some(Command {
            rx_status: *d.first()?,
            node: *d.get(1)?,
            class: *c.first()?,
            cmd: c.get(1).copied(),
            params: c.get(2..).unwrap_or(&[]).to_vec(),
        })
    }
}

pub fn basic_class_name(c: u8) -> &'static str {
    match c {
        0x01 => "controller",
        0x02 => "static controller",
        0x03 => "slave",
        0x04 => "routing slave",
        _ => "?",
    }
}

pub fn generic_class_name(c: u8) -> &'static str {
    match c {
        0x01 => "remote controller",
        0x02 => "static controller",
        0x03 => "AV control point",
        0x04 => "display",
        0x07 => "notification sensor",
        0x08 => "thermostat",
        0x09 => "window covering",
        0x0f => "repeater slave",
        0x10 => "binary switch",
        0x11 => "multilevel switch",
        0x12 => "remote switch",
        0x13 => "toggle switch",
        0x15 => "Z/IP node",
        0x16 => "ventilation",
        0x17 => "security panel",
        0x18 => "wall controller",
        0x20 => "binary sensor",
        0x21 => "multilevel sensor",
        0x30 => "pulse meter",
        0x31 => "meter",
        0x40 => "entry control",
        0x50 => "semi-interoperable",
        0xa1 => "alarm sensor",
        0xff => "non-interoperable",
        _ => "?",
    }
}

pub fn command_class_name(c: u8) -> &'static str {
    match c {
        0x20 => "Basic",
        0x25 => "Switch Binary",
        0x26 => "Switch Multilevel",
        0x27 => "Switch All",
        0x30 => "Sensor Binary",
        0x31 => "Sensor Multilevel",
        0x32 => "Meter",
        0x35 => "Meter Pulse",
        0x3c => "Meter Table Config",
        0x3d => "Meter Table Monitor",
        0x3e => "Meter Table Push",
        0x40 => "Thermostat Mode",
        0x43 => "Thermostat Setpoint",
        0x5a => "Device Reset Locally",
        0x5e => "Z-Wave Plus Info",
        0x60 => "Multi Channel",
        0x70 => "Configuration",
        0x71 => "Notification",
        0x72 => "Manufacturer Specific",
        0x73 => "Powerlevel",
        0x80 => "Battery",
        0x84 => "Wake Up",
        0x85 => "Association",
        0x86 => "Version",
        0x8e => "Multi Channel Association",
        0x98 => "Security",
        0x9f => "Security 2",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn feed(rx: &mut Receiver, bytes: &[u8]) -> Vec<Event> {
        bytes.iter().filter_map(|&b| rx.push(b)).collect()
    }

    fn response(func: u8, data: &[u8]) -> Vec<u8> {
        Frame { kind: Kind::Response, func, data: data.to_vec() }.encode()
    }

    #[test]
    fn queries_on_the_line() {
        // Known Serial API byte sequences.
        assert_eq!(Query::Version.frame().encode(), [0x01, 0x03, 0x00, 0x15, 0xe9]);
        assert_eq!(Query::InitData.frame().encode(), [0x01, 0x03, 0x00, 0x02, 0xfe]);
        assert_eq!(Query::MemoryId.frame().encode(), [0x01, 0x03, 0x00, 0x20, 0xdc]);
        assert_eq!(Query::Capabilities.frame().encode(), [0x01, 0x03, 0x00, 0x07, 0xfb]);
        assert_eq!(Query::ControllerCapabilities.frame().encode(), [0x01, 0x03, 0x00, 0x05, 0xf9]);
        assert_eq!(Query::NodeProtocolInfo(2).frame().encode(), [0x01, 0x04, 0x00, 0x41, 0x02, 0xb8]);
    }

    #[test]
    fn receive_acks_and_a_frame_in_pieces() {
        let mut rx = Receiver::new();
        let f = response(func::MEMORY_GET_ID, &[0xc0, 0xff, 0xee, 0x42, 0x01]);
        let mut line = vec![ACK];
        line.extend_from_slice(&f);
        let (a, b) = line.split_at(4);
        let mut ev = feed(&mut rx, a);
        assert!(rx.busy());
        ev.extend(feed(&mut rx, b));
        assert!(!rx.busy());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0], Event::Ack);
        let Event::Frame(fr) = &ev[1] else { panic!("{ev:?}") };
        assert_eq!(fr.kind, Kind::Response);
        assert_eq!(MemoryId::parse(&fr.data), Some(MemoryId { home_id: 0xc0ff_ee42, node_id: 1 }));
    }

    #[test]
    fn bad_checksum_and_stray_bytes() {
        let mut rx = Receiver::new();
        let mut f = response(func::GET_VERSION, &[0x41, 0x00, 0x01]);
        *f.last_mut().unwrap() ^= 0x55;
        assert_eq!(feed(&mut rx, &f), [Event::Bad]);
        assert_eq!(feed(&mut rx, &[0x7f, NAK, CAN]), [Event::Stray(0x7f), Event::Nak, Event::Can]);
        // An unknown type is not a frame either.
        let mut g = response(func::GET_VERSION, &[]);
        g[2] = 0x05;
        let n = g.len() - 1;
        g[n] = checksum(&g[1..n]);
        assert_eq!(feed(&mut rx, &g), [Event::Bad]);
        // A frame cut off: reset, and the next one is whole.
        let h = response(func::GET_VERSION, &[0x41, 0x00, 0x01]);
        assert!(feed(&mut rx, &h[..3]).is_empty());
        rx.reset();
        assert!(matches!(feed(&mut rx, &h)[..], [Event::Frame(_)]));
    }

    #[test]
    fn version() {
        let mut d = b"Z-Wave 2.78\0".to_vec();
        d.push(0x01);
        let v = Version::parse(&d).unwrap();
        assert_eq!(v.text, "Z-Wave 2.78");
        assert_eq!(v.library_name(), "static controller");
        assert_eq!(Version::parse(b"no nul"), None);
    }

    #[test]
    fn capabilities() {
        let mut d = vec![4, 55, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01];
        let mut mask = [0u8; 32];
        for f in [0x02u8, 0x15, 0x20, 0x41] {
            mask[(f as usize - 1) / 8] |= 1 << ((f - 1) % 8);
        }
        d.extend_from_slice(&mask);
        let c = Capabilities::parse(&d).unwrap();
        assert_eq!((c.app_version, c.app_revision, c.product_type, c.product_id), (4, 55, 1, 1));
        assert!(c.supports(0x41) && c.supports(0x02) && !c.supports(0x03) && !c.supports(0));
        assert_eq!(c.count(), 4);
    }

    #[test]
    fn init_data_nodes() {
        // Nodes 1, 2 and 232 (the last bit of a 29-byte map), chip ZW0301.
        let mut d = vec![5, 0x08, 29];
        let mut map = [0u8; 29];
        map[0] = 0b11;
        map[28] = 0x80;
        d.extend_from_slice(&map);
        d.extend_from_slice(&[3, 1]);
        let i = InitData::parse(&d).unwrap();
        assert_eq!(i.nodes, [1, 2, 232]);
        assert_eq!((i.api_version, i.chip_type, i.chip_version), (5, Some(3), Some(1)));
        assert!(i.caps & InitData::SIS != 0);
    }

    #[test]
    fn node_info_and_controller_caps() {
        // A listening routing slave, a meter.
        let n = NodeInfo::parse(&[0xd3, 0x1c, 0x00, 0x04, 0x31, 0x01]).unwrap();
        assert!(n.known() && n.listening() && n.routing() && !n.flirs());
        assert_eq!((n.protocol_version(), generic_class_name(n.generic)), (4, "meter"));
        assert!(!NodeInfo::parse(&[0; 6]).unwrap().known());
        let c = ControllerCaps(0x1c);
        assert!(!c.secondary() && c.sis_present() && c.real_primary() && c.suc());
    }

    #[test]
    fn send_data_to_a_node() {
        // NoOperation to node 2, ACK + auto route, callback 0x2a.
        let q = Query::Send { node: 2, cmd: NodeCmd::NoOperation, callback: 0x2a };
        let e = q.frame().encode();
        assert_eq!(e[..9], [0x01, 0x08, 0x00, 0x13, 0x02, 0x01, 0x00, 0x05, 0x2a]);
        assert_eq!(e[9], checksum(&e[1..9]));
        let q = Query::Send { node: 2, cmd: NodeCmd::ManufacturerSpecificGet, callback: 1 };
        assert_eq!(q.frame().data, [0x02, 0x02, 0x72, 0x04, 0x05, 0x01]);
        // The callback: ID 0x2a, no ACK from the node.
        let cb = Frame::request(func::SEND_DATA, &[0x2a, 0x01, 0x00, 0x10]);
        assert_eq!(send_callback(&cb), Some((0x2a, TxStatus::NoAck)));
        let res = Frame { kind: Kind::Response, func: func::SEND_DATA, data: alloc::vec![1] };
        assert_eq!(send_callback(&res), None);
    }

    #[test]
    fn manufacturer_report_from_a_node() {
        let c = Command::parse(&[0x00, 0x02, 0x08, 0x72, 0x05, 0x01, 0x28, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(manufacturer_report(&c), Some((0x0128, 0, 0)));
        let short = Command::parse(&[0x00, 0x02, 0x03, 0x72, 0x05, 0x01]).unwrap();
        assert_eq!(manufacturer_report(&short), None);
    }

    #[test]
    fn version_report_from_a_node() {
        // An Eneco meter adapter's answer.
        let c = Command::parse(&[0x00, 0x02, 0x07, 0x86, 0x12, 0x03, 0x03, 0x43, 0x01, 0x23]).unwrap();
        let v = version_report(&c).unwrap();
        assert_eq!((v.library, v.protocol, v.application), (3, (3, 0x43), (1, 0x23)));
    }

    #[test]
    fn network_management_frames() {
        assert_eq!(Manage::SetDefault { callback: 7 }.frame().encode()[..5], [0x01, 0x04, 0x00, 0x42, 0x07]);
        assert_eq!(Manage::AddNodeStart { callback: 3 }.frame().data, [0x81, 3]);
        assert_eq!(Manage::AddNodeStop { callback: 0 }.frame().data, [0x05, 0]);
        let r = Manage::RemoveNodeStart { callback: 9 }.frame();
        assert_eq!((r.func, r.data.as_slice()), (0x4b, &[0x81u8, 9][..]));
    }

    #[test]
    fn add_node_callbacks() {
        let ready = Frame::request(func::ADD_NODE_TO_NETWORK, &[3, 0x01, 0x00, 0x00]);
        let a = AddRemove::parse(&ready).unwrap();
        assert_eq!((a.callback, a.status, a.node, a.classes), (3, AddRemoveStatus::Ready, 0, None));
        // Adding a slave: node 2, a routing slave meter with three CCs.
        let slave = Frame::request(func::ADD_NODE_TO_NETWORK, &[3, 0x03, 0x02, 0x06, 0x04, 0x31, 0x02, 0x32, 0x72, 0x86]);
        let a = AddRemove::parse(&slave).unwrap();
        assert_eq!((a.status, a.node, a.classes), (AddRemoveStatus::Slave, 2, Some((0x04, 0x31, 0x02))));
        assert_eq!(a.command_classes, [0x32, 0x72, 0x86]);
        // Short callbacks (no length byte) still parse.
        let done = Frame::request(func::REMOVE_NODE_FROM_NETWORK, &[9, 0x06]);
        assert_eq!(AddRemove::parse(&done).unwrap().status, AddRemoveStatus::Done);
        assert_eq!(AddRemove::parse(&Frame::request(func::SEND_DATA, &[1, 0])), None);
    }

    #[test]
    fn command_from_a_node() {
        // Node 2, Meter Report (0x32 0x02) with three parameter bytes.
        let c = Command::parse(&[0x00, 0x02, 0x05, 0x32, 0x02, 0x21, 0x44, 0x00]).unwrap();
        assert_eq!((c.node, c.class, c.cmd), (2, 0x32, Some(0x02)));
        assert_eq!(c.params, [0x21, 0x44, 0x00]);
        assert_eq!(command_class_name(c.class), "Meter");
        assert_eq!(Command::parse(&[0x00, 0x02, 0x09, 0x32]), None);
    }
}
