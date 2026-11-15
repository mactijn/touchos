//! A WebSocket client connection (text messages, ping/pong, close): the
//! socket side. The frames themselves are touchos_http::ws.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use touchos_http::ws::{self, Event, Reader, opcode};
use touchos_netc::{Conn, NetError};
use touchos_rt::{exec, time};

/// The largest message accepted from Home Assistant. get_states carries
/// every entity with its attributes; a large installation sends a few
/// MiB. Beyond this the connection is dropped and made again.
const MAX_MESSAGE: usize = 16 << 20;

pub struct WebSocket<'a> {
    tcp: Conn<'a>,
    chunk: Vec<u8>,
    reader: Reader,
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    touchos_rt::rand::fill(&mut b);
    b
}

impl<'a> WebSocket<'a> {
    /// HTTP upgrade on an established TCP connection.
    pub async fn handshake(
        mut tcp: Conn<'a>,
        host: &str,
        path: &str,
    ) -> Result<WebSocket<'a>, NetError> {
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: touchos\r\n\r\n",
            ws::base64(&random::<16>())
        );
        tcp.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let end = time::now_ms() + 10_000;
        let header_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if time::now_ms() > end || buf.len() > 16 * 1024 {
                return Err(NetError::Timeout);
            }
            let n = tcp.read(&mut chunk).await?;
            if n == 0 {
                return Err(NetError::Closed);
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let status = core::str::from_utf8(&buf[..header_end])
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        if !status.contains(" 101") {
            ::log::warn!("ha: websocket upgrade refused: {status}");
            return Err(NetError::ConnectFailed);
        }
        let mut reader = Reader::new(MAX_MESSAGE);
        reader.push(&buf[header_end..]);
        Ok(WebSocket {
            tcp,
            chunk: alloc::vec![0; touchos_netc::tls::RECORD_BUF],
            reader,
        })
    }

    async fn send_frame(&mut self, op: u8, payload: &[u8]) -> Result<(), NetError> {
        self.tcp.write_all(&ws::encode_frame(op, payload, random())).await
    }

    /// The bit signalled when data arrives (to wait for it with others).
    pub fn bit(&self) -> usize {
        self.tcp.bit()
    }

    pub async fn send_text(&mut self, text: &str) -> Result<(), NetError> {
        self.send_frame(opcode::TEXT, text.as_bytes()).await
    }

    /// Non-blocking: read what's available and return a complete text message if one arrived.
    pub async fn poll_text(&mut self) -> Result<Option<String>, NetError> {
        while let Some(n) = self.tcp.try_read(&mut self.chunk).await? {
            if n == 0 {
                break;
            }
            self.reader.push(&self.chunk[..n]);
        }
        loop {
            match self.reader.next() {
                Ok(Some(Event::Text(t))) => return Ok(Some(t)),
                Ok(Some(Event::Ping(p))) => self.send_frame(opcode::PONG, &p).await?,
                Ok(Some(Event::Close)) => return Err(NetError::Closed),
                Ok(None) => return Ok(None),
                Err(ws::TooLong) => {
                    ::log::warn!("ha: a message over {} MiB; reconnecting", MAX_MESSAGE >> 20);
                    return Err(NetError::Closed);
                }
            }
        }
    }

    /// Wait for the next text message (with timeout).
    pub async fn recv_text(&mut self, timeout_ms: u64) -> Result<String, NetError> {
        let end = time::now_ms() + timeout_ms;
        loop {
            if let Some(m) = self.poll_text().await? {
                return Ok(m);
            }
            if time::now_ms() > end {
                return Err(NetError::Timeout);
            }
            exec::sleep_ms(5).await;
        }
    }
}
