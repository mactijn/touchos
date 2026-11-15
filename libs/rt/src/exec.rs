//! A single-threaded async executor for user processes: `spawn`, `run`,
//! `sleep_ms`, `yield_now`.
//!
//! When no task can run, the process blocks in the kernel (`WAIT`, or `RECV`
//! for a server) until the next timer deadline or a notification. Each
//! notification bit wakes the tasks waiting for it ([`wait_bits`]).

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use touchos_abi::{Error, Tag};

use crate::{ipc, sys, time};

type BoxFuture = Pin<Box<dyn Future<Output = ()>>>;

struct Flag(AtomicBool);
impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

struct Task {
    fut: BoxFuture,
    woken: Arc<Flag>,
    waker: Waker,
}

/// A global for a process's state. Every process has one thread and no
/// signal handlers, so a RefCell behind a Sync wrapper is enough; using it
/// again while it is in use (a nested `with`) panics, so init restarts the
/// process (a spin lock would hang silently instead).
pub struct Local<T>(RefCell<T>);
unsafe impl<T> Sync for Local<T> {}

impl<T> Local<T> {
    pub const fn new(v: T) -> Local<T> {
        Local(RefCell::new(v))
    }

    /// Run `f` with the value.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }
}

static SPAWNED: Local<Vec<Task>> = Local(RefCell::new(Vec::new()));
static TIMERS: Local<Vec<(u64, Waker)>> = Local(RefCell::new(Vec::new()));
static BIT_WAKERS: Local<Vec<(usize, Waker)>> = Local(RefCell::new(Vec::new()));
/// Notification bits received and not yet taken by a [`wait_bits`].
static PENDING: AtomicUsize = AtomicUsize::new(0);
/// Bits handed out by [`alloc_bit`].
static USED_BITS: AtomicUsize = AtomicUsize::new(0);

pub fn spawn(fut: impl Future<Output = ()> + 'static) {
    let woken = Arc::new(Flag(AtomicBool::new(true)));
    let waker = Waker::from(woken.clone());
    SPAWNED.0.borrow_mut().push(Task {
        fut: Box::pin(fut),
        woken,
        waker,
    });
}

/// A notification bit of our own (for a ring, a timer, a death notice...).
pub fn alloc_bit() -> usize {
    for i in 0..usize::BITS {
        let b = 1usize << i;
        if USED_BITS.fetch_or(b, Ordering::Relaxed) & b == 0 {
            return b;
        }
    }
    panic!("all {} notification bits are in use", usize::BITS);
}

/// Give a bit from [`alloc_bit`] back.
pub fn free_bit(bit: usize) {
    PENDING.fetch_and(!bit, Ordering::Relaxed);
    USED_BITS.fetch_and(!bit, Ordering::Relaxed);
}

/// Wait for one of `mask` or `ms` milliseconds, whichever comes first.
pub async fn wait_bits_or_ms(mask: usize, ms: u64) -> usize {
    let deadline = time::now_us() + ms * 1000;
    WaitBitsUntil {
        mask,
        deadline,
        registered: false,
    }
    .await
}

struct WaitBitsUntil {
    mask: usize,
    deadline: u64,
    registered: bool,
}

impl Future for WaitBitsUntil {
    type Output = usize;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        let got = PENDING.fetch_and(!self.mask, Ordering::Relaxed) & self.mask;
        if got != 0 || time::now_us() >= self.deadline {
            return Poll::Ready(got);
        }
        register_bits(self.mask, cx.waker());
        if !self.registered {
            TIMERS
                .0
                .borrow_mut()
                .push((self.deadline, cx.waker().clone()));
            self.registered = true;
        }
        Poll::Pending
    }
}

/// Wake `w` when one of `mask` is signalled (once per waker and mask).
fn register_bits(mask: usize, w: &Waker) {
    let mut list = BIT_WAKERS.0.borrow_mut();
    if !list.iter().any(|(m, x)| *m == mask && x.will_wake(w)) {
        list.push((mask, w.clone()));
    }
}

fn dispatch_bits(bits: usize) {
    if bits == 0 {
        return;
    }
    PENDING.fetch_or(bits, Ordering::Relaxed);
    BIT_WAKERS.0.borrow_mut().retain(|(mask, w)| {
        if mask & bits != 0 {
            w.wake_by_ref();
            false
        } else {
            true
        }
    });
}

/// Set notification bits of our own, without the kernel: for one task to
/// wake another in this process (the tasks waiting for them run next).
pub fn raise(bits: usize) {
    dispatch_bits(bits);
}

/// Take the bits of `mask` that were signalled (without waiting).
pub fn take_bits(mask: usize) -> usize {
    PENDING.fetch_and(!mask, Ordering::Relaxed) & mask
}

/// Wait until one of the bits in `mask` is signalled; returns those bits.
pub fn wait_bits(mask: usize) -> WaitBits {
    WaitBits(mask)
}

pub struct WaitBits(usize);

impl Future for WaitBits {
    type Output = usize;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        let got = PENDING.fetch_and(!self.0, Ordering::Relaxed) & self.0;
        if got != 0 {
            return Poll::Ready(got);
        }
        register_bits(self.0, cx.waker());
        Poll::Pending
    }
}

/// Run ready tasks once; returns (whether any ran, earliest timer).
fn poll_tasks(tasks: &mut Vec<Task>) -> (bool, u64) {
    tasks.append(&mut SPAWNED.0.borrow_mut());
    let now = time::now_us();
    TIMERS.0.borrow_mut().retain(|(deadline, waker)| {
        if *deadline <= now {
            waker.wake_by_ref();
            false
        } else {
            true
        }
    });
    let mut any_ran = false;
    tasks.retain_mut(|t| {
        if !t.woken.0.swap(false, Ordering::Acquire) {
            return true;
        }
        any_ran = true;
        let mut cx = Context::from_waker(&t.waker);
        t.fut.as_mut().poll(&mut cx).is_pending()
    });
    let busy = any_ran || !SPAWNED.0.borrow().is_empty();
    let next = if busy {
        0
    } else {
        TIMERS
            .0
            .borrow()
            .iter()
            .map(|(d, _)| *d)
            .min()
            .unwrap_or(u64::MAX)
    };
    (busy, next)
}

/// Run all tasks forever.
pub fn run() -> ! {
    let mut tasks = Vec::new();
    loop {
        let (_, deadline) = poll_tasks(&mut tasks);
        if let Ok(bits) = sys::wait(deadline) {
            dispatch_bits(bits);
        }
    }
}

/// Run all tasks and serve requests on `ep`. `handler` gets each request
/// (the payload is in the UTCB, see [`ipc::Msg`]) and its badge, and must
/// put the reply in the UTCB and return its tag right away: it may not wait.
pub fn serve(ep: u32, mut handler: impl FnMut(ipc::Msg, usize) -> Tag) -> ! {
    let mut tasks = Vec::new();
    let mut reply: Option<Tag> = None;
    loop {
        let (_, deadline) = poll_tasks(&mut tasks);
        let r = match reply.take() {
            Some(t) => sys::reply_recv(ep, t, deadline),
            None => sys::recv(ep, deadline),
        };
        match r {
            Ok((tag, x)) if tag.label() == touchos_abi::LABEL_NOTIFIED => dispatch_bits(x),
            Ok((tag, badge)) => reply = Some(handler(ipc::Msg::from_tag(tag), badge)),
            Err(Error::Timeout) => {}
            Err(e) => {
                ::log::warn!("serve: {e:?}");
                sys::yield_now();
            }
        }
    }
}

/// Sleep for at least `ms` milliseconds (resolution: one timer tick, 4 ms).
pub fn sleep_ms(ms: u64) -> Sleep {
    Sleep {
        deadline: time::now_us() + ms * 1000,
        registered: false,
    }
}

pub struct Sleep {
    deadline: u64,
    registered: bool,
}

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if time::now_us() >= self.deadline {
            return Poll::Ready(());
        }
        if !self.registered {
            TIMERS
                .0
                .borrow_mut()
                .push((self.deadline, cx.waker().clone()));
            self.registered = true;
        }
        Poll::Pending
    }
}

/// Let other tasks run, then continue.
pub fn yield_now() -> YieldNow {
    YieldNow(false)
}

pub struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
