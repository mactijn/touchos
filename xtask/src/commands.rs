//! The commands, with the flags and options each takes and its usage: one
//! table that checks the command line (an unknown or misspelt flag is an
//! error, not silently ignored) and prints the help.

use anyhow::{Result, bail};

pub struct Cmd {
    pub name: &'static str,
    /// Flags without a value.
    pub flags: &'static [&'static str],
    /// Options that take the next word as their value.
    pub opts: &'static [&'static str],
    /// The usage lines (the first one is the synopsis).
    pub help: &'static str,
}

const R: &str = "--release";

pub const COMMANDS: &[Cmd] = &[
    Cmd {
        name: "build",
        flags: &[R],
        opts: &["--board", "--cmdline"],
        help: "build [--board toon|imx25] [--release] [--cmdline \"ARGS\"]\n    the kernel, the initrd, the config and the boot info in build/arm-<board>/ (default: toon)",
    },
    Cmd {
        name: "qemu",
        flags: &[R],
        opts: &["--cmdline", "--secs"],
        help: "qemu [--release] [--cmdline \"ARGS\"] [--secs N]\n    boot in QEMU -M imx25-pdk: the console on stdout, the settings page on localhost:8080",
    },
    Cmd {
        name: "load",
        flags: &[R],
        opts: &["--cmdline", "--secs"],
        help: "load [--release] [--cmdline \"ARGS\"] [--secs N]\n    load into the Toon's RAM over JTAG ($TOON_HOST) and show its console",
    },
    Cmd {
        name: "flash",
        flags: &[R, "--config"],
        opts: &["--slot", "--cmdline", "--secs"],
        help: "flash [--slot A|B] [--config] [--release] [--cmdline \"ARGS\"]\n    write touchos into the Toon's NAND over JTAG ($TOON_HOST); --config: also config/touchos.toml",
    },
    Cmd { name: "nandboot", flags: &[], opts: &["--secs"], help: "nandboot [--secs N]\n    let the Toon boot from its NAND by itself, and show its console" },
    Cmd {
        name: "bundle",
        flags: &[R],
        opts: &["--cmdline"],
        help: "bundle [--release] [--cmdline \"ARGS\"]\n    build/touchos-<build>.update, a signed update for the settings page",
    },
    Cmd { name: "keygen", flags: &[], opts: &[], help: "keygen\n    make the signing key for updates (once)" },
    Cmd { name: "check", flags: &["--quick"], opts: &[], help: "check [--quick]\n    host tests, both boards' builds, the settings page's tests" },
    Cmd { name: "selftest", flags: &[], opts: &["--secs"], help: "selftest [--secs N]\n    boot QEMU imx25 with `selftest`; fails on a FAIL line or a kernel panic" },
    Cmd { name: "log", flags: &[], opts: &[], help: "log [PORT]\n    print the kernel log sent over UDP (debug.log_host; port 5555)" },
    Cmd { name: "pair", flags: &[], opts: &["--port"], help: "pair [URL] [--port N]\n    pair without the screen (PIN from the UDP log; needs debug.log_pin)" },
    Cmd {
        name: "cacerts",
        flags: &["--check"],
        opts: &[],
        help: "cacerts [PEM] [--check]\n    libs/certs/cacerts.der, the roots TLS trusts, from a CA bundle (default /etc/ssl/certs/ca-certificates.crt); --check: only compare",
    },
    Cmd {
        name: "tzdata",
        flags: &["--check"],
        opts: &[],
        help: "tzdata [DIR] [--check]\n    libs/civil/src/zones.rs, every time zone's rule, from compiled tzdata (default /usr/share/zoneinfo); --check: only compare",
    },
    Cmd { name: "webui", flags: &[], opts: &[], help: "webui [serve [URL]]\n    build the settings page; serve: develop it at http://localhost:4200, /api forwarded to a panel" },
];

/// Whether the flag `name` is on the command line (`commands::check` has
/// made sure it is one the command takes).
pub fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// The value after the option `name`.
pub fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// The usage text.
pub fn usage() -> String {
    let mut s = String::from("usage: cargo xtask <command> [options]\n");
    for c in COMMANDS {
        for l in c.help.lines() {
            s += "  ";
            s += l;
            s.push('\n');
        }
    }
    s
}

/// Check `args` (the command first) against the table: the command is
/// known and every `--flag` is one it takes.
pub fn check(args: &[String]) -> Result<&'static Cmd> {
    let Some(name) = args.first() else { bail!("{}", usage()) };
    let Some(cmd) = COMMANDS.iter().find(|c| c.name == name) else {
        bail!("unknown command {name:?}\n{}", usage())
    };
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if cmd.opts.contains(&a) {
            if i + 1 >= args.len() {
                bail!("{a} needs a value\n  {}", cmd.help.lines().next().unwrap());
            }
            i += 1;
        } else if a.starts_with("--") && !cmd.flags.contains(&a) {
            bail!("`{name}` takes no {a}\n  {}", cmd.help.lines().next().unwrap());
        }
        i += 1;
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_are_checked() {
        assert!(check(&v(&["qemu", "--release", "--cmdline", "--secs"])).is_ok()); // a value may look like a flag
        assert!(check(&v(&["flash", "--sloot", "B"])).is_err()); // misspelt
        assert!(check(&v(&["bundle", "--config"])).is_err()); // a flash flag
        assert!(check(&v(&["qemu", "--cmdline"])).is_err()); // no value
        assert!(check(&v(&["frobnicate"])).is_err());
        assert!(check(&v(&["log", "5556"])).is_ok());
    }
}
