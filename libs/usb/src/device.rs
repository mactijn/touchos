//! A USB device on some host controller: enumeration after the address,
//! descriptors, strings, configuration, and control transfers.

use alloc::string::String;

use crate::desc::{self, Config, DeviceDesc, Endpoint};
use crate::{HostController, Result, UsbError};
use touchos_drv::mem::{self, Dma};
use touchos_rt::{exec, time};

/// Control transfers carry at most this much data (one buffer).
pub const CONTROL_MAX: usize = 4096;

/// Wait for a device on any root port and enumerate it, for a controller
/// that carries one device (the Toon's Wi-Fi module); no hot-plug after
/// that. servers/usb, which drives whatever comes and goes, runs its own
/// loop over the port changes instead.
pub async fn wait_for_device<H: HostController>(hc: &mut H) -> Device<H> {
    // Give port power and the PHY a moment.
    exec::sleep_ms(200).await;
    let t0 = time::now_ms();
    let mut told = false;
    loop {
        hc.poll();
        for port in 1..=hc.ports() {
            hc.ack_port(port);
            if !hc.port_status(port).connected {
                continue;
            }
            if let Some(dev) = Device::attach(hc, port).await {
                return dev;
            }
            exec::sleep_ms(1000).await; // it failed (logged); not too often
        }
        if !told && time::now_ms() - t0 > 5000 {
            ::log::warn!("no device on the USB port yet; still waiting");
            told = true;
        }
        exec::sleep_ms(100).await;
    }
}

pub struct Device<H: HostController> {
    pub port: u8,
    pub speed: u8,
    pub desc: DeviceDesc,
    pub config: Option<Config>,
    pub product: String,
    /// The controller's state for this device.
    pub hc: H::Dev,
    /// Data stage of control transfers.
    buf: Dma,
}

impl<H: HostController> Device<H> {
    /// Bring up the device on a connected port: reset the port, address
    /// the device and read its descriptor, in up to three attempts (a
    /// device that just powered up can miss the first). None if it never
    /// answered or went away meanwhile.
    pub async fn attach(hc: &mut H, port: u8) -> Option<Device<H>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let retry = attempt > 1;
            if retry {
                exec::sleep_ms(100).await;
                if !hc.port_status(port).connected {
                    return None; // gone
                }
            }
            let speed = match hc.reset_port(port, retry).await {
                Ok(s) => s,
                Err(e) => {
                    ::log::warn!("port {port}: reset failed: {e}");
                    if attempt < 3 { continue } else { return None }
                }
            };
            match Device::new(hc, port, speed).await {
                Ok(d) => return Some(d),
                Err(_) if attempt < 3 => ::log::warn!("port {port}: retrying ({attempt}/3)"),
                Err(_) => return None,
            }
        }
    }

    /// Address the device on a freshly reset port, then read its device
    /// descriptor and product name. On failure the device is disabled.
    pub async fn new(hc: &mut H, port: u8, speed: u8) -> Result<Device<H>> {
        let mut buf = mem::alloc_dma(CONTROL_MAX);
        let hcd = hc.address(port, speed, &mut buf).await?;
        let mut dev = Device {
            port,
            speed,
            desc: DeviceDesc::default(),
            config: None,
            product: String::new(),
            hc: hcd,
            buf,
        };
        match dev.setup(hc).await {
            Ok(()) => Ok(dev),
            Err((step, e)) => {
                ::log::warn!("port {port}: {step} failed: {e}");
                dev.disable(hc).await;
                Err(e)
            }
        }
    }

    async fn setup(&mut self, hc: &mut H) -> core::result::Result<(), (&'static str, UsbError)> {
        let n = self
            .control_in(hc, 0x80, 6, 0x0100, 0, 18)
            .await
            .map_err(|e| ("device descriptor", e))?;
        self.desc = DeviceDesc::parse(&self.buf.bytes()[..n])
            .ok_or(("device descriptor", UsbError::Protocol("malformed")))?;
        if self.desc.i_product != 0 {
            self.product = self.string(hc, self.desc.i_product).await.unwrap_or_default();
        }
        Ok(())
    }

    async fn control(&mut self, hc: &mut H, rt: u8, req: u8, value: u16, index: u16, len: u16) -> Result<usize> {
        let mut setup = [rt, req, 0, 0, 0, 0, 0, 0];
        setup[2..4].copy_from_slice(&value.to_le_bytes());
        setup[4..6].copy_from_slice(&index.to_le_bytes());
        setup[6..8].copy_from_slice(&len.to_le_bytes());
        hc.control(&mut self.hc, setup, &mut self.buf).await
    }

    /// A control transfer that reads up to `len` bytes (into [`data`]).
    /// Returns how many came.
    ///
    /// [`data`]: Device::data
    pub async fn control_in(&mut self, hc: &mut H, rt: u8, req: u8, value: u16, index: u16, len: u16) -> Result<usize> {
        let len = len.min(CONTROL_MAX as u16);
        self.buf.bytes_mut()[..len as usize].fill(0);
        self.control(hc, rt | 0x80, req, value, index, len).await
    }

    /// A control transfer without data.
    pub async fn control_out(&mut self, hc: &mut H, rt: u8, req: u8, value: u16, index: u16) -> Result<()> {
        self.control(hc, rt & 0x7f, req, value, index, 0).await.map(|_| ())
    }

    /// A control transfer that sends `data` (at most [`CONTROL_MAX`] bytes).
    pub async fn control_out_data(&mut self, hc: &mut H, rt: u8, req: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        if data.len() > CONTROL_MAX {
            return Err(UsbError::Protocol("control data too long"));
        }
        self.buf.bytes_mut()[..data.len()].copy_from_slice(data);
        self.control(hc, rt & 0x7f, req, value, index, data.len() as u16).await.map(|_| ())
    }

    /// The data of the last [`control_in`](Device::control_in).
    pub fn data(&self) -> &[u8] {
        self.buf.bytes()
    }

    pub async fn string(&mut self, hc: &mut H, index: u8) -> Result<String> {
        self.control_in(hc, 0x80, 6, 0x0300 | index as u16, 0x0409, 255).await?;
        let b = self.buf.bytes();
        let len = (b[0] as usize).min(255);
        let units = (2..len).step_by(2).map(|i| u16::from_le_bytes([b[i], b[i + 1]]));
        Ok(char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect())
    }

    /// Read configuration descriptor number `index` (0-based).
    pub async fn read_config(&mut self, hc: &mut H, index: u8) -> Result<Config> {
        self.control_in(hc, 0x80, 6, 0x0200 | index as u16, 0, 9).await?;
        let total = u16::from_le_bytes([self.buf.bytes()[2], self.buf.bytes()[3]]).min(CONTROL_MAX as u16);
        self.control_in(hc, 0x80, 6, 0x0200 | index as u16, 0, total).await?;
        desc::parse_config(&self.buf.bytes()[..total as usize]).ok_or(UsbError::Protocol("bad config descriptor"))
    }

    /// Select a configuration (SET_CONFIGURATION).
    pub async fn set_configuration(&mut self, hc: &mut H, cfg: Config) -> Result<()> {
        self.control_out(hc, 0x00, 9, cfg.value as u16, 0).await?;
        self.config = Some(cfg);
        Ok(())
    }

    /// Make endpoints of the current configuration usable.
    pub async fn configure_endpoints(&mut self, hc: &mut H, eps: &[Endpoint]) -> Result<()> {
        hc.configure_endpoints(&mut self.hc, eps).await
    }

    /// Queue a bulk/interrupt transfer; returns the token to [`check`](Device::check).
    pub fn queue(&mut self, hc: &mut H, ep: u8, buf_phys: u64, len: u32) -> u64 {
        hc.queue(&mut self.hc, ep, buf_phys, len)
    }

    /// Has the transfer completed? On success: the bytes *not* transferred.
    pub fn check(&mut self, hc: &mut H, ep: u8, token: u64) -> Option<Result<u32>> {
        hc.check(&mut self.hc, ep, token)
    }

    pub async fn disable(self, hc: &mut H) {
        hc.disable(self.hc).await;
    }
}
