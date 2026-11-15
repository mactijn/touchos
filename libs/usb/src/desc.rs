//! USB descriptor parsing.

use alloc::vec::Vec;

#[derive(Debug, Clone, Default)]
pub struct DeviceDesc {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub i_manufacturer: u8,
    pub i_product: u8,
    pub num_configs: u8,
}

impl DeviceDesc {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 18 || b[1] != 1 {
            return None;
        }
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        Some(DeviceDesc {
            usb_version: u16_at(2),
            class: b[4],
            subclass: b[5],
            protocol: b[6],
            max_packet0: b[7],
            vendor: u16_at(8),
            product: u16_at(10),
            i_manufacturer: b[14],
            i_product: b[15],
            num_configs: b[17],
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Endpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    pub fn number(&self) -> u8 {
        self.address & 0x0f
    }
    /// 0 control, 1 isochronous, 2 bulk, 3 interrupt
    pub fn transfer_type(&self) -> u8 {
        self.attributes & 3
    }
    /// Endpoint index: number × 2 + direction (the xHCI device context
    /// index; EHCI drivers use it as a table index too).
    pub fn dci(&self) -> u8 {
        self.number() * 2 + self.is_in() as u8
    }
}

#[derive(Debug, Clone)]
pub struct Interface {
    pub number: u8,
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
    /// Class-specific descriptors that follow the interface descriptor (raw).
    pub extra: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

pub fn parse_config(b: &[u8]) -> Option<Config> {
    if b.len() < 9 || b[1] != 2 {
        return None;
    }
    let mut cfg = Config {
        value: b[5],
        interfaces: Vec::new(),
    };
    let mut i = 0;
    while i + 2 <= b.len() {
        let len = b[i] as usize;
        if len < 2 || i + len > b.len() {
            break;
        }
        let d = &b[i..i + len];
        match d[1] {
            4 if len >= 9 => cfg.interfaces.push(Interface {
                number: d[2],
                alt: d[3],
                class: d[5],
                subclass: d[6],
                protocol: d[7],
                endpoints: Vec::new(),
                extra: Vec::new(),
            }),
            5 if len >= 7 => {
                if let Some(iface) = cfg.interfaces.last_mut() {
                    iface.endpoints.push(Endpoint {
                        address: d[2],
                        attributes: d[3],
                        max_packet: u16::from_le_bytes([d[4], d[5]]),
                        interval: d[6],
                    });
                }
            }
            2 => {}
            _ => {
                if let Some(iface) = cfg.interfaces.last_mut() {
                    iface.extra.push(d.to_vec());
                }
            }
        }
        i += len;
    }
    Some(cfg)
}

pub fn class_name(class: u8) -> &'static str {
    match class {
        0x00 => "per-interface",
        0x01 => "audio",
        0x02 => "CDC control",
        0x03 => "HID",
        0x08 => "mass storage",
        0x09 => "hub",
        0x0a => "CDC data",
        0x0e => "video",
        0xe0 => "wireless",
        0xef => "misc",
        0xff => "vendor-specific",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device's configuration: a HID interface with one interrupt IN
    /// endpoint and its HID class descriptor, then a vendor interface with
    /// two bulk endpoints.
    const CONFIG: &[u8] = &[
        9, 2, 48, 0, 2, 1, 0, 0x80, 50, // configuration 1, 2 interfaces
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0: HID boot keyboard
        9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID descriptor (extra)
        7, 5, 0x81, 3, 8, 0, 10, // EP 1 IN interrupt, 8 bytes
        9, 4, 1, 0, 2, 0xff, 0, 0, 0, // interface 1: vendor
        7, 5, 0x02, 2, 0, 2, 0, // EP 2 OUT bulk, 512
        7, 5, 0x83, 2, 0, 2, 0, // EP 3 IN bulk, 512
    ];

    #[test]
    fn a_configuration() {
        let c = parse_config(CONFIG).unwrap();
        assert_eq!(c.value, 1);
        assert_eq!(c.interfaces.len(), 2);
        let hid = &c.interfaces[0];
        assert_eq!((hid.class, hid.subclass, hid.protocol), (3, 1, 1));
        assert_eq!(hid.extra, [CONFIG[18..27].to_vec()]);
        assert_eq!(hid.endpoints.len(), 1);
        let ep = hid.endpoints[0];
        assert!(ep.is_in() && ep.transfer_type() == 3 && ep.max_packet == 8);
        let bulk = &c.interfaces[1].endpoints;
        assert_eq!((bulk[0].dci(), bulk[1].dci()), (4, 7)); // 2 OUT, 3 IN
        assert_eq!(bulk[1].max_packet, 512);
    }

    #[test]
    fn what_a_device_may_send_wrong() {
        // Cut anywhere: what is whole is kept, nothing panics.
        for n in 0..CONFIG.len() {
            let _ = parse_config(&CONFIG[..n]);
        }
        assert!(parse_config(&CONFIG[..8]).is_none());
        // A descriptor that claims to run past the end, or a zero length,
        // ends the walk.
        let mut long = CONFIG.to_vec();
        long[27] = 200;
        assert_eq!(parse_config(&long).unwrap().interfaces[0].endpoints.len(), 0);
        let mut zero = CONFIG.to_vec();
        zero[9] = 0;
        assert_eq!(parse_config(&zero).unwrap().interfaces.len(), 0);
        // Too short to be what they say: ignored, not read past.
        let short = [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 4, 4, 0, 0, 5, 5, 0x81, 3, 8];
        assert_eq!(parse_config(&short).unwrap().interfaces.len(), 0);
        // An endpoint before any interface.
        let orphan = [9, 2, 16, 0, 0, 1, 0, 0x80, 50, 7, 5, 0x81, 3, 8, 0, 10];
        assert!(parse_config(&orphan).unwrap().interfaces.is_empty());
    }

    #[test]
    fn a_device_descriptor() {
        let d = [18, 1, 0, 2, 0, 0, 0, 64, 0x8f, 0x14, 0x70, 0x53, 0, 1, 1, 2, 3, 1];
        let dd = DeviceDesc::parse(&d).unwrap();
        assert_eq!((dd.vendor, dd.product, dd.max_packet0, dd.num_configs), (0x148f, 0x5370, 64, 1));
        assert!(DeviceDesc::parse(&d[..17]).is_none());
        let mut wrong = d;
        wrong[1] = 2; // a configuration, not a device descriptor
        assert!(DeviceDesc::parse(&wrong).is_none());
    }
}
