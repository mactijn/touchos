//! USB CDC-ECM (Ethernet Control Model) class driver: standard USB Ethernet,
//! as spoken by QEMU's usb-net, RTL8153-based adapters (configuration 2),
//! and many others. One USB bulk transfer = one Ethernet frame.

use alloc::vec::Vec;

use crate::link;
use touchos_usb::desc::{Config, Endpoint};
use touchos_usb::{Device, HostController, Result, UsbError};
use crate::mem::{self, Dma};

const RX_BUFS: usize = 8;
const TX_BUFS: usize = 8;
const BUF_SIZE: usize = 2048;

struct Xfer {
    buf: Dma,
    pending: Option<u64>,
}

pub struct Ecm {
    pub port: u8,
    /// The bulk endpoints' addresses.
    in_ep: u8,
    out_ep: u8,
    out_mps: usize,
    rx: Vec<Xfer>,
    tx: Vec<Xfer>,
    pub failed: bool,
}

/// Does this configuration contain an ECM function?
pub fn is_ecm(cfg: &Config) -> bool {
    cfg.interfaces
        .iter()
        .any(|i| i.class == 0x02 && i.subclass == 0x06)
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    if s.len() != 12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for i in 0..6 {
        mac[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(mac)
}

impl Ecm {
    pub async fn attach<H: HostController>(hc: &mut H, dev: &mut Device<H>) -> Result<Ecm> {
        let cfg = dev
            .config
            .clone()
            .ok_or(UsbError::Protocol("not configured"))?;
        let ctrl = cfg
            .interfaces
            .iter()
            .find(|i| i.class == 0x02 && i.subclass == 0x06)
            .ok_or(UsbError::Protocol("no ECM control interface"))?;
        // Class-specific functional descriptors (CS_INTERFACE = 0x24).
        let func = |subtype: u8| {
            ctrl.extra
                .iter()
                .find(|d| d.len() >= 4 && d[1] == 0x24 && d[2] == subtype)
        };
        let data_if = func(0x06)
            .and_then(|d| d.get(4).copied())
            .unwrap_or(ctrl.number + 1);
        let mac_index = func(0x0f)
            .map(|d| d[3])
            .ok_or(UsbError::Protocol("no Ethernet functional descriptor"))?;
        let mac_str = dev.string(hc, mac_index).await?;
        let mac = parse_mac(&mac_str).ok_or(UsbError::Protocol("bad MAC address string"))?;

        // The data interface's alternate setting with the two bulk endpoints.
        let data = cfg
            .interfaces
            .iter()
            .find(|i| {
                i.number == data_if
                    && i.endpoints
                        .iter()
                        .filter(|e| e.transfer_type() == 2)
                        .count()
                        == 2
            })
            .ok_or(UsbError::Protocol(
                "no ECM data interface with bulk endpoints",
            ))?;
        let bulk_in: Endpoint = *data
            .endpoints
            .iter()
            .find(|e| e.is_in() && e.transfer_type() == 2)
            .unwrap();
        let bulk_out: Endpoint = *data
            .endpoints
            .iter()
            .find(|e| !e.is_in() && e.transfer_type() == 2)
            .unwrap();

        dev.configure_endpoints(hc, &[bulk_in, bulk_out]).await?;
        // SET_INTERFACE: activate the alternate setting that has the endpoints.
        dev.control_out(hc, 0x01, 11, data.alt as u16, data_if as u16)
            .await?;
        // SET_ETHERNET_PACKET_FILTER: directed + broadcast + all-multicast.
        // Optional; some devices stall it.
        let _ = dev
            .control_out(hc, 0x21, 0x43, 0x000e, ctrl.number as u16)
            .await;

        let mut ecm = Ecm {
            port: dev.port,
            in_ep: bulk_in.address,
            out_ep: bulk_out.address,
            out_mps: (bulk_out.max_packet & 0x7ff) as usize,
            rx: (0..RX_BUFS)
                .map(|_| Xfer {
                    buf: mem::alloc_dma(BUF_SIZE),
                    pending: None,
                })
                .collect(),
            tx: (0..TX_BUFS)
                .map(|_| Xfer {
                    buf: mem::alloc_dma(BUF_SIZE),
                    pending: None,
                })
                .collect(),
            failed: false,
        };
        for x in ecm.rx.iter_mut() {
            x.pending = Some(dev.queue(hc, ecm.in_ep, x.buf.phys, BUF_SIZE as u32));
        }
        ::log::info!(
            "port {} ethernet (CDC-ECM) ready, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            dev.port,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
        link::up(mac);
        Ok(ecm)
    }

    pub fn detach(&self) {
        link::down();
    }

    /// Move frames between the USB endpoints and the link queues.
    pub fn poll<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        if self.failed {
            return;
        }
        // Received frames.
        for i in 0..self.rx.len() {
            let Some(token) = self.rx[i].pending else {
                continue;
            };
            match dev.check(hc, self.in_ep, token) {
                None => {}
                Some(Ok(residual)) => {
                    let n = BUF_SIZE.saturating_sub(residual as usize);
                    if n >= 14 {
                        link::deliver(self.rx[i].buf.bytes()[..n].to_vec());
                    }
                    self.rx[i].pending = Some(dev.queue(hc, self.in_ep, self.rx[i].buf.phys, BUF_SIZE as u32));
                }
                Some(Err(e)) => {
                    ::log::warn!("port {} ethernet receive failed: {e}", self.port);
                    self.failed = true;
                    link::down();
                    return;
                }
            }
        }
        // Finished transmissions free their buffers.
        for x in self.tx.iter_mut() {
            if let Some(token) = x.pending
                && let Some(r) = dev.check(hc, self.out_ep, token)
            {
                if let Err(e) = r {
                    ::log::warn!("port {} ethernet send failed: {e}", self.port);
                }
                x.pending = None;
            }
        }
        // New frames to send.
        for x in self.tx.iter_mut().filter(|x| x.pending.is_none()) {
            let Some(frame) = link::pop_tx() else {
                break;
            };
            let mut len = frame.len().min(BUF_SIZE - 1);
            x.buf.bytes_mut()[..len].copy_from_slice(&frame[..len]);
            // A transfer that is an exact multiple of the packet size would need a
            // zero-length packet to end it; add one padding byte instead (as Linux does).
            if self.out_mps > 0 && len % self.out_mps == 0 {
                x.buf.bytes_mut()[len] = 0;
                len += 1;
            }
            x.pending = Some(dev.queue(hc, self.out_ep, x.buf.phys, len as u32));
        }
    }
}
