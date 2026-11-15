//! Client of the config server (on touchos itself: it makes the calls
//! through touchos-rt).

use alloc::string::String;
use touchos_abi::Error;
use touchos_rt::{ipc, sys};

use crate::proto::{self, req};
use crate::{Config, Effects};

pub struct Client {
    ep: u32,
}

/// The result of a change.
#[derive(Debug)]
pub enum Update {
    Ok {
        version: u64,
        effects: Effects,
        /// 1 = saved, 0 = not saved (read-only), 2 = saving failed.
        saved: u8,
    },
    /// Someone else changed the settings first; this is the current version.
    Conflict(u64),
    /// "field: message" lines.
    Invalid(String),
    Denied,
}

impl Client {
    /// The config endpoint we were given as `name` (usually "config").
    pub fn new(name: &str) -> Option<Client> {
        touchos_rt::cap(name).map(|ep| Client { ep })
    }

    /// (version, file text).
    pub fn text(&self) -> Result<(u64, String), Error> {
        let m = ipc::call(self.ep, req::GET, &[], &[])?;
        if m.label != proto::REPLY_OK || m.caps().is_empty() || m.data().len() < 8 {
            return Err(Error::InvalidArgument);
        }
        let version = u64::from_le_bytes(m.data()[..8].try_into().unwrap());
        let text = touchos_rt::caps::read_shared(m.caps()[0]).ok_or(Error::InvalidArgument)?;
        Ok((version, String::from_utf8_lossy(&text).into_owned()))
    }

    /// (version, parsed settings). Problems were already logged by the server.
    pub fn get(&self) -> Result<(u64, Config), Error> {
        let (v, text) = self.text()?;
        let cfg = Config::parse(&text).map(|(c, _)| c).unwrap_or_default();
        Ok((v, cfg))
    }

    /// Signal our notification with `bits` whenever the settings change.
    pub fn watch(&self, bits: usize) -> Result<(), Error> {
        let n = sys::notif_create()?;
        let m = ipc::call(self.ep, req::WATCH, &(bits as u64).to_le_bytes(), &[n]); // u64 on the wire
        let _ = sys::cap_drop(n);
        match m?.label {
            proto::REPLY_OK => Ok(()),
            _ => Err(Error::InvalidArgument),
        }
    }

    /// (version, source, problems as "field: message" lines).
    pub fn status(&self) -> Result<(u64, u32, String), Error> {
        let m = ipc::call(self.ep, req::STATUS, &[], &[])?;
        let d = m.data();
        if m.label != proto::REPLY_OK || d.len() < 12 {
            return Err(Error::InvalidArgument);
        }
        Ok((
            u64::from_le_bytes(d[..8].try_into().unwrap()),
            u32::from_le_bytes(d[8..12].try_into().unwrap()),
            String::from_utf8_lossy(&d[12..]).into_owned(),
        ))
    }

    /// Apply a patch (see `crate::patch`) on top of `base_version`.
    pub fn update(&self, base_version: u64, patch: &str) -> Result<Update, Error> {
        let mut data = alloc::vec::Vec::with_capacity(8 + patch.len());
        data.extend_from_slice(&base_version.to_le_bytes());
        data.extend_from_slice(patch.as_bytes());
        if data.len() > touchos_abi::UTCB_DATA {
            return Err(Error::InvalidArgument);
        }
        let m = ipc::call(self.ep, req::UPDATE, &data, &[])?;
        let d = m.data();
        let u64_at = |o: usize| {
            d.get(o..o + 8)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        };
        Ok(match m.label {
            proto::REPLY_OK => Update::Ok {
                version: u64_at(0).unwrap_or(0),
                effects: Effects::from_bits(
                    d.get(8..12)
                        .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap())),
                ),
                saved: d.get(12).copied().unwrap_or(0),
            },
            proto::REPLY_CONFLICT => Update::Conflict(u64_at(0).unwrap_or(0)),
            proto::REPLY_INVALID => Update::Invalid(String::from_utf8_lossy(d).into_owned()),
            proto::REPLY_DENIED => Update::Denied,
            _ => return Err(Error::InvalidArgument),
        })
    }
}

/// This process's settings: fetched from "config" once, re-read when the
/// config server says they changed. One reader of changes per process
/// (the task that reacts to them); `get` may be called from anywhere.
///
/// When the config server can't be reached at the start, the defaults are
/// used. When a re-read fails, the settings stay as they were.
pub mod settings {
    use alloc::sync::Arc;
    use touchos_rt::{Local, exec};

    use super::Client;
    use crate::Config;

    struct Watched {
        client: Option<Client>,
        bit: usize,
        current: Arc<Config>,
    }

    static W: Local<Option<Watched>> = Local::new(None);

    fn with<R>(f: impl FnOnce(&mut Watched) -> R) -> R {
        W.with(|w| {
            let w = w.get_or_insert_with(|| {
                let client = Client::new("config");
                let bit = exec::alloc_bit();
                if client.as_ref().is_some_and(|c| c.watch(bit).is_err()) {
                    log::warn!("can't watch the settings");
                }
                let current = match client.as_ref().map(|c| c.get()) {
                    Some(Ok((_, cfg))) => cfg,
                    _ => {
                        log::warn!("config server not reachable; using the defaults");
                        Config::default()
                    }
                };
                Watched { client, bit, current: Arc::new(current) }
            });
            f(w)
        })
    }

    /// The current settings.
    pub fn get() -> Arc<Config> {
        with(|w| w.current.clone())
    }

    /// After a change: (the settings before, the settings now), once.
    pub fn poll() -> Option<(Arc<Config>, Arc<Config>)> {
        with(|w| {
            if exec::take_bits(w.bit) == 0 {
                return None;
            }
            reread(w)
        })
    }

    /// Wait for the next change: (the settings before, the settings now).
    pub async fn changed() -> (Arc<Config>, Arc<Config>) {
        let bit = with(|w| w.bit);
        loop {
            exec::wait_bits(bit).await;
            if let Some(c) = with(reread) {
                return c;
            }
        }
    }

    fn reread(w: &mut Watched) -> Option<(Arc<Config>, Arc<Config>)> {
        match w.client.as_ref()?.get() {
            Ok((_, new)) => {
                let old = core::mem::replace(&mut w.current, Arc::new(new));
                Some((old, w.current.clone()))
            }
            Err(e) => {
                log::warn!("can't read the changed settings ({e:?}); keeping the old ones");
                None
            }
        }
    }
}
