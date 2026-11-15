//! How a message crosses: a caller blocks until a receiver takes its
//! message and replies; the kernel copies the message from UTCB to UTCB
//! (and capabilities from table to table). A thread blocked in RECV also
//! wakes up for notifications.

use alloc::sync::Arc;
use alloc::vec::Vec;
use touchos_abi::{Error, LABEL_NOTIFIED, Tag, UTCB_CAPS, UTCB_DATA, Utcb};

use super::sched::schedule;
use super::{IpcResult, SCHED, State, Thread, with};
use crate::arch::{self, irq as interrupts};
use crate::cap::{Cap, Endpoint};
use crate::mem;
use crate::time;
use crate::uabi::layout::UTCB_VA;

/// Copy a message (payload and capabilities) from one UTCB to another.
/// Returns the tag as the receiver sees it.
fn transfer(from: &Thread, to: &Thread, tag: Tag) -> Result<u64, Error> {
    let (Some(fp), Some(tp)) = (&from.process, &to.process) else {
        return Err(Error::InvalidArgument);
    };
    let len = tag.len().min(UTCB_DATA);
    let ncaps = tag.ncaps().min(UTCB_CAPS);
    // One of the two is the current thread, whose UTCB is at UTCB_VA in the
    // active address space: its cached lines go back to memory first. Then
    // both UTCBs through the direct map (caches that alias need this).
    arch::paging::sync_user(UTCB_VA, core::mem::size_of::<Utcb>());
    // The other side's lines (by their own address: ARM's FCSE puts every
    // process's at its own) go back to memory too, and away, so it reads
    // what the kernel writes.
    arch::paging::sync_space(from.root, UTCB_VA, core::mem::size_of::<Utcb>());
    arch::paging::sync_space(to.root, UTCB_VA, core::mem::size_of::<Utcb>());
    arch::paging::sync_direct(from.utcb, core::mem::size_of::<Utcb>());
    arch::paging::sync_direct(to.utcb, core::mem::size_of::<Utcb>());
    let src = mem::phys_to_virt(from.utcb) as *const Utcb;
    let dst = mem::phys_to_virt(to.utcb) as *mut Utcb;
    let mut moved = [0u32; UTCB_CAPS];
    if ncaps > 0 {
        let caps: Vec<Cap> = {
            let table = fp.caps.lock();
            (0..ncaps)
                .map(|i| table.get(unsafe { (*src).caps[i] }).cloned())
                .collect::<Result<_, _>>()?
        };
        let mut table = tp.caps.lock();
        for (i, c) in caps.into_iter().enumerate() {
            moved[i] = table.insert(c)?;
        }
    }
    unsafe {
        core::ptr::copy_nonoverlapping((*src).data.as_ptr(), (*dst).data.as_mut_ptr(), len);
        (*dst).caps = moved;
    }
    arch::paging::sync_direct(to.utcb, core::mem::size_of::<Utcb>());
    Ok(Tag::new(tag.label(), len, ncaps).0 as u64)
}

/// `CALL`: send a message and wait for the reply. Returns (tag, 0).
pub fn call(ep: &Arc<Endpoint>, badge: u64, tag: u64) -> IpcResult {
    interrupts::disable();
    {
        let mut g = SCHED.lock();
        let s = g.as_mut().unwrap();
        let me = s.me();
        let my_id = s.threads[me].id;
        s.threads[me].ipc_result = None;
        let receiver = s
            .threads
            .iter()
            .position(|t| matches!(t.state, State::RecvBlocked { ep: e, .. } if e == ep.id));
        match receiver {
            Some(r) => {
                let t2 = transfer(&s.threads[me], &s.threads[r], Tag(tag as usize))?;
                let rt = &mut s.threads[r];
                rt.ipc_result = Some(Ok((t2, badge)));
                rt.reply_to = Some(my_id);
                rt.state = State::Ready;
                let rid = rt.id;
                s.threads[me].state = State::ReplyBlocked(rid);
                s.hint = Some(rid);
            }
            None => {
                ep.queue.lock().push_back((my_id, badge, tag));
                s.threads[me].state = State::SendBlocked(ep.id);
            }
        }
    }
    schedule();
    with(|s| s.cur().ipc_result.take()).unwrap_or(Err(Error::Dead))
}

fn notified(t: &mut Thread) -> Option<IpcResult> {
    (t.pending != 0).then(|| {
        Ok((
            Tag::new(LABEL_NOTIFIED, 0, 0).0 as u64,
            core::mem::take(&mut t.pending),
        ))
    })
}

/// `RECV`: wait for a message on `ep` (returns (tag, badge)), a
/// notification (returns (LABEL_NOTIFIED tag, bits)) or the deadline.
pub fn recv(ep: &Arc<Endpoint>, deadline: u64) -> IpcResult {
    interrupts::disable();
    {
        let mut g = SCHED.lock();
        let s = g.as_mut().unwrap();
        let me = s.me();
        if let Some(r) = notified(&mut s.threads[me]) {
            return r;
        }
        loop {
            let Some((stid, badge, tag)) = ep.queue.lock().pop_front() else {
                break;
            };
            let Some(si) = s.index(stid) else { continue };
            if s.threads[si].state != State::SendBlocked(ep.id) {
                continue;
            }
            match transfer(&s.threads[si], &s.threads[me], Tag(tag as usize)) {
                Ok(t2) => {
                    let my_id = s.threads[me].id;
                    s.threads[si].state = State::ReplyBlocked(my_id);
                    s.threads[me].reply_to = Some(stid);
                    return Ok((t2, badge));
                }
                Err(e) => {
                    // The caller sent a bad capability: fail its call.
                    s.threads[si].ipc_result = Some(Err(e));
                    s.threads[si].state = State::Ready;
                }
            }
        }
        if deadline <= time::now_us() {
            return Err(Error::Timeout);
        }
        let t = &mut s.threads[me];
        t.ipc_result = None;
        t.state = State::RecvBlocked {
            ep: ep.id,
            deadline,
        };
    }
    schedule();
    with(|s| {
        let t = s.cur();
        t.ipc_result
            .take()
            .or_else(|| notified(t))
            .unwrap_or(Err(Error::Timeout))
    })
}

/// Reply to the caller whose message we received last.
pub fn reply(tag: u64) -> Result<(), Error> {
    with(|s| {
        let me = s.me();
        let my_id = s.threads[me].id;
        let target = s.threads[me]
            .reply_to
            .take()
            .ok_or(Error::InvalidArgument)?;
        let ti = s.index(target).ok_or(Error::Dead)?;
        if s.threads[ti].state != State::ReplyBlocked(my_id) {
            return Err(Error::Dead);
        }
        let r = transfer(&s.threads[me], &s.threads[ti], Tag(tag as usize));
        let t = &mut s.threads[ti];
        t.ipc_result = Some(r.map(|t2| (t2, 0)));
        t.state = State::Ready;
        s.hint = Some(target);
        // The caller can run on an idle core while we go on.
        s.kick_idle();
        r.map(|_| ())
    })
}

