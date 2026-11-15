//! OpenTherm frames (the protocol between a thermostat and a boiler,
//! version 2.2 of the public specification): parsing and checking a
//! frame, the names of the data IDs, and their values decoded.
//!
//! A frame is 32 bits, most significant first:
//!
//! | bits | |
//! |---|---|
//! | 31 | parity: the frame has an even number of 1 bits |
//! | 30-28 | message type ([`MsgType`]) |
//! | 27-24 | spare (0) |
//! | 23-16 | data ID ([`DataId`]) |
//! | 15-0 | data value (two bytes; meaning by data ID) |
//!
//! The thermostat (master) sends READ-DATA or WRITE-DATA; the boiler
//! (slave) answers READ-ACK, WRITE-ACK, DATA-INVALID or UNKNOWN-DATAID.
//!
//! On the Toon the boiler is behind the Ketelmodule, on UART2: how the
//! module wraps these frames on that line isn't known yet (servers/boiler
//! records it). This crate only knows OpenTherm itself, and touchos only
//! reads: nothing here builds a WRITE-DATA frame.

#![no_std]

/// The message type (bits 30-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    ReadData,
    WriteData,
    InvalidData,
    ReadAck,
    WriteAck,
    DataInvalid,
    UnknownDataId,
    /// 3: reserved.
    Reserved,
}

impl MsgType {
    fn from_bits(b: u32) -> MsgType {
        match b & 7 {
            0 => MsgType::ReadData,
            1 => MsgType::WriteData,
            2 => MsgType::InvalidData,
            3 => MsgType::Reserved,
            4 => MsgType::ReadAck,
            5 => MsgType::WriteAck,
            6 => MsgType::DataInvalid,
            _ => MsgType::UnknownDataId,
        }
    }

    /// Sent by the thermostat (the master), not the boiler.
    pub fn from_master(self) -> bool {
        matches!(self, MsgType::ReadData | MsgType::WriteData | MsgType::InvalidData | MsgType::Reserved)
    }
}

/// One frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub msg: MsgType,
    pub id: u8,
    pub value: u16,
}

/// Why a 32-bit word isn't a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An odd number of 1 bits.
    Parity,
    /// The spare bits (27-24) aren't 0.
    Spare,
}

impl Frame {
    /// Check and split a frame.
    pub fn parse(w: u32) -> Result<Frame, Error> {
        if w.count_ones() % 2 != 0 {
            return Err(Error::Parity);
        }
        if (w >> 24) & 0xf != 0 {
            return Err(Error::Spare);
        }
        Ok(Frame { msg: MsgType::from_bits(w >> 28), id: (w >> 16) as u8, value: w as u16 })
    }

    /// The data ID, if this crate knows it.
    pub fn data_id(&self) -> Option<DataId> {
        DataId::from_id(self.id)
    }

    /// The value as the data ID defines it (None for IDs not known here).
    pub fn reading(&self) -> Option<Reading> {
        let id = self.data_id()?;
        Some(match id.kind() {
            Kind::F88 => Reading::Number(f88(self.value)),
            Kind::U16 => Reading::Count(self.value),
            Kind::S16 => Reading::Number(self.value as i16 as f32),
            Kind::Flags => Reading::Flags { hi: (self.value >> 8) as u8, lo: self.value as u8 },
            Kind::U8Pair => Reading::Pair((self.value >> 8) as u8, self.value as u8),
        })
    }
}

/// A signed fixed-point value with 8 fraction bits (temperatures, °C;
/// percentages; bar; l/min).
pub fn f88(v: u16) -> f32 {
    v as i16 as f32 / 256.0
}

/// How a data ID's two bytes are to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    F88,
    U16,
    S16,
    /// Two bytes of flags (high byte, low byte).
    Flags,
    /// Two separate bytes.
    U8Pair,
}

/// A decoded value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    Number(f32),
    Count(u16),
    Flags { hi: u8, lo: u8 },
    Pair(u8, u8),
}

macro_rules! data_ids {
    ($($name:ident = $id:literal, $kind:ident, $unit:literal, $what:literal;)*) => {
        /// The data IDs touchos knows (the ones a boiler's state is read
        /// from; the specification has more).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DataId { $($name),* }

        impl DataId {
            pub fn from_id(id: u8) -> Option<DataId> {
                match id { $($id => Some(DataId::$name),)* _ => None }
            }
            pub fn id(self) -> u8 {
                match self { $(DataId::$name => $id),* }
            }
            pub fn kind(self) -> Kind {
                match self { $(DataId::$name => Kind::$kind),* }
            }
            /// The unit ("" for none).
            pub fn unit(self) -> &'static str {
                match self { $(DataId::$name => $unit),* }
            }
            /// What it is, in words.
            pub fn what(self) -> &'static str {
                match self { $(DataId::$name => $what),* }
            }
        }
    };
}

data_ids! {
    Status = 0, Flags, "", "status (high: thermostat, low: boiler)";
    ControlSetpoint = 1, F88, "°C", "control setpoint (flow temperature asked for)";
    FaultFlags = 5, Flags, "", "application-specific fault flags and OEM fault code";
    MaxRelModulation = 14, F88, "%", "maximum relative modulation";
    RoomSetpoint = 16, F88, "°C", "room setpoint";
    RelModulation = 17, F88, "%", "relative modulation level";
    ChPressure = 18, F88, "bar", "central heating water pressure";
    DhwFlowRate = 19, F88, "l/min", "hot water flow rate";
    RoomTemperature = 24, F88, "°C", "room temperature";
    BoilerWaterTemperature = 25, F88, "°C", "boiler flow water temperature";
    DhwTemperature = 26, F88, "°C", "hot water temperature";
    OutsideTemperature = 27, F88, "°C", "outside temperature";
    ReturnWaterTemperature = 28, F88, "°C", "return water temperature";
    ExhaustTemperature = 33, S16, "°C", "exhaust temperature";
    DhwSetpoint = 56, F88, "°C", "hot water setpoint";
    MaxChSetpoint = 57, F88, "°C", "maximum central heating water setpoint";
    OpenThermVersionSlave = 125, F88, "", "OpenTherm version of the boiler";
    BurnerStarts = 116, U16, "", "burner starts";
    ChPumpStarts = 117, U16, "", "central heating pump starts";
    BurnerHours = 120, U16, "h", "burner operation hours";
    SlaveVersion = 127, U8Pair, "", "boiler product type and version";
}

/// The boiler's status bits (low byte of [`DataId::Status`]).
pub mod slave_status {
    pub const FAULT: u8 = 1 << 0;
    pub const CH_ACTIVE: u8 = 1 << 1;
    pub const DHW_ACTIVE: u8 = 1 << 2;
    pub const FLAME: u8 = 1 << 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with its parity bit set right.
    fn frame(msg: u32, id: u8, value: u16) -> u32 {
        let w = msg << 28 | (id as u32) << 16 | value as u32;
        if w.count_ones() % 2 == 1 { w | 1 << 31 } else { w }
    }

    #[test]
    fn frames_parse_and_decode() {
        // READ-ACK, boiler water temperature 45.5 °C.
        let f = Frame::parse(frame(4, 25, 0x2d80)).unwrap();
        assert_eq!((f.msg, f.data_id()), (MsgType::ReadAck, Some(DataId::BoilerWaterTemperature)));
        assert_eq!(f.reading(), Some(Reading::Number(45.5)));
        assert!(!f.msg.from_master());
        // A negative outside temperature (-3.25 °C).
        let f = Frame::parse(frame(4, 27, (-3.25f32 * 256.0) as i16 as u16)).unwrap();
        assert_eq!(f.reading(), Some(Reading::Number(-3.25)));
        // READ-DATA status from the thermostat: CH enabled (high byte bit 0).
        let f = Frame::parse(frame(0, 0, 0x0100)).unwrap();
        assert!(f.msg.from_master());
        assert_eq!(f.reading(), Some(Reading::Flags { hi: 1, lo: 0 }));
        // Burner hours, a plain count.
        assert_eq!(Frame::parse(frame(4, 120, 5000)).unwrap().reading(), Some(Reading::Count(5000)));
        // An ID this crate doesn't name still parses.
        let f = Frame::parse(frame(7, 200, 0)).unwrap();
        assert_eq!((f.msg, f.data_id(), f.reading()), (MsgType::UnknownDataId, None, None));
    }

    #[test]
    fn damaged_frames_are_refused() {
        let good = frame(4, 25, 0x2d80);
        assert_eq!(Frame::parse(good ^ 1), Err(Error::Parity));
        assert_eq!(Frame::parse(good ^ (3 << 24)), Err(Error::Spare));
    }

    #[test]
    fn data_ids_round_trip() {
        for id in 0..=255u8 {
            if let Some(d) = DataId::from_id(id) {
                assert_eq!(d.id(), id);
                assert!(!d.what().is_empty());
            }
        }
    }
}
