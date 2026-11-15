//! Just enough HTTP/1.1 for the panel's settings page: parse one request
//! (with limits), build one response. Every connection serves a single
//! request (`Connection: close`). `ws` is the WebSocket frame layer of a
//! client (the Home Assistant connection).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod ws;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MAX_HEAD: usize = 8 * 1024;
pub const MAX_BODY: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// A cookie's value.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == name).then_some(v)
        })
    }

    /// Whether the browser used one of our names in `Host` (anything else
    /// is a DNS-rebinding attempt): `our_ip`, or localhost (QEMU's port
    /// forwarding). A port after the name doesn't matter.
    pub fn host_ok(&self, our_ip: Option<[u8; 4]>) -> bool {
        let Some(host) = self.header("host") else {
            return false;
        };
        let name = host
            .rsplit_once(':')
            .map_or(host, |(h, p)| if p.bytes().all(|b| b.is_ascii_digit()) { h } else { host });
        if name == "localhost" || name == "127.0.0.1" {
            return true;
        }
        our_ip.is_some_and(|ip| name == format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]))
    }

    /// Whether a change comes from our own page: the custom `X-Touchos:
    /// 1` header (a form or a simple cross-site request can't set it) and,
    /// when the browser says where it comes from, the same origin.
    pub fn from_our_page(&self) -> bool {
        let same_origin = match (self.header("origin"), self.header("host")) {
            (Some(o), Some(h)) => o == format!("http://{h}"),
            (Some(_), None) => false,
            (None, _) => true,
        };
        self.header("x-touchos") == Some("1") && same_origin
    }

    /// Whether the browser takes a gzip-compressed answer.
    pub fn accepts_gzip(&self) -> bool {
        self.header("accept-encoding")
            .is_some_and(|e| e.split(',').any(|c| c.split(';').next().is_some_and(|n| n.trim() == "gzip")))
    }

    /// A query parameter (`?n=200`), not URL-decoded.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == name).then_some(v)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Need more bytes.
    Partial,
    /// 400
    Bad,
    /// 431: the head is too large.
    HeadTooLarge,
    /// 413: the body is too large.
    BodyTooLarge,
}

impl Error {
    pub fn status(self) -> u16 {
        match self {
            Error::Partial | Error::Bad => 400,
            Error::HeadTooLarge => 431,
            Error::BodyTooLarge => 413,
        }
    }
}

/// Parse a request from the bytes received so far.
pub fn parse(buf: &[u8]) -> Result<Request, Error> {
    let (mut req, head_len, len) = parse_head(buf)?;
    if len > MAX_BODY {
        return Err(Error::BodyTooLarge);
    }
    if buf.len() < head_len + len {
        return Err(Error::Partial);
    }
    req.body = buf[head_len..head_len + len].to_vec();
    Ok(req)
}

/// Parse just the head: the request without its body, the head's length,
/// and the body's length (Content-Length, not checked against
/// [`MAX_BODY`]: for uploads that are streamed elsewhere).
pub fn parse_head(buf: &[u8]) -> Result<(Request, usize, usize), Error> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    let head_len = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) if buf.len() >= MAX_HEAD => return Err(Error::HeadTooLarge),
        Ok(httparse::Status::Partial) => return Err(Error::Partial),
        Err(httparse::Error::TooManyHeaders) => return Err(Error::HeadTooLarge),
        Err(_) => return Err(Error::Bad),
    };
    if head_len > MAX_HEAD {
        return Err(Error::HeadTooLarge);
    }
    let target = req.path.ok_or(Error::Bad)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_ascii_lowercase(),
                String::from_utf8_lossy(h.value).trim().to_string(),
            )
        })
        .collect();
    let len = match headers.iter().find(|(n, _)| n == "content-length") {
        Some((_, v)) => v.parse::<usize>().map_err(|_| Error::Bad)?,
        None => 0,
    };
    if headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Err(Error::Bad); // no chunked bodies
    }
    Ok((
        Request {
            method: req.method.ok_or(Error::Bad)?.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers,
            body: Vec::new(),
        },
        head_len,
        len,
    ))
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "",
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            content_type,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response::new(status, "application/json", body)
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// The bytes to send.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            reason(self.status),
            self.content_type,
            self.body.len()
        )
        .into_bytes();
        for (n, v) in &self.headers {
            out.extend_from_slice(n.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

/// Escape a string for a JSON document.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_with_body() {
        let raw = b"PATCH /api/config?x=1&n=20 HTTP/1.1\r\nHost: 10.0.0.5\r\nCookie: a=b; touchos_sid=abc\r\nContent-Length: 4\r\n\r\n{\"a\"";
        assert_eq!(parse(&raw[..raw.len() - 1]), Err(Error::Partial));
        let r = parse(raw).unwrap();
        assert_eq!(r.method, "PATCH");
        assert_eq!(r.path, "/api/config");
        assert_eq!(r.param("n"), Some("20"));
        assert_eq!(r.header("host"), Some("10.0.0.5"));
        assert_eq!(r.cookie("touchos_sid"), Some("abc"));
        assert_eq!(r.body, b"{\"a\"");
    }

    #[test]
    fn limits() {
        let big = alloc::format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(parse(big.as_bytes()), Err(Error::BodyTooLarge));
        let head = alloc::format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(MAX_HEAD));
        assert_eq!(parse(head.as_bytes()), Err(Error::HeadTooLarge));
        assert_eq!(parse(b"GARBAGE\r\n\r\n"), Err(Error::Bad));
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(Error::Bad)
        );
    }

    #[test]
    fn response_bytes() {
        let r = Response::json(200, "{}").header("X-Frame-Options", "DENY");
        let s = String::from_utf8(r.to_bytes()).unwrap();
        assert!(s.starts_with(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n"
        ));
        assert!(s.ends_with("X-Frame-Options: DENY\r\n\r\n{}"));
    }

    #[test]
    fn json_escape() {
        assert_eq!(json_str("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }

    fn with_headers(h: &[(&str, &str)]) -> Request {
        Request {
            method: "POST".into(),
            path: "/api/config".into(),
            query: String::new(),
            headers: h.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn host_checks() {
        let ip = Some([192, 0, 2, 10]);
        assert!(with_headers(&[("host", "192.0.2.10")]).host_ok(ip));
        assert!(with_headers(&[("host", "192.0.2.10:80")]).host_ok(ip));
        assert!(with_headers(&[("host", "localhost:8080")]).host_ok(None));
        assert!(!with_headers(&[("host", "evil.example")]).host_ok(ip)); // rebinding
        assert!(!with_headers(&[("host", "192.0.2.1")]).host_ok(ip));
        assert!(!with_headers(&[("host", "192.0.2.10")]).host_ok(None)); // no address yet
        assert!(!with_headers(&[]).host_ok(ip));
    }

    #[test]
    fn our_page_checks() {
        let h = ("host", "192.0.2.10");
        assert!(with_headers(&[h, ("x-touchos", "1")]).from_our_page());
        assert!(with_headers(&[h, ("x-touchos", "1"), ("origin", "http://192.0.2.10")]).from_our_page());
        assert!(!with_headers(&[h, ("x-touchos", "1"), ("origin", "http://evil.example")]).from_our_page());
        assert!(!with_headers(&[h, ("origin", "http://192.0.2.10")]).from_our_page()); // no header
        assert!(!with_headers(&[("x-touchos", "1"), ("origin", "http://192.0.2.10")]).from_our_page());
    }

    #[test]
    fn gzip_negotiation() {
        assert!(with_headers(&[("accept-encoding", "gzip, deflate, br")]).accepts_gzip());
        assert!(with_headers(&[("accept-encoding", "br;q=1.0, gzip;q=0.8")]).accepts_gzip());
        assert!(!with_headers(&[("accept-encoding", "x-gzip-ish, br")]).accepts_gzip());
        assert!(!with_headers(&[]).accepts_gzip());
    }
}
