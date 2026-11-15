//! USB HID class driver: boot-protocol keyboards and mice. Other HID
//! interfaces (e.g. the dock's touchpad in report mode) are logged raw.

use crate::input::{self, Event};
use touchos_usb::desc::Interface;
use touchos_usb::{Device, HostController, Result, UsbError};
use crate::mem::{self, Dma};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyboard,
    Mouse,
    Other,
}

pub struct Hid {
    pub port: u8,
    iface: u8,
    /// The interrupt IN endpoint's address.
    ep: u8,
    kind: Kind,
    buf: Dma,
    len: u32,
    pending: Option<u64>,
    last_keys: [u8; 6],
    raw_logged: u32,
    /// A read failed: the device is re-attached (see main's `Recovery`).
    pub failed: bool,
}

impl Hid {
    pub async fn attach<H: HostController>(hc: &mut H, dev: &mut Device<H>, iface: &Interface) -> Result<Hid> {
        let ep = *iface
            .endpoints
            .iter()
            .find(|e| e.is_in() && e.transfer_type() == 3)
            .ok_or(UsbError::Protocol("HID interface without interrupt IN endpoint"))?;
        let kind = match (iface.subclass, iface.protocol) {
            (1, 1) => Kind::Keyboard,
            (1, 2) => Kind::Mouse,
            _ => Kind::Other,
        };
        if kind != Kind::Other {
            // SET_PROTOCOL(boot): fixed, simple report formats.
            dev.control_out(hc, 0x21, 0x0b, 0, iface.number as u16)
                .await?;
        }
        // SET_IDLE(0): only report on change. Some devices stall this; that's fine.
        let _ = dev
            .control_out(hc, 0x21, 0x0a, 0, iface.number as u16)
            .await;
        dev.configure_endpoints(hc, &[ep]).await?;

        let len = (ep.max_packet & 0x7ff).clamp(8, 64) as u32;
        let mut hid = Hid {
            port: dev.port,
            iface: iface.number,
            ep: ep.address,
            kind,
            buf: mem::alloc_dma(64),
            len,
            pending: None,
            last_keys: [0; 6],
            raw_logged: 0,
            failed: false,
        };
        hid.pending = Some(dev.queue(hc, hid.ep, hid.buf.phys, hid.len));
        ::log::info!("port {} if{}: HID {:?} ready", dev.port, iface.number, kind);
        Ok(hid)
    }

    /// Check for a completed report; handle it and queue the next read.
    pub fn poll<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        let Some(token) = self.pending else { return };
        match dev.check(hc, self.ep, token) {
            None => {}
            Some(Ok(residual)) => {
                let n = self.len.saturating_sub(residual) as usize;
                let report: [u8; 64] = {
                    let mut r = [0u8; 64];
                    r[..n].copy_from_slice(&self.buf.bytes()[..n]);
                    r
                };
                self.handle(&report[..n]);
                self.pending = Some(dev.queue(hc, self.ep, self.buf.phys, self.len));
            }
            Some(Err(e)) => {
                ::log::warn!("port {} if{}: HID read failed: {e}", self.port, self.iface);
                self.pending = None;
                self.failed = true;
            }
        }
    }

    fn handle(&mut self, r: &[u8]) {
        match self.kind {
            Kind::Keyboard if r.len() >= 8 => {
                let mods = r[0];
                let keys = &r[2..8];
                for &k in keys {
                    if k > 3 && !self.last_keys.contains(&k) {
                        input::push(Event::Key {
                            usage: k,
                            down: true,
                            mods,
                        });
                    }
                }
                for &k in &self.last_keys {
                    if k > 3 && !keys.contains(&k) {
                        input::push(Event::Key {
                            usage: k,
                            down: false,
                            mods,
                        });
                    }
                }
                self.last_keys.copy_from_slice(keys);
            }
            Kind::Mouse if r.len() >= 3 => {
                input::push(Event::PointerRel {
                    dx: r[1] as i8 as i16,
                    dy: r[2] as i8 as i16,
                    buttons: r[0],
                });
            }
            _ => {
                if self.raw_logged < 20 {
                    self.raw_logged += 1;
                    ::log::debug!("port {} if{} report {:02x?}", self.port, self.iface, r);
                }
            }
        }
    }
}
