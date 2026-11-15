//! `cargo xtask pair [URL]`: pair with a panel without looking at its
//! screen. The panel must have `debug.log_pin = true` and send its log here
//! (`debug.log_host` = this machine, UDP port 5555 unless `--port`).
//!
//! It asks the panel for a PIN, catches the PIN in the UDP log, pairs, and
//! writes the session to build/panel.cookies (a curl cookie jar:
//! `curl -b build/panel.cookies …`). It also prints the cookie, for test
//! browsers.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::root;

/// How long the PIN may take to arrive in the log.
const PIN_WAIT: Duration = Duration::from_secs(20);

pub fn pair(url: &str, log_port: u16) -> Result<()> {
    let hostport = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if hostport.contains('/') || url.starts_with("https://") {
        bail!("expected a URL like http://192.168.1.42 or http://localhost:8080");
    }
    let host = hostport
        .rsplit_once(':')
        .map_or(hostport.as_str(), |(h, _)| h);

    // Listen before asking, so the PIN line can't be missed.
    let sock = UdpSocket::bind(("0.0.0.0", log_port)).with_context(|| {
        format!(
            "UDP port {log_port} is in use, probably by `cargo xtask log` \
             (pgrep -af \"[x]task log\"); stop it and try again"
        )
    })?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;

    let r = http(&hostport, "POST", "/api/pair/start", None)?;
    if r.status != 200 {
        bail!("the panel didn't start pairing ({}): {}", r.status, r.body);
    }
    println!("the panel shows a PIN; waiting for it in the log (UDP port {log_port})...");
    let pin = wait_for_pin(&sock)?;
    println!("PIN {pin}");

    let r = http(
        &hostport,
        "POST",
        "/api/pair",
        Some(&format!("{{\"pin\":\"{pin}\"}}")),
    )?;
    if r.status != 200 {
        bail!("pairing failed ({}): {}", r.status, r.body);
    }
    let token = r
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie"))
        .find_map(|(_, v)| v.split(';').next()?.trim().strip_prefix("touchos_sid="))
        .context("the panel paired but sent no session cookie")?
        .to_string();

    let jar = root().join("build/panel.cookies");
    std::fs::create_dir_all(jar.parent().unwrap())?;
    // Netscape cookie-jar format, as curl reads it.
    std::fs::write(
        &jar,
        format!("# touchos session for {url}\n{host}\tFALSE\t/\tFALSE\t0\ttouchos_sid\t{token}\n"),
    )?;
    println!("paired: touchos_sid={token}");
    println!("cookie jar: {} (curl -b)", jar.display());
    Ok(())
}

/// The first "pairing: PIN 123456" line from the log.
fn wait_for_pin(sock: &UdpSocket) -> Result<String> {
    let start = Instant::now();
    let mut buf = [0u8; 2048];
    while start.elapsed() < PIN_WAIT {
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        if let Some(pin) = find_pin(&text) {
            return Ok(pin);
        }
    }
    bail!(
        "no PIN in the log within {} s: is debug.log_pin on, and does debug.log_host point at this machine?",
        PIN_WAIT.as_secs()
    )
}

fn find_pin(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let rest = &l[l.find("pairing: PIN ")? + "pairing: PIN ".len()..];
        let pin: String = rest.chars().take(6).collect();
        (pin.len() == 6 && pin.chars().all(|c| c.is_ascii_digit())).then_some(pin)
    })
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

/// One HTTP/1.1 request to the panel (it closes the connection after each).
fn http(hostport: &str, method: &str, path: &str, json: Option<&str>) -> Result<Reply> {
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let mut s = TcpStream::connect(&addr).with_context(|| format!("connecting to {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = json.unwrap_or("");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nX-Touchos: 1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if json.is_some() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .context("malformed HTTP reply")?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .context("malformed HTTP status line")?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::find_pin;

    #[test]
    fn finds_the_pin_line_only() {
        assert_eq!(
            find_pin("[ 1.0] I web: pairing: PIN shown on the panel"),
            None
        );
        assert_eq!(
            find_pin("[ 21.3] I web: pairing: PIN 740428 (debug.log_pin)").as_deref(),
            Some("740428")
        );
    }
}
