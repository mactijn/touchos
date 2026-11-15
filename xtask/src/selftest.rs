//! `cargo xtask selftest [--secs N]`: boot QEMU imx25-pdk with the kernel
//! argument `selftest` and judge the result.
//!
//! init starts the test programs (servers/hello, servers/selftest and its
//! own checks) and logs "selftest: finished" when the last one has ended.
//! The run passes when that line comes, at least one PASS line came, and
//! no FAIL line or kernel panic did. The whole console goes to
//! build/selftest-imx25.log.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{arm, root};

const END: &str = "selftest: finished";

pub fn main(secs: u64) -> Result<()> {
    let a = arm::build(arm::Board::Imx25, false, "selftest")?;
    run(arm::qemu_command(&a), "imx25", secs)
}

fn run(mut cmd: Command, board: &str, secs: u64) -> Result<()> {
    let log_path = root().join(format!("build/selftest-{board}.log"));
    let mut log = std::fs::File::create(&log_path).with_context(|| log_path.display().to_string())?;
    println!("selftest on {board}: up to {secs} s; the console goes to {}", log_path.display());
    let mut child = cmd
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .context("starting QEMU")?;
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).split(b'\n') {
            let Ok(line) = line else { break };
            if tx.send(String::from_utf8_lossy(&line).into_owned()).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(secs);
    let (mut pass, mut fails, mut finished) = (0usize, Vec::new(), false);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = match rx.recv_timeout(left) {
            Ok(l) => l,
            Err(_) => break, // the deadline, or QEMU ended
        };
        writeln!(log, "{line}")?;
        if line.contains("PASS:") {
            pass += 1;
            println!("{line}");
        } else if line.contains("FAIL:") || line.contains("KERNEL PANIC") {
            fails.push(line.clone());
            println!("{line}");
        }
        if line.contains(END) {
            finished = true;
            break;
        }
    }
    // QEMU that ended by itself (it couldn't start, or the machine
    // stopped) is the reason, not the deadline.
    let exited = child.try_wait().ok().flatten();
    let _ = child.kill();
    let _ = child.wait();

    println!("selftest on {board}: {pass} PASS, {} FAIL", fails.len());
    if !fails.is_empty() {
        bail!("selftest on {board} failed:\n{}", fails.join("\n"));
    }
    if let (false, Some(status)) = (finished, exited) {
        bail!("selftest on {board}: QEMU ended ({status}) before \"{END}\" (see {})", log_path.display());
    }
    if !finished {
        bail!("selftest on {board}: no \"{END}\" within {secs} s (see {})", log_path.display());
    }
    if pass == 0 {
        bail!("selftest on {board}: no PASS lines (see {})", log_path.display());
    }
    Ok(())
}
