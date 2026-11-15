//! The dashboard, in user space.
//!
//! It owns the framebuffer (mapped write-combining) and the backlight
//! registers. Home Assistant state, network status and input arrive through
//! shared memory; service calls go out the same way. Settings come from the
//! config server and are applied live when they change.

#![no_std]
#![no_main]

extern crate alloc;

mod backlight;
mod dash;
mod gfx;
mod sources;

pub use sources::{config, events, ha, input, net, overlay, time};
use sources::shm;

use log::{error, info};
use touchos_rt::{cap, exec, sys};

touchos_rt::entry!(main);

/// The panel's linear framebuffer: 32 bpp, or 16 bpp RGB565 (the Toon's
/// LCD controller).
pub struct Framebuffer {
    pub base: *mut u8,
    pub width: usize,
    pub height: usize,
    /// In bytes.
    pub pitch: usize,
    pub bpp: u8,
    pub r_shift: u8,
    pub g_shift: u8,
    pub b_shift: u8,
    /// A hash of what each panel row shows (`gfx::row_hash` of its pixels
    /// before conversion; 0 at first, which no row hashes to in practice):
    /// `present` skips rows that didn't change.
    pub rows: alloc::vec::Vec<u32>,
}

fn framebuffer() -> Option<Framebuffer> {
    let info = touchos_rt::time::kinfo().fb;
    let c = cap("framebuffer")?;
    if !matches!(info.bpp, 16 | 32) || info.width == 0 {
        error!("unsupported framebuffer ({} bpp)", info.bpp);
        return None;
    }
    let len = info.pitch as u64 * info.height as u64;
    let va = sys::mmio_map(c, 0, len).ok()?;
    Some(Framebuffer {
        base: va as *mut u8,
        width: info.width as usize,
        height: info.height as usize,
        pitch: info.pitch as usize,
        bpp: info.bpp as u8,
        r_shift: info.r_shift,
        g_shift: info.g_shift,
        b_shift: info.b_shift,
        rows: alloc::vec![0; info.height as usize],
    })
}

/// Requests from the web server.
fn handle(msg: touchos_rt::ipc::Msg) -> touchos_abi::Tag {
    use touchos_proto::ui;
    let d = msg.data();
    match msg.label {
        ui::SHOW_PIN if d.len() > 4 => {
            let (pin, secs) = d.split_at(d.len() - 4);
            let secs = u32::from_le_bytes(secs.try_into().unwrap()) as u64;
            overlay::show(
                alloc::string::String::from_utf8_lossy(pin).into_owned(),
                secs.min(600),
            );
        }
        ui::HIDE_PIN => overlay::hide(),
        _ => return touchos_rt::ipc::prepare(ui::BAD_REQUEST, &[], &[]),
    }
    touchos_rt::ipc::prepare(ui::OK, &[], &[])
}

fn main() {
    if !config::get().display.ui {
        info!("display.ui = \"off\": leaving the screen to the kernel log");
        return;
    }
    if shm::get().is_none() {
        error!("no shared memory with the HA client; can't run");
        return;
    }
    let Some(fb) = framebuffer() else {
        error!("no framebuffer");
        return;
    };
    exec::spawn(dash::task(fb));
    match cap("ep") {
        Some(ep) => exec::serve(ep, |msg, _| handle(msg)),
        None => exec::run(),
    }
}
