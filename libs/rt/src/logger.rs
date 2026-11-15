//! `log` backend: each record becomes one `SYS_LOG` call. The kernel adds
//! the process name, so all processes share one ordered log.

use core::fmt::Write;
use touchos_abi::level;

use crate::sys;

struct Logger;

/// Crates whose debug and trace messages we drop.
const LIBS: &[&str] = &["embedded_tls", "smoltcp", "p384", "rsa"];

/// Formats into a fixed buffer; long messages are cut off.
struct Buf {
    data: [u8; 480],
    len: usize,
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.data.len() - self.len;
        let mut n = s.len().min(room);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

impl log::Log for Logger {
    /// Everything from the program itself; libraries (TLS, TCP/IP) only
    /// from Info up, or their debug output floods the log.
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info || !LIBS.iter().any(|l| m.target().starts_with(l))
    }
    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut buf = Buf {
            data: [0; 480],
            len: 0,
        };
        let _ = write!(buf, "{}", record.args());
        let lvl = match record.level() {
            log::Level::Error => level::ERROR,
            log::Level::Warn => level::WARN,
            log::Level::Info => level::INFO,
            log::Level::Debug => level::DEBUG,
            log::Level::Trace => level::TRACE,
        };
        // Valid UTF-8: Buf only cuts at char boundaries.
        sys::log(lvl, unsafe {
            core::str::from_utf8_unchecked(&buf.data[..buf.len])
        });
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Trace);
}
