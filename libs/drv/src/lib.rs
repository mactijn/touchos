//! What device drivers need, in user space:
//!
//! - [`mem`]: DMA buffers (physically contiguous memory objects whose
//!   physical address we may learn);
//! - [`mmio`]: [`Regs`](mmio::Regs), a mapped register window with
//!   volatile accessors (from an MMIO capability);
//! - [`link`]: the Ethernet link to the net server (rings + a state board);
//! - [`input`]: input events to the dashboard (a ring).
//!
//! There is no IOMMU: a driver that programs DMA can reach all memory.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod mmio;

#[cfg(target_os = "none")]
pub mod mem {
    use touchos_abi::{mem as memflags, prot};
    use touchos_rt::sys;

    pub const PAGE: u64 = 4096;

    /// A physically contiguous, zeroed buffer for device DMA (never freed:
    /// a device may still write to it).
    pub struct Dma {
        pub phys: u64,
        pub virt: *mut u8,
        pub len: usize,
    }
    unsafe impl Send for Dma {}

    impl Dma {
        pub fn read32(&self, off: usize) -> u32 {
            debug_assert!(off + 4 <= self.len);
            unsafe { (self.virt.add(off) as *const u32).read_volatile() }
        }
        pub fn write32(&self, off: usize, v: u32) {
            debug_assert!(off + 4 <= self.len);
            unsafe { (self.virt.add(off) as *mut u32).write_volatile(v) }
        }
        pub fn write64(&self, off: usize, v: u64) {
            self.write32(off, v as u32);
            self.write32(off + 4, (v >> 32) as u32);
        }
        pub fn bytes(&self) -> &[u8] {
            unsafe { core::slice::from_raw_parts(self.virt, self.len) }
        }
        pub fn bytes_mut(&mut self) -> &mut [u8] {
            unsafe { core::slice::from_raw_parts_mut(self.virt, self.len) }
        }
        pub fn zero(&mut self) {
            self.bytes_mut().fill(0);
        }
    }

    pub fn alloc_dma(len: usize) -> Dma {
        let size = (len as u64).max(1);
        let mem = sys::mem_create(size, memflags::CONTIGUOUS).expect("out of DMA memory");
        let virt = sys::mem_map(mem, 0, size, prot::WRITE).expect("map DMA memory") as *mut u8;
        let phys = sys::mem_phys(mem, 0).expect("DMA memory without PHYS right");
        // The mapping keeps the memory alive; drivers allocate hundreds of
        // buffers, more than a capability table holds.
        let _ = sys::cap_drop(mem);
        Dma { phys, virt, len }
    }

    /// `pages` contiguous zeroed pages the device may use; returns their
    /// physical address (we never touch them ourselves).
    pub fn alloc_frames(pages: u64) -> u64 {
        let mem = sys::mem_create(pages * PAGE, memflags::CONTIGUOUS).expect("out of DMA memory");
        let phys = sys::mem_phys(mem, 0).expect("DMA memory without PHYS right");
        // Contiguous memory is never freed, so the capability can go.
        let _ = sys::cap_drop(mem);
        phys
    }
}

/// The Ethernet link to the net server: frames in two rings (we produce
/// "link_rx", consume "link_tx") and the MAC address on a board.
#[cfg(target_os = "none")]
pub mod link {
    use alloc::vec::Vec;
    use touchos_proto::net::{FRAME_SLOT, FRAME_SLOTS, Link};
    use touchos_rt::shm::{self, Board, Pushed, Ring, Shared};
    use touchos_rt::{cap, sys};

    struct L {
        rx: Shared<Ring>,
        tx: Shared<Ring>,
        state: Shared<Board>,
    }

    static LINK: spin::Once<Option<L>> = spin::Once::new();

    fn l() -> Option<&'static L> {
        LINK.call_once(|| {
            let l = L {
                rx: shm::ring("link_rx")?,
                tx: shm::ring("link_tx")?,
                state: shm::board("link_state", true)?,
            };
            l.rx.init(FRAME_SLOTS, FRAME_SLOT);
            Some(l)
        })
        .as_ref()
    }

    /// Wake us (notification `bits`) when the net server queues a frame.
    pub fn wait_tx(bits: usize) {
        if let Some(t) = cap("link_tx") {
            let _ = sys::mem_wait(t, bits);
        }
    }

    fn publish(mac: Option<[u8; 6]>, ap: Option<touchos_proto::net::HostName>) {
        if let Some(l) = l() {
            l.state.write_as(&Link { mac, ap });
            let _ = sys::mem_notify(l.state.mem());
            let _ = sys::mem_notify(l.rx.mem());
        }
    }

    pub fn up(mac: [u8; 6]) {
        publish(Some(mac), None);
    }

    /// Up as an access point of our own: the setup network `ssid` (net
    /// then takes the setup address and serves DHCP and DNS on it).
    pub fn up_ap(mac: [u8; 6], ssid: touchos_proto::net::HostName) {
        publish(Some(mac), Some(ssid));
    }

    pub fn down() {
        publish(None, None);
    }

    /// A frame arrived from the network.
    pub fn deliver(frame: Vec<u8>) {
        if let Some(l) = l()
            && l.rx.push(&frame) == Pushed::Wake
        {
            let _ = sys::mem_notify(l.rx.mem());
        }
    }

    /// The next frame to send, if any.
    pub fn pop_tx() -> Option<Vec<u8>> {
        let l = l()?;
        let mut buf = alloc::vec![0u8; FRAME_SLOT as usize];
        let n = l.tx.pop(&mut buf)?;
        buf.truncate(n);
        Some(buf)
    }
}

/// Input events to the dashboard: we produce the ring "input" (USB HID in
/// the USB server, touch and buttons in the platform server).
#[cfg(target_os = "none")]
pub mod input {
    pub use touchos_proto::input::Event;
    use touchos_proto::input::{RING_SLOT, RING_SLOTS};
    use touchos_rt::shm::{self, Pushed, Ring, Shared};
    use touchos_rt::sys;

    static OUT: spin::Once<Option<Shared<Ring>>> = spin::Once::new();

    fn out() -> Option<&'static Shared<Ring>> {
        OUT.call_once(|| {
            let ring = shm::ring("input")?;
            ring.init(RING_SLOTS, RING_SLOT);
            Some(ring)
        })
        .as_ref()
    }

    /// Queue an event (dropped if the dashboard isn't keeping up), and
    /// wake the dashboard if it waits for one.
    pub fn push(e: Event) {
        if let Some(ring) = out()
            && ring.push(&touchos_proto::encode(&e)) == Pushed::Wake
        {
            let _ = sys::mem_notify(ring.mem());
        }
    }
}
