//! Tests for the microkernel's IPC and device access, started by init when
//! the kernel command line has `selftest`. Modes:
//!
//! - `pong`: serve the "ep" endpoint: label 1 echoes a number + 1; label 2
//!   receives a ring (memory capability) and a notification capability and
//!   streams numbered messages into the ring.
//! - `ping`: measure call round trips, pass capabilities, read the ring.
//! - `mmio`: read a hardware counter through an MMIO capability.
//! - `atomics`: atomic adds from two processes under preemption.
//! - `random`: random bytes from the kernel.
//! - `cfg`: the config server's capabilities, watchers and patches.
//! - `rights`: a system capability's missing rights are refused.
//! - `kernel-input`: malformed syscall input refused, never a panic.
//! - `restart`: init restarts a program at once when it asks to.
//! - `crashy`: panic after 200 ms (init restarts it, then gives up).

#![no_std]
#![no_main]

extern crate alloc;

use log::{error, info};
use touchos_abi::{Error, Tag, prot};
use touchos_rt::shm::{self, Pushed};
use touchos_rt::{cap, exec, ipc, sys, time};

touchos_rt::entry!(main);

const ECHO: u32 = 1;
const RING: u32 = 2;
const RING_SLOTS: u32 = 64;
const RING_SLOT: u32 = 64;
const RING_MSGS: u32 = 20_000;

fn main() {
    match touchos_rt::cmdline().split_whitespace().next().unwrap_or("") {
        "pong" => pong(),
        "ping" => ping(),
        "mmio" => mmio(),
        "atomics" => atomics(),
        "random" => random(),
        "cfg" => cfg(),
        "rights" => rights(),
        "kernel-input" => kernel_input(),
        "restart" => restart(),
        "crashy" => {
            time::sleep_ms(200);
            panic!("crashing on purpose");
        }
        other => error!("unknown mode \"{other}\""),
    }
}

/// A system capability without the WATCHDOG right: the watchdog is
/// init's alone.
fn rights() {
    let Some(system) = touchos_rt::cap("system") else {
        error!("FAIL: no system capability");
        return;
    };
    match sys::watchdog(system, touchos_abi::watchdog::STOP) {
        Err(_) => info!("PASS: watchdog refused without its right"),
        Ok(_) => error!("FAIL: stopped the watchdog without the WATCHDOG right"),
    }
}

use touchos_rt::ipc::reply;

fn pong() {
    let ep = cap("ep").expect("no endpoint");
    exec::serve(ep, |msg, badge| match msg.label {
        ECHO => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&msg.data()[..8]);
            let v = u64::from_le_bytes(b) + 1;
            reply(0, &(v ^ badge as u64).to_le_bytes())
        }
        RING if msg.caps().len() == 1 => {
            // Reply now; stream from a task (servers never block in a request).
            exec::spawn(produce(msg.caps()[0]));
            reply(0, &[])
        }
        _ => reply(u32::MAX, &[]),
    })
}

/// The producer of the ring test: set it up, then push, waking the
/// consumer through MEM_NOTIFY when it may sleep (as the drivers do).
async fn produce(mem: u32) {
    let Some(ring) = shm::ring_of(mem) else {
        error!("FAIL: pong can't map the ring");
        return;
    };
    ring.init(RING_SLOTS, RING_SLOT);
    let mut msg = [0u8; RING_SLOT as usize];
    for i in 0..RING_MSGS {
        msg[..4].copy_from_slice(&i.to_le_bytes());
        loop {
            match ring.push(&msg) {
                Pushed::Wake => {
                    let _ = sys::mem_notify(ring.mem());
                    break;
                }
                Pushed::Queued => break,
                Pushed::Full => {
                    sys::yield_now();
                    exec::yield_now().await;
                }
            }
        }
    }
}

fn ping() {
    let ep = cap("ep").expect("no endpoint");

    // Round trips. pong XORs the badge into the answer: proves the badge
    // that init minted arrives.
    const N: u64 = 5000;
    let t0 = time::now_us();
    for i in 0..N {
        match ipc::call(ep, ECHO, &i.to_le_bytes(), &[]) {
            Ok(m) if m.data().len() == 8 => {
                let mut b = [0u8; 8];
                b.copy_from_slice(m.data());
                if u64::from_le_bytes(b) != (i + 1) ^ 42 {
                    error!("FAIL: wrong echo for {i}");
                    return;
                }
            }
            r => {
                error!("FAIL: call {i}: {:?}", r.map(|m| m.label));
                return;
            }
        }
    }
    let dt = time::now_us() - t0;
    info!(
        "PASS: {N} IPC round trips with badge, {}.{} us each",
        dt / N,
        dt * 10 / N % 10
    );

    // A bad capability slot in a message must fail the call, not the server.
    let u = ipc::utcb();
    u.caps[0] = 200;
    match sys::call(ep, Tag::new(ECHO, 8, 1)) {
        Err(Error::InvalidCap) => info!("PASS: call with a bad capability refused"),
        r => error!("FAIL: call with a bad capability: {r:?}"),
    }

    // Shared memory ring: create it, send it and our notification to pong.
    // (Wait until init has stopped the CPU-burning "forever" test, which
    // would otherwise get a full time slice between ring batches.)
    time::sleep_ms(1500);
    let len = shm::ring_size(RING_SLOTS, RING_SLOT) as u64;
    let mem = sys::mem_create(len, 0).expect("mem_create");
    let Some(c) = shm::ring_of(mem) else {
        error!("FAIL: can't map the ring");
        return;
    };
    // Woken through MEM_WAIT when pong pushes into an empty ring.
    let _ = sys::mem_wait(mem, 1 << 7);
    match ipc::call(ep, RING, &[], &[mem]) {
        Ok(m) if m.label == 0 => {}
        r => {
            error!("FAIL: ring set-up: {:?}", r.map(|m| m.label));
            return;
        }
    }
    let mut buf = [0u8; RING_SLOT as usize];
    let t0 = time::now_us();
    let mut next = 0u32;
    let mut waits = 0u32;
    while next < RING_MSGS {
        match c.pop(&mut buf) {
            Some(_) => {
                let got = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                if got != next {
                    error!("FAIL: ring message {got}, expected {next}");
                    return;
                }
                next += 1;
            }
            None => {
                if time::now_us() - t0 > 10_000_000 {
                    error!("FAIL: ring stalled at message {next}");
                    return;
                }
                waits += 1;
                let _ = sys::wait(time::now_us() + 100_000);
            }
        }
    }
    let dt = (time::now_us() - t0).max(1);
    info!(
        "PASS: {RING_MSGS} ring messages in order, {} k/s, {waits} waits",
        RING_MSGS as u64 * 1000 / dt
    );
}

/// `atomics N`: two copies share a page and each add 1 to a 32-bit and a
/// 64-bit counter N times. Timer interrupts preempt them in the middle of
/// read-modify-write sequences (on ARMv5: in the kernel's restartable
/// helpers), so the totals only add up if the atomics are atomic.
/// Asks init for a restart (exit::RESTART) twice: each time it must be
/// started again at once, not after a failure's backoff (1 s, then 2 s).
/// "shared" (zeroed, kept by init across the restarts) holds the count and
/// the start times.
fn restart() {
    let mem = cap("shared").expect("no shared memory");
    let va = sys::mem_map(mem, 0, 4096, prot::WRITE).expect("mem_map");
    let starts = unsafe { core::slice::from_raw_parts_mut(va as usize as *mut u64, 4) };
    let n = starts[0] as usize;
    if n > 2 {
        error!("FAIL: started {} times", n + 1);
        return;
    }
    starts[1 + n] = time::now_ms();
    starts[0] = n as u64 + 1;
    if n < 2 {
        touchos_rt::sys::exit(touchos_abi::exit::RESTART);
    }
    let gaps = [starts[2] - starts[1], starts[3] - starts[2]];
    // (A failure waits 1 s before its restart; starting a process in
    // QEMU's ARM takes about half that.)
    if gaps.iter().all(|&g| g < 1000) {
        info!("PASS: restarted at once when asked, twice ({} and {} ms)", gaps[0], gaps[1]);
    } else {
        error!("FAIL: asked restarts waited {} and {} ms (counted as failures?)", gaps[0], gaps[1]);
    }
}

fn atomics() {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
    #[repr(C)]
    struct Shared {
        c32: AtomicU32,
        done: AtomicU32,
        c64: AtomicU64,
    }
    let n: u32 = touchos_rt::cmdline().split_whitespace().nth(1).and_then(|w| w.parse().ok()).unwrap_or(100_000);
    let mem = cap("shared").expect("no shared memory");
    let va = sys::mem_map(mem, 0, 4096, prot::WRITE).expect("mem_map");
    let s = unsafe { &*(va as usize as *const Shared) };
    let t0 = time::now_us();
    for _ in 0..n {
        s.c32.fetch_add(1, AcqRel);
        s.c64.fetch_add(1 << 32 | 1, AcqRel); // carries across the halves
    }
    let us = time::now_us() - t0;
    if s.done.fetch_add(1, AcqRel) == 1 {
        // The second to finish checks.
        let (c32, c64) = (s.c32.load(Acquire), s.c64.load(Acquire));
        let want = 2 * n as u64;
        if c32 as u64 == want && c64 == want << 32 | want {
            info!("PASS: 2 x {n} atomic adds under preemption: {c32} and {c64:#x} ({} ns per pair)", us * 1000 / n as u64);
        } else {
            error!("FAIL: atomic adds lost: u32 {c32}, u64 {c64:#x}, expected {want} and {:#x}", want << 32 | want);
        }
    }
}

/// `random`: 4 KiB from `rt::rand` (the kernel's pool): about
/// half the bits set, and a second request differs from the first.
/// Malformed input from a process must be refused by the kernel, never
/// panic it. "system" carries only the right to read the kernel log.
fn kernel_input() {
    // An ELF image whose program headers start a few bytes before the end
    // of the address space (any process may ask to spawn one).
    let mut elf = [0u8; 64];
    elf[0..4].copy_from_slice(b"\x7fELF");
    elf[4] = 1; // ELF32
    elf[5] = 1; // little-endian
    elf[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    elf[18..20].copy_from_slice(&40u16.to_le_bytes()); // ARM
    elf[28..32].copy_from_slice(&(u32::MAX - 8).to_le_bytes()); // e_phoff
    elf[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
    elf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
    let spawned = sys::mem_create(4096, 0).and_then(|mem| {
        let va = sys::mem_map(mem, 0, 4096, prot::READ | prot::WRITE)?;
        unsafe { core::ptr::copy_nonoverlapping(elf.as_ptr(), va as *mut u8, elf.len()) };
        sys::mem_unmap(va)?;
        let args: touchos_abi::SpawnArgs = unsafe { core::mem::zeroed() };
        let r = sys::proc_spawn(mem, 0, elf.len() as u64, &args);
        let _ = sys::cap_drop(mem);
        r
    });
    match spawned {
        Err(e) => info!("PASS: ELF with program headers past the end refused ({e:?})"),
        Ok(_) => error!("FAIL: ELF with program headers past the end was started"),
    }

    // Reading the kernel log into an empty buffer.
    match cap("system").map(|s| sys::klog_read(s, &mut [], 0)) {
        Some(Ok((0, _))) => info!("PASS: kernel log read into an empty buffer"),
        r => error!("FAIL: kernel log read into an empty buffer: {r:?}"),
    }
}

fn random() {
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    touchos_rt::rand::fill(&mut a);
    touchos_rt::rand::fill(&mut b);
    let ones: u32 = a.iter().map(|x| x.count_ones()).sum();
    let half = 4096 * 4;
    if a != b && ones.abs_diff(half) < 400 {
        info!("PASS: random bytes: {ones} of {} bits set, two requests differ", 2 * half);
    } else {
        error!("FAIL: random bytes: {ones} of {} bits set, equal: {}", 2 * half, a == b);
    }
}

/// `mmio OFFSET BITS`: a free-running counter at OFFSET (32 or 64 bits wide)
/// in the "counter" MMIO capability must move.
fn mmio() {
    let mut args = touchos_rt::cmdline().split_whitespace().skip(1);
    let off = args
        .next()
        .and_then(|a| u64::from_str_radix(a.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0xf0);
    let wide = args.next() != Some("32");
    let counter = cap("counter").expect("no counter");
    let base = sys::mmio_map(counter, 0, 0x1000).expect("mmio_map");
    let read = || unsafe {
        let p = base + off;
        if wide { (p as usize as *const u64).read_volatile() } else { (p as usize as *const u32).read_volatile() as u64 }
    };
    let a = read();
    time::sleep_ms(10);
    let b = read();
    if b > a {
        info!("PASS: hardware counter through an MMIO capability: {a} -> {b}");
    } else {
        error!("FAIL: hardware counter didn't move ({a} -> {b})");
    }
    match sys::mmio_map(counter, 0, 0x2000) {
        Err(_) => info!("PASS: mapping beyond the MMIO capability refused"),
        Ok(_) => error!("FAIL: mapped beyond the MMIO capability"),
    }
}

fn cfg() {
    use touchos_config::client::{Client, Update};
    let (Some(public), Some(full), Some(admin)) = (
        Client::new("config_pub"),
        Client::new("config_full"),
        Client::new("config_admin"),
    ) else {
        error!("FAIL: missing config capabilities");
        return;
    };
    let (v, full_text) = full.text().expect("get");
    let (_, pub_text) = public.text().expect("get");
    let (_, pub_cfg) = public.get().expect("get");
    let full_cfg = touchos_config::Config::parse(&full_text)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let token = full_cfg.homeassistant.token.clone().unwrap_or_default();
    if !token.is_empty()
        && !pub_text.contains(&token)
        && pub_cfg.homeassistant.token.as_deref() == Some(touchos_config::patch::REDACTED)
    {
        info!("PASS: config read; secrets only with the full capability");
    } else if token.is_empty() {
        info!("SKIP: no token in this config to test redaction");
    } else {
        error!("FAIL: secrets visible through the public capability");
    }

    let bit = 1usize << 3;
    public.watch(bit).expect("watch");
    match admin.update(v, "display.brightness = 33") {
        Ok(Update::Ok { version, .. }) if version == v + 1 => {}
        r => {
            error!("FAIL: update: {r:?}");
            return;
        }
    }
    match sys::wait(time::now_us() + 1_000_000) {
        Ok(b) if b & bit != 0 => info!("PASS: watcher notified of the change"),
        r => error!("FAIL: no change notification ({r:?})"),
    }
    match public.get() {
        Ok((v2, c)) if v2 == v + 1 && c.display.brightness == 33 => {
            info!("PASS: update applied (brightness 33, version {v2})")
        }
        r => error!(
            "FAIL: after update: {:?}",
            r.map(|(v, c)| (v, c.display.brightness))
        ),
    }
    match admin.update(v, "display.brightness = 40") {
        Ok(Update::Conflict(cur)) if cur == v + 1 => info!("PASS: stale update refused (conflict)"),
        r => error!("FAIL: stale update: {r:?}"),
    }
    match admin.update(v + 1, "display.brightness = \"x\"\nnope.key = 1") {
        Ok(Update::Invalid(why)) if why.lines().count() == 2 => {
            info!("PASS: invalid update refused: {}", why.replace('\n', "; "))
        }
        r => error!("FAIL: invalid update: {r:?}"),
    }
    match public.update(v + 1, "display.brightness = 50") {
        Ok(Update::Denied) => info!("PASS: update without the admin capability refused"),
        r => error!("FAIL: update with the public capability: {r:?}"),
    }
}
