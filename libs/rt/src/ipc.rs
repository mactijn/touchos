//! Message passing on top of the UTCB.

use touchos_abi::{Error, Tag, UTCB_CAPS, UTCB_DATA, Utcb, layout::UTCB_VA};

use crate::sys;

/// Our IPC buffer.
#[allow(clippy::mut_from_ref)]
pub fn utcb() -> &'static mut Utcb {
    unsafe { &mut *(UTCB_VA as *mut Utcb) }
}

/// Put a message in the UTCB; returns its tag.
pub fn prepare(label: u32, data: &[u8], caps: &[u32]) -> Tag {
    let u = utcb();
    let n = data.len().min(UTCB_DATA);
    u.data[..n].copy_from_slice(&data[..n]);
    let c = caps.len().min(UTCB_CAPS);
    u.caps[..c].copy_from_slice(&caps[..c]);
    Tag::new(label, n, c)
}

/// A received or reply message: label, payload and capability slots.
pub struct Msg {
    pub label: u32,
    pub len: usize,
    pub caps: [u32; UTCB_CAPS],
    pub ncaps: usize,
}

impl Msg {
    pub(crate) fn from_tag(tag: Tag) -> Msg {
        let u = utcb();
        Msg {
            label: tag.label(),
            len: tag.len().min(UTCB_DATA),
            caps: u.caps,
            ncaps: tag.ncaps().min(UTCB_CAPS),
        }
    }
    /// The payload (valid until the next IPC).
    pub fn data(&self) -> &'static [u8] {
        &utcb().data[..self.len]
    }
    pub fn caps(&self) -> &[u32] {
        &self.caps[..self.ncaps]
    }
}

/// A reply without capabilities (for a server's handler to return).
pub fn reply(label: u32, data: &[u8]) -> Tag {
    prepare(label, data, &[])
}

/// Call `ep` with a message and wait for the reply.
pub fn call(ep: u32, label: u32, data: &[u8], caps: &[u32]) -> Result<Msg, Error> {
    let tag = prepare(label, data, caps);
    sys::call(ep, tag).map(Msg::from_tag)
}
