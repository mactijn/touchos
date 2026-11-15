#!/usr/bin/env python3
"""Check that the Toon's NAND still matches the backup, read-only.

Run on the Pi, with U-Boot at its prompt (toonconsole.py's autostop):

    ./nand-verify.py [BACKUP_DIR]      # default ~/toon-backup/nand

Per 16 MiB chunk, U-Boot `nand read`s the good blocks into RAM and
`crc32`s them, exactly as nand-backup.py did; the CRC must equal the one
stored with the backup (chunk-*.bin.crc). The bad-block list must be the
same too. Only `nand bad`, `nand read` and `crc32` are sent to U-Boot.
Takes about 30 s (no JTAG transfer).
"""
import os, re, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
DIR = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/toon-backup/nand")
NAND_SIZE = 128 << 20
BLOCK = 128 << 10
CHUNK = 16 << 20
BUF = 0xA4000000  # as nand-backup.py: above U-Boot and its framebuffer


def uboot(cmd, timeout=60):
    r = subprocess.run([os.path.join(HERE, "toon-cmd"), "-t", str(timeout), cmd],
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"no U-Boot prompt after {cmd!r}:\n{r.stdout}{r.stderr}")
    return r.stdout


bad = sorted(int(x, 16) for x in re.findall(r"^\S+ +([0-9a-f]{8})$", uboot("nand bad"), re.M))
saved = sorted(int(x, 16) for x in open(os.path.join(DIR, "bad-blocks.txt")).read().split())
ok = bad == saved
print(f"bad blocks: {[hex(b) for b in bad]}" + ("" if ok else f" (backup: {[hex(b) for b in saved]})"))

for off in range(0, NAND_SIZE, CHUNK):
    good = CHUNK - BLOCK * sum(1 for b in bad if off <= b < off + CHUNK)
    t0 = time.monotonic()
    out = uboot(f"nand read {BUF:x} {off:x} {good:x}", timeout=120)
    if "bytes read: OK" not in out:
        sys.exit(f"nand read failed at 0x{off:x}:\n{out}")
    m = re.search(r"==> ([0-9a-f]{8})", uboot(f"crc32 {BUF:x} {good:x}"))
    if not m:
        sys.exit(f"no crc32 result at 0x{off:x}")
    want = open(os.path.join(DIR, f"chunk-{off:08x}.bin.crc")).read().strip()
    same = m.group(1) == want
    ok &= same
    print(f"chunk 0x{off:08x}: crc32 {m.group(1)} {'same' if same else 'DIFFERS from ' + want}"
          f" ({time.monotonic() - t0:.0f} s)", flush=True)

print("NAND matches the backup" if ok else "NAND DIFFERS from the backup")
sys.exit(0 if ok else 1)
