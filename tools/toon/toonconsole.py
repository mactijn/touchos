#!/usr/bin/env python3
"""Console service for the Toon's UART0 (JTAG header pins 11/13), run on the Pi.

The one process that owns the serial port:
- logs everything the Toon sends, one timestamped line at a time, to LOG;
- sends whatever is written to the FIFO `console.in` to the Toon;
- stops U-Boot's autoboot by itself: while the file `autostop` exists, it
  types a password whenever "Enter password" appears. With `auto` in the
  file, the password is picked by the U-Boot version seen last (stock R6/R8,
  or ToonRooter's RAM build); anything else in the file is used literally.
  A missing or empty file lets the Toon boot normally;
- guards against Linux starting: while the file `guard` exists, "Starting
  kernel" makes it halt the CPU over JTAG (`toon-ocd -k halt`), long before
  the stock system can bring up Wi-Fi. If the file holds a text, that text is
  the trigger instead (e.g. "INIT: version" lets a kernel with init=/bin/sh
  run, but stops the stock init scripts).

    python3 toonconsole.py [--port /dev/ttyS0] [--log ~/toon-backup/serial.log]

Partial lines (prompts such as "U-Boot> ") are logged after 0.5 s of quiet,
followed by " [no newline]". Use `toon-cmd` to send one command and wait for
the prompt.
"""
import argparse, os, select, subprocess, time
import serial

here = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--port", default="/dev/ttyS0")
ap.add_argument("--log", default=os.path.expanduser("~/toon-backup/serial.log"))
ap.add_argument("--fifo", default=os.path.join(here, "console.in"))
ap.add_argument("--autostop", default=os.path.join(here, "autostop"))
ap.add_argument("--guard", default=os.path.join(here, "guard"))
args = ap.parse_args()

os.makedirs(os.path.dirname(args.log), exist_ok=True)
if not os.path.exists(args.fifo):
    os.mkfifo(args.fifo, 0o600)
fifo = os.open(args.fifo, os.O_RDWR | os.O_NONBLOCK)  # RDWR: never sees EOF

ser = serial.Serial(args.port, 115200, timeout=0)
out = open(args.log, "ab", buffering=0)


def log(line: bytes):
    out.write(time.strftime("%T ").encode() + line + b"\n")


# Known U-Boot passwords (from ToonRooter), by the version line's prefix.
PASSWORDS = [
    (b"U-Boot 2010.09-R8 ", "3BHf2"),
    (b"U-Boot 2010.09-R6 ", "f4E9J"),
    (b"U-Boot 2010.09 (", "toon"),  # ToonRooter's RAM build
]
uboot_version = b""


def autostop_password():
    try:
        with open(args.autostop) as f:
            pw = f.read().strip()
    except FileNotFoundError:
        return ""
    if pw != "auto":
        return pw
    for prefix, known in PASSWORDS:
        if uboot_version.startswith(prefix):
            return known
    return ""


def guard_trigger():
    try:
        with open(args.guard, "rb") as f:
            return f.read().strip() or b"Starting kernel"
    except FileNotFoundError:
        return None


def kernel_guard(trigger):
    log(b">>> (guard: saw '" + trigger + b"', halting the CPU over JTAG)")
    subprocess.Popen([os.path.join(here, "toon-ocd"), "-k", "halt"],
                     stdout=open(os.path.join(here, "guard.log"), "ab"),
                     stderr=subprocess.STDOUT, start_new_session=True)


log(f"=== toonconsole start {time.strftime('%F %T')} on {args.port}".encode())
buf = b""
window = b""  # recent bytes, to spot the password prompt across reads
last_rx = time.monotonic()
while True:
    ready, _, _ = select.select([ser.fileno(), fifo], [], [], 0.5)
    if fifo in ready:
        data = os.read(fifo, 4096)
        if data:
            ser.write(data)
            log(b">>> " + data.rstrip(b"\n").replace(b"\n", b"\\n"))
    if ser.fileno() in ready:
        data = ser.read(4096)
        if data:
            last_rx = time.monotonic()
            buf += data
            # Search all of this read plus the tail of the last one: a read
            # can return hundreds of bytes when this loop runs late, and a
            # trigger in it must not fall off a short window.
            seen = window + data
            window = seen[-64:]
            # stock: "Enter password - autoboot in 2 sec"; RAM build: "Enter the password"
            if b"Enter password" in seen or b"Enter the password" in seen:
                window = b""
                pw = autostop_password()
                if pw:
                    time.sleep(0.05)
                    ser.write(pw.encode())
                    log(b">>> (autostop: password sent)")
                else:
                    log(b">>> (autostop: off or no known password, not stopping)")
            trigger = guard_trigger()
            if trigger and trigger in seen:
                window = b""
                kernel_guard(trigger)
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                line = line.replace(b"\r", b"")
                if line.startswith(b"U-Boot 20"):
                    uboot_version = line
                log(line)
    if buf and time.monotonic() - last_rx > 0.5:
        log(buf.replace(b"\r", b"") + b" [no newline]")
        buf = b""
