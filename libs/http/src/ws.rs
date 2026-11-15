//! The WebSocket frame layer (RFC 6455) of a client: frames out (always
//! masked), frames in and messages put back together, plus the base64 of
//! the opening handshake. No I/O: the caller owns the socket, and hands
//! in the random bytes for keys and masks.
//!
//! Everything read here comes from the network, so lengths are checked
//! before they are used, and a message has an upper size.

use alloc::string::String;
use alloc::vec::Vec;

pub mod opcode {
    pub const CONTINUATION: u8 = 0;
    pub const TEXT: u8 = 1;
    pub const BINARY: u8 = 2;
    pub const CLOSE: u8 = 8;
    pub const PING: u8 = 9;
    pub const PONG: u8 = 10;
}

/// Base64 (standard alphabet, padded), for `Sec-WebSocket-Key`.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One final frame from a client: `mask` must be fresh random bytes.
pub fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::with_capacity(payload.len() + 14);
    f.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n < 65536 => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// What the server sent, as far as a client cares.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A whole text message (fragments put together).
    Text(String),
    /// Answer with a pong carrying the same payload.
    Ping(Vec<u8>),
    Close,
}

/// A frame or message longer than the reader's limit.
#[derive(Debug, PartialEq, Eq)]
pub struct TooLong;

/// Collects received bytes and hands out events.
pub struct Reader {
    buf: Vec<u8>,
    /// The fragments of a text message so far.
    partial: Vec<u8>,
    max: usize,
}

impl Reader {
    /// `max`: the largest message (and frame) accepted.
    pub fn new(max: usize) -> Reader {
        Reader { buf: Vec::new(), partial: Vec::new(), max }
    }

    /// Bytes from the connection.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next event, if one is complete. After `Err(TooLong)` the
    /// connection is beyond repair: close it.
    pub fn next(&mut self) -> Result<Option<Event>, TooLong> {
        while let Some((fin, op, payload)) = take_frame(&mut self.buf, self.max)? {
            match op {
                opcode::CONTINUATION | opcode::TEXT => {
                    if self.partial.len() + payload.len() > self.max {
                        return Err(TooLong);
                    }
                    self.partial.extend_from_slice(&payload);
                    if fin {
                        let msg = core::mem::take(&mut self.partial);
                        return Ok(Some(Event::Text(String::from_utf8_lossy(&msg).into_owned())));
                    }
                }
                opcode::CLOSE => return Ok(Some(Event::Close)),
                opcode::PING => return Ok(Some(Event::Ping(payload))),
                _ => {} // pong, binary: not used
            }
        }
        Ok(None)
    }
}

/// One complete frame from the front of `buf`, removed from it: (fin,
/// opcode, unmasked payload). None while it is incomplete.
fn take_frame(buf: &mut Vec<u8>, max: usize) -> Result<Option<(bool, u8, Vec<u8>)>, TooLong> {
    let b = &buf[..];
    if b.len() < 2 {
        return Ok(None);
    }
    let (fin, op) = (b[0] & 0x80 != 0, b[0] & 0x0f);
    let masked = b[1] & 0x80 != 0;
    let (len, mut off) = match b[1] & 0x7f {
        126 if b.len() >= 4 => (u16::from_be_bytes([b[2], b[3]]) as u64, 4),
        127 if b.len() >= 10 => (u64::from_be_bytes(b[2..10].try_into().unwrap()), 10),
        126 | 127 => return Ok(None),
        n => (n as u64, 2),
    };
    // Checked as u64 first: on a 32-bit CPU the length would wrap in usize.
    if len > max as u64 {
        return Err(TooLong);
    }
    let len = len as usize;
    let mask = if masked {
        let Some(m) = b.get(off..off + 4) else { return Ok(None) };
        off += 4;
        Some([m[0], m[1], m[2], m[3]])
    } else {
        None
    };
    let Some(data) = b.get(off..off + len) else { return Ok(None) };
    let mut payload = data.to_vec();
    if let Some(m) = mask {
        for (i, x) in payload.iter_mut().enumerate() {
            *x ^= m[i % 4];
        }
    }
    buf.drain(..off + len);
    Ok(Some((fin, op, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A frame as a server sends it (unmasked).
    fn server_frame(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 } else { 0 } | op];
        match payload.len() {
            n if n < 126 => f.push(n as u8),
            n if n < 65536 => {
                f.push(126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn base64_vectors() {
        // RFC 4648 test vectors.
        let v = [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")];
        for (i, o) in v {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn client_frames_round_trip() {
        for len in [0, 5, 125, 126, 300, 65535, 65536, 70000] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let f = encode_frame(opcode::TEXT, &payload, [1, 2, 3, 4]);
            assert_eq!(f[1] & 0x80, 0x80, "a client always masks");
            let mut buf = f.clone();
            let (fin, op, got) = take_frame(&mut buf, 1 << 20).unwrap().unwrap();
            assert!(fin && op == opcode::TEXT && got == payload && buf.is_empty(), "length {len}");
        }
    }

    #[test]
    fn messages_from_pieces() {
        let mut r = Reader::new(1 << 20);
        let mut wire = server_frame(false, opcode::TEXT, b"hel");
        wire.extend(server_frame(true, opcode::PING, b"p"));
        wire.extend(server_frame(true, opcode::CONTINUATION, b"lo"));
        wire.extend(server_frame(true, opcode::CLOSE, b""));
        // One byte at a time: nothing comes out before it is whole.
        let mut events = Vec::new();
        for b in &wire {
            r.push(&[*b]);
            while let Some(e) = r.next().unwrap() {
                events.push(e);
            }
        }
        assert_eq!(events, [Event::Ping(b"p".to_vec()), Event::Text("hello".into()), Event::Close]);
    }

    #[test]
    fn lengths_from_the_network_are_limited() {
        // A 64-bit length that wraps on a 32-bit CPU, a length over the
        // limit, and fragments that add up to more than the limit.
        let mut r = Reader::new(1000);
        r.push(&[0x81, 127, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfc]);
        assert_eq!(r.next(), Err(TooLong));

        let mut r = Reader::new(1000);
        r.push(&server_frame(true, opcode::TEXT, &[b'x'; 1001]));
        assert_eq!(r.next(), Err(TooLong));

        let mut r = Reader::new(1000);
        r.push(&server_frame(false, opcode::TEXT, &[b'x'; 600]));
        r.push(&server_frame(true, opcode::CONTINUATION, &[b'x'; 600]));
        assert_eq!(r.next(), Err(TooLong));
    }

    #[test]
    fn incomplete_frames_wait() {
        let mut r = Reader::new(1000);
        r.push(&[0x81, 126, 0x01]); // the 16-bit length is cut off
        assert_eq!(r.next(), Ok(None));
        r.push(&[0x00]);
        assert_eq!(r.next(), Ok(None)); // 256 bytes announced, none there
        r.push(&[b'a'; 256]);
        assert_eq!(r.next(), Ok(Some(Event::Text("a".repeat(256)))));
        // A masked frame (servers don't, but the mask is honoured).
        let mut f = vec![0x81, 0x82, 1, 2, 3, 4];
        f.extend([b'o' ^ 1, b'k' ^ 2]);
        r.push(&f[..4]);
        assert_eq!(r.next(), Ok(None));
        r.push(&f[4..]);
        assert_eq!(r.next(), Ok(Some(Event::Text("ok".into()))));
    }
}
