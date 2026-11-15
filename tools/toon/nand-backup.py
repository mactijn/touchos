#!/usr/bin/env python3
"""Back up the Toon's whole NAND over JTAG, verified, without writing to it.

Runs on the Pi, with toonconsole.py running and the Toon at a U-Boot prompt
(stock or ToonRooter's RAM build). Per 16 MiB chunk of *physical* NAND:

1. U-Boot `nand read`s the chunk's good blocks into RAM at BUF (it skips bad
   blocks by itself; the size is chosen so it stops exactly at the chunk end);
2. U-Boot `crc32`s that buffer;
3. OpenOCD halts the CPU, `dump_image`s the buffer, and resumes;
4. the dump's CRC32 must equal U-Boot's.

The result is a physical image (bad blocks filled with 0xFF), the list of bad
blocks, and checksums:

    python3 nand-backup.py [OUTDIR]          (default ~/toon-backup/nand)

Only `nand bad`, `nand read` and `crc32` are sent to U-Boot.
"""
import hashlib, os, re, subprocess, sys, time, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/toon-backup/nand")
NAND_SIZE = 128 << 20
BLOCK = 128 << 10
CHUNK = 16 << 20
BUF = 0xA4000000  # above U-Boot (0xa1f00000) and its framebuffer (0xa1400000)


def uboot(cmd, timeout=60):
    r = subprocess.run([os.path.join(HERE, "toon-cmd"), "-t", str(timeout), cmd],
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"no U-Boot prompt after {cmd!r}:\n{r.stdout}{r.stderr}")
    return r.stdout


def log(msg):
    print(time.strftime("%T"), msg, flush=True)


os.makedirs(OUT, exist_ok=True)

bad = sorted(int(x, 16) for x in re.findall(r"^\S+ +([0-9a-f]{8})$", uboot("nand bad"), re.M))
log(f"bad blocks: {[hex(b) for b in bad]}")
with open(os.path.join(OUT, "bad-blocks.txt"), "w") as f:
    f.write("".join(f"0x{b:08x}\n" for b in bad))

for off in range(0, NAND_SIZE, CHUNK):
    path = os.path.join(OUT, f"chunk-{off:08x}.bin")
    bad_here = [b for b in bad if off <= b < off + CHUNK]
    good = CHUNK - BLOCK * len(bad_here)
    if os.path.exists(path) and os.path.exists(path + ".crc"):
        log(f"chunk 0x{off:08x}: already done, skipped")
        continue

    out = uboot(f"nand read {BUF:x} {off:x} {good:x}", timeout=120)
    if "bytes read: OK" not in out:
        sys.exit(f"nand read failed at 0x{off:x}:\n{out}")
    m = re.search(r"==> ([0-9a-f]{8})", uboot(f"crc32 {BUF:x} {good:x}"))
    if not m:
        sys.exit(f"no crc32 result at 0x{off:x}")
    want = int(m.group(1), 16)

    t0 = time.monotonic()
    env = dict(os.environ, TOON_OCD_TIMEOUT="900")
    ocd = subprocess.run([os.path.join(HERE, "toon-ocd"),
                          f"halt; dump_image {path}.part 0x{BUF:x} 0x{good:x}"],
                         capture_output=True, text=True, env=env)
    if "toon-ocd: resumed" not in ocd.stdout or "ERROR" in ocd.stdout:
        sys.exit(f"JTAG dump failed at 0x{off:x}:\n{ocd.stdout}")
    data = open(path + ".part", "rb").read()
    got = zlib.crc32(data)
    if len(data) != good or got != want:
        sys.exit(f"chunk 0x{off:x}: CRC mismatch (U-Boot {want:08x}, JTAG {got:08x}, {len(data)} bytes)")
    os.replace(path + ".part", path)
    with open(path + ".crc", "w") as f:
        f.write(f"{want:08x}\n")
    log(f"chunk 0x{off:08x}: {good} bytes, crc32 {want:08x} OK, "
        f"bad {[hex(b) for b in bad_here]}, {time.monotonic() - t0:.0f} s over JTAG")

# Physical image: good blocks in order, 0xFF for the bad ones.
img = os.path.join(OUT, "nand-physical.bin")
with open(img, "wb") as o:
    for off in range(0, NAND_SIZE, CHUNK):
        data = open(os.path.join(OUT, f"chunk-{off:08x}.bin"), "rb").read()
        pos = 0
        for blk in range(off, off + CHUNK, BLOCK):
            if blk in bad:
                o.write(b"\xff" * BLOCK)
            else:
                o.write(data[pos:pos + BLOCK])
                pos += BLOCK
        assert pos == len(data)
sha = hashlib.sha256(open(img, "rb").read()).hexdigest()
with open(os.path.join(OUT, "SHA256SUMS"), "w") as f:
    f.write(f"{sha}  nand-physical.bin\n")
log(f"done: {img} ({os.path.getsize(img)} bytes), sha256 {sha}")
