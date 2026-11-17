//! Where the dashboard's data comes from and goes to: the settings, Home
//! Assistant's state and the service calls (shared memory with ha), the
//! network status (net's board), input events (the USB and platform
//! servers' rings), the pairing PIN (asked for by the web server), and the
//! time.

use log::error;

/// Shared memory with the Home Assistant client, the network stack and
/// the input drivers.
pub mod shm {
    use super::*;
    use touchos_rt::shm::{self as rt_shm, Board, Ring, Shared};

    pub struct Shm {
        /// Input from the USB server (HID keyboards and mice).
        pub input: Shared<Ring>,
        /// Input from the platform server (touch, orientation), if any.
        pub touch: Option<Shared<Ring>>,
        /// Service calls to the HA client (we produce).
        pub calls: Shared<Ring>,
        pub ha: Shared<Board>,
        pub net: Shared<Board>,
    }

    static SHM: spin::Once<Option<Shm>> = spin::Once::new();

    fn logged<T>(name: &str, v: Option<T>) -> Option<T> {
        if v.is_none() {
            error!("can't map {name}");
        }
        v
    }

    pub fn get() -> Option<&'static Shm> {
        SHM.call_once(|| {
            let shm = Shm {
                input: logged("input", rt_shm::ring("input"))?,
                touch: rt_shm::ring("touch"),
                calls: logged("ha_calls", rt_shm::ring("ha_calls"))?,
                ha: logged("ha_state", rt_shm::board("ha_state", false))?,
                net: logged("net_state", rt_shm::board("net_state", false))?,
            };
            shm.calls.init(touchos_proto::ha::CALL_SLOTS, touchos_proto::ha::CALL_SLOT);
            Some(shm)
        })
        .as_ref()
    }
}

/// Settings from the config server (touchos_config::client::settings).
pub mod config {
    pub use touchos_config::client::settings::get;
    pub use touchos_config::{Config, Rotation};

    /// True (once) after the settings changed; `get` then returns the new ones.
    pub fn changed() -> bool {
        let changed = touchos_config::client::settings::poll().is_some();
        if changed {
            log::info!("settings changed; applying");
        }
        changed
    }
}

/// Home Assistant state (from the HA client's board) and service calls.
pub mod ha {
    use alloc::vec::Vec;
    pub use touchos_proto::ha::{Entity, Snapshot as HaState};
    use touchos_proto::{encode, ha::{CallData, ServiceCall}};

    pub static STATE: spin::Mutex<HaState> = spin::Mutex::new(HaState {
        entities: alloc::collections::BTreeMap::new(),
        connected: false,
        version: 0,
        areas: Vec::new(),
        entity_area: alloc::collections::BTreeMap::new(),
        hidden: alloc::collections::BTreeSet::new(),
        registry_version: 0,
    });
    static SEQ: spin::Mutex<(u64, Vec<u8>)> = spin::Mutex::new((0, Vec::new()));

    /// Pick up a new snapshot if the HA client published one.
    pub fn refresh() {
        let Some(shm) = super::shm::get() else { return };
        let mut g = SEQ.lock();
        let (seq, buf) = &mut *g;
        if let Some(snap) = shm.ha.read_as::<HaState>(seq, buf) {
            *STATE.lock() = snap;
        }
    }

    /// Queue a service call, e.g. ("light", "toggle", "light.kitchen", None).
    pub fn call_service(
        domain: &str,
        service: &str,
        entity_id: &str,
        data: CallData,
    ) {
        let Some(shm) = super::shm::get() else { return };
        let call = ServiceCall {
            domain: domain.into(),
            service: service.into(),
            entity_id: entity_id.into(),
            data,
        };
        // The HA client waits for calls: wake it.
        if shm.calls.push(&encode(&call)) == touchos_rt::shm::Pushed::Wake {
            let _ = touchos_rt::sys::mem_notify(shm.calls.mem());
        }
    }
}

/// What wakes the dashboard: one bit, signalled by the input drivers, the
/// HA client and the network stack (their shared memory, `MEM_WAIT`) and
/// by our own request handler (the PIN).
pub mod events {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use touchos_rt::{exec, sys};

    static BIT: AtomicUsize = AtomicUsize::new(0);

    /// The bit (registered on first use).
    pub fn bit() -> usize {
        match BIT.load(Ordering::Relaxed) {
            0 => {
                let b = exec::alloc_bit();
                if let Some(shm) = super::shm::get() {
                    let mems = [Some(shm.input.mem()), shm.touch.as_ref().map(|t| t.mem()), Some(shm.ha.mem()), Some(shm.net.mem())];
                    for m in mems.into_iter().flatten() {
                        let _ = sys::mem_wait(m, b);
                    }
                }
                BIT.store(b, Ordering::Relaxed);
                b
            }
            b => b,
        }
    }

    /// Wake the dashboard from this process.
    pub fn raise() {
        exec::raise(bit());
    }
}

/// Network status (from the network stack's board).
pub mod net {
    use touchos_proto::net::Status;

    pub struct Net {
        pub ipv4: Option<([u8; 4], u8)>,
    }

    static CUR: spin::Mutex<(u64, Status)> = spin::Mutex::new((
        0,
        Status {
            ipv4: None,
            generation: 0,
            hostname: touchos_proto::net::HostName::EMPTY,
        },
    ));

    pub fn with<R>(f: impl FnOnce(&Net) -> R) -> Option<R> {
        let shm = super::shm::get()?;
        let mut g = CUR.lock();
        let mut buf = alloc::vec::Vec::new();
        if let Some(st) = shm.net.read_as::<Status>(&mut g.0, &mut buf) {
            g.1 = st;
        }
        Some(f(&Net { ipv4: g.1.ipv4 }))
    }
}

/// Input events (touch, USB HID) and the screen orientation.
pub mod input {
    use core::sync::atomic::{AtomicU8, Ordering};
    pub use touchos_proto::input::Event;

    static ROTATION: AtomicU8 = AtomicU8::new(0);

    pub fn pop() -> Option<Event> {
        let shm = super::shm::get()?;
        let mut buf = [0u8; touchos_proto::input::RING_SLOT as usize];
        let rings = core::iter::once(&shm.input).chain(shm.touch.as_ref());
        for ring in rings {
            while let Some(n) = ring.pop(&mut buf) {
                match touchos_proto::decode::<Event>(&buf[..n]) {
                    Some(Event::Rotation(r)) => ROTATION.store(r, Ordering::Relaxed),
                    Some(e) => return Some(e),
                    None => {}
                }
            }
        }
        None
    }

    /// Quarter turns: the fixed setting, or what the accelerometer says.
    pub fn rotation() -> u8 {
        match super::config::get().display.rotation {
            super::config::Rotation::Fixed(r) => r,
            super::config::Rotation::Auto => ROTATION.load(Ordering::Relaxed),
        }
    }
}

/// The pairing PIN the web server asked us to show.
pub mod overlay {
    use alloc::string::String;

    static PIN: spin::Mutex<Option<(String, u64)>> = spin::Mutex::new(None);

    pub fn show(pin: String, secs: u64) {
        *PIN.lock() = Some((pin, super::time::now_ms() + secs * 1000));
        super::events::raise();
    }

    pub fn hide() {
        *PIN.lock() = None;
        super::events::raise();
    }

    /// (PIN, seconds left) while it should be on screen.
    pub fn current() -> Option<(String, u64)> {
        let mut g = PIN.lock();
        let now = super::time::now_ms();
        match g.as_ref() {
            Some((p, until)) if now < *until => Some((p.clone(), (until - now).div_ceil(1000))),
            Some(_) => {
                *g = None;
                None
            }
            None => None,
        }
    }
}

pub mod time {
    pub use touchos_civil::Tz;
    pub use touchos_rt::time::{now_ms, unix_now};
}

