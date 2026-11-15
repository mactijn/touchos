//! Kernel logging: `log` crate facade -> serial port + framebuffer console.

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write;
use spin::Mutex;

use crate::console::{self, Console};
use crate::board::serial::Serial;

pub static SERIAL: Mutex<Serial> = Mutex::new(Serial::new());
pub static CONSOLE: Mutex<Console> = Mutex::new(Console::new());

/// The last lines of the log with sequence numbers, for `KLOG_READ` (the
/// user-space network server ships them over UDP).
struct Ring {
    first_seq: u64,
    lines: VecDeque<String>,
}
static RING: Mutex<Ring> = Mutex::new(Ring {
    first_seq: 0,
    lines: VecDeque::new(),
});
const RING_MAX: usize = 2000;

/// Copy whole lines with sequence number >= `cursor` into `out` (each ends
/// with '\n'). Returns (bytes written, next cursor).
pub fn ring_read(cursor: u64, out: &mut [u8]) -> (usize, u64) {
    if out.is_empty() {
        return (0, cursor); // (KLOG_READ with length 0)
    }
    let r = RING.lock();
    let mut seq = cursor.max(r.first_seq);
    let mut n = 0;
    while let Some(line) = r.lines.get((seq - r.first_seq) as usize) {
        if n + line.len() + 1 > out.len() {
            if n == 0 {
                // A line longer than the buffer: send its start.
                let k = out.len().saturating_sub(1);
                out[..k].copy_from_slice(&line.as_bytes()[..k]);
                out[k] = b'\n';
                return (out.len(), seq + 1);
            }
            break;
        }
        out[n..n + line.len()].copy_from_slice(line.as_bytes());
        out[n + line.len()] = b'\n';
        n += line.len() + 1;
        seq += 1;
    }
    (n, seq)
}

fn ring_push(level: char, args: &core::fmt::Arguments) {
    if !crate::mem::heap_ready() {
        return;
    }
    let ms = if crate::time::is_calibrated() {
        crate::time::now_ms()
    } else {
        0
    };
    let mut line = String::new();
    let _ = write!(line, "[{:>5}.{:03}] {level} {args}", ms / 1000, ms % 1000);
    {
        let mut r = RING.lock();
        if r.lines.len() >= RING_MAX {
            r.lines.pop_front();
            r.first_seq += 1;
        }
        r.lines.push_back(line);
    }
}

/// The framebuffer while a process (the UI) has it: (process id, console fb).
static LENT_FB: Mutex<Option<(u32, console::Framebuffer)>> = Mutex::new(None);

/// A process mapped the framebuffer: the console stops drawing.
pub fn framebuffer_taken(pid: u32) {
    if let Some(fb) = CONSOLE.lock().take_framebuffer() {
        *LENT_FB.lock() = Some((pid, fb));
    }
}

/// A process ended: if it had the framebuffer, the console gets it back.
pub fn process_ended(pid: u32) {
    let mut lent = LENT_FB.lock();
    if lent.as_ref().is_some_and(|(p, _)| *p == pid) {
        let (_, fb) = lent.take().unwrap();
        drop(lent);
        CONSOLE.lock().attach(fb);
        ::log::warn!("console: the UI process ended; showing the log");
    }
}

struct KernelLogger;

impl ::log::Log for KernelLogger {
    fn enabled(&self, _: &::log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &::log::Record) {
        use ::log::Level::*;
        let (tag, color) = match record.level() {
            Error => ("E", console::RED),
            Warn => ("W", console::YELLOW),
            Info => ("I", console::FG),
            Debug => ("D", console::DIM),
            Trace => ("T", console::DIM),
        };
        let _ = writeln!(SERIAL.lock(), "[{tag}] {}", record.args());
        ring_push(tag.chars().next().unwrap_or('?'), record.args());
        if record.level() > ::log::Level::Info {
            return; // debug/trace: serial and network log only
        }
        let mut con = CONSOLE.lock();
        con.set_color(color);
        let _ = writeln!(con, "{}", record.args());
        con.set_color(console::FG);
    }

    fn flush(&self) {}
}

static LOGGER: KernelLogger = KernelLogger;

pub fn init() {
    SERIAL.lock().init();
    let _ = ::log::set_logger(&LOGGER);
    ::log::set_max_level(::log::LevelFilter::Trace);
}

/// Print without a log level/newline (used for tables and the panic screen).
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::klog::SERIAL.lock(), $($arg)*);
        let _ = write!($crate::klog::CONSOLE.lock(), $($arg)*);
    }};
}
