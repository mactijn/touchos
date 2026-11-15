//! touchos's boot stage on the Toon's NAND, and the tool that writes it.
//!
//! **Boot** (entry +0): U-Boot's stock `boot_nand` reads the old `kernel`
//! partition (3 MiB of good blocks from 0x300000) and `bootm`s it: that
//! is this program, as a uImage. It reads touchos's area (libs/nand's
//! layout): the newest state record names the active system copy, whose
//! kernel, initrd and boot info go where the JTAG loader puts them
//! (libs/abi's imx_boot), with the newest config record as touchos.toml;
//! then it starts the kernel. A copy that fails its CRCs makes it try the
//! other one.
//!
//! **Flash** (entry +4, from JTAG, `cargo xtask flash`): a payload at
//! 0xa2000000 (libs/nand's `FlashPayload`): the boot image, a system copy
//! and a config. Each is written, read back and compared; the state
//! record that makes the copy active comes last.
//!
//! **Probe** (entry +8, from JTAG): read-only: the chip's ID, the bad
//! blocks, and CRCs of a few blocks to compare with the backup.
//!
//! Boots with the MMU and caches on (an identity map, RAM cached): the
//! stage's part of a boot went from 10 s to 3 s; they are off
//! again before the kernel starts, as U-Boot's `bootm` leaves them. Flash
//! and probe run with the MMU off (under JTAG U-Boot is halted at its
//! prompt). On UART1 as U-Boot set it up.
//! Never touches blocks 0-23 (U-Boot, its env, the splash).
#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

use touchos_nand::image::{self, Config, ConfigKind, FlashPayload, State, SysHeader};
use touchos_nand::layout::{self, BadBlocks, Region};
use touchos_nand::nfc::{self, BLOCK, Nfc, PAGE, PAGES_PER_BLOCK};
use touchos_nand::system::{self, Reason};

global_asm!(
    r#"
    .section .text.start, "ax"
    .global _start
    .arm
_start:
    b 1f
    b 2f
    b 3f
1:  mov r4, #0
    b 4f
2:  mov r4, #1
    b 4f
3:  mov r4, #2
4:  ldr sp, =0xa7f00000
    @ The I-cache on (the MMU stays off): copying 8 MiB goes faster.
    mrc p15, 0, r0, c1, c0, 0
    orr r0, r0, #0x1000
    mcr p15, 0, r0, c1, c0, 0
    @ Zero .bss (the uImage holds only what is loaded).
    ldr r0, =__bss_start
    ldr r1, =__bss_end
    mov r2, #0
5:  cmp r0, r1
    strlo r2, [r0], #4
    blo 5b
    mov r0, r4
    bl main
6:  b 6b
"#
);

// ---- the console (UART1, set up by U-Boot) ----

const UART1: usize = 0x1000_a000;
const UTXD: usize = 0x40;
const UTS: usize = 0xb4;
const UTS_TXFULL: u32 = 1 << 4;

fn putc(c: u8) {
    unsafe {
        while read_volatile((UART1 + UTS) as *const u32) & UTS_TXFULL != 0 {}
        write_volatile((UART1 + UTXD) as *mut u32, c as u32);
    }
}

fn puts(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            putc(b'\r');
        }
        putc(b);
    }
}

fn hex(v: u32) {
    puts("0x");
    for i in (0..8).rev() {
        putc(b"0123456789abcdef"[((v >> (i * 4)) & 0xf) as usize]);
    }
}

fn dec(mut v: u32) {
    let mut b = [0u8; 10];
    let mut i = b.len();
    loop {
        i -= 1;
        b[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for &c in &b[i..] {
        putc(c);
    }
}

fn err(what: &str, e: nfc::Error) {
    puts("nandboot: ");
    puts(what);
    puts(": ");
    puts(match e {
        nfc::Error::Timeout => "timeout",
        nfc::Error::Ecc => "uncorrectable ECC error",
        nfc::Error::Failed => "the chip reports a failed program/erase",
        nfc::Error::Range => "out of range",
        nfc::Error::Verify => "didn't read back",
    });
    puts("\n");
}

// ---- RAM (the MMU is off: physical addresses) ----

/// libs/abi's imx_boot layout on the Toon (RAM at 0xa0000000).
const KERNEL: usize = 0xa000_8000;
const BOOTINFO: usize = 0xa100_0000;
const CONFIG_RAM: usize = 0xa101_0000;
const INITRD: usize = 0xa200_0000;
/// The flash payload, loaded over JTAG.
const PAYLOAD: usize = 0xa200_0000;
/// Machine number and ATAGs as U-Boot passes them (the kernel ignores them).
const MACH_ED20: u32 = 0xbf8;
const ATAGS: u32 = 0xa000_0100;

fn ram(addr: usize, len: usize) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) }
}

// ---- NAND access by region (bad blocks skipped, as U-Boot does) ----

/// The bad blocks from `first` on (U-Boot's blocks are never even read).
fn scan(nfc: &Nfc, first: u32) -> Result<BadBlocks, nfc::Error> {
    let mut bad = BadBlocks::default();
    for b in first..nfc::BLOCKS {
        if nfc.is_bad(b)? {
            bad.mark(b);
        }
    }
    Ok(bad)
}

/// Read `out.len()` bytes from byte `offset` of `region`.
fn read(nfc: &Nfc, bad: &BadBlocks, region: Region, offset: usize, out: &mut [u8]) -> Result<(), nfc::Error> {
    system::read(&mut { nfc }, bad, region, offset, out)
}

/// Erase the blocks `data` needs in `region` (from its start) and write it.
fn write(nfc: &Nfc, bad: &BadBlocks, region: Region, data: &[u8]) -> Result<(), nfc::Error> {
    system::write(&mut { nfc }, bad, region, data)
}

/// Read `data` back from the start of `region` and compare.
fn verify(nfc: &Nfc, bad: &BadBlocks, region: Region, data: &[u8]) -> Result<bool, nfc::Error> {
    system::verify(&mut { nfc }, bad, region, 0, data)
}

/// The first page of `region`'s first good block.
fn first_page(bad: &BadBlocks, region: Region) -> Option<u32> {
    system::first_page(bad, region)
}

/// The newest record of each ring.
fn newest_state(nfc: &Nfc, bad: &BadBlocks) -> Option<(State, u32)> {
    system::newest_state(&mut { nfc }, bad)
}

fn newest_config(nfc: &Nfc, bad: &BadBlocks) -> Option<(Config, u32)> {
    let mut page = [0u8; PAGE];
    let mut best: Option<(Config, u32)> = None;
    for b in layout::CONFIG.good(bad) {
        if nfc.read_page(b * PAGES_PER_BLOCK, &mut page, None).is_err() {
            continue;
        }
        if let Some(c) = Config::decode(&page)
            && best.is_none_or(|(x, _)| c.seq > x.seq)
        {
            best = Some((c, b));
        }
    }
    best
}

/// Read config record `c` (in `block`) into `out`; false if its CRC fails.
fn read_config(nfc: &Nfc, bad: &BadBlocks, block: u32, c: &Config, out: &mut [u8]) -> Result<bool, nfc::Error> {
    let r = Region { first: block, end: block + 1 };
    let n = c.len as usize;
    read(nfc, bad, r, image::CONFIG_HEADER_LEN, &mut out[..n])?;
    Ok(image::crc32(0, &out[..n]) == c.crc)
}

// ---- boot ----

/// Load system copy `slot` into RAM; its header if all parts check out.
fn load_slot(nfc: &Nfc, bad: &BadBlocks, slot: u8) -> Result<Option<SysHeader>, nfc::Error> {
    let region = layout::slot(slot).ok_or(nfc::Error::Range)?;
    let mut page = [0u8; PAGE];
    nfc.read_page(first_page(bad, region).ok_or(nfc::Error::Range)?, &mut page, None)?;
    let Some(h) = SysHeader::decode(&page) else {
        puts("nandboot: copy ");
        putc(slot);
        puts(": no header\n");
        return Ok(None);
    };
    for (name, part, at, max) in [
        ("kernel", h.kernel, KERNEL, BOOTINFO - KERNEL),
        ("boot info", h.bootinfo, BOOTINFO, CONFIG_RAM - BOOTINFO),
        ("initrd", h.initrd, INITRD, 0xa700_0000 - INITRD),
    ] {
        let len = part.len as usize;
        if len > max {
            puts("nandboot: ");
            puts(name);
            puts(" too big\n");
            return Ok(None);
        }
        let dst = ram(at, len);
        read(nfc, bad, region, part.offset as usize, dst)?;
        if image::crc32(0, dst) != part.crc {
            puts("nandboot: copy ");
            putc(slot);
            puts(": ");
            puts(name);
            puts(" fails its CRC\n");
            return Ok(None);
        }
    }
    Ok(Some(h))
}

/// Say why this copy starts.
fn say_plan(slot: u8, r: &State, why: Reason) {
    puts("nandboot: copy ");
    putc(slot);
    puts(match why {
        Reason::Active => ", the active copy",
        Reason::Trial => ", on trial",
        Reason::TrialFailed => ": the copy on trial wasn't confirmed; back to the active copy",
        Reason::FellBack => ": the active copy wasn't confirmed in its starts; the other one takes over",
        Reason::NoRecord => " (no state record yet)",
    });
    puts(" (start ");
    dec(r.starts as u32);
    puts(")\n");
}

/// `slot=X` at the end of the kernel's command line (in the boot info
/// block, now in RAM): which copy runs. (`build=` is in
/// the copy's command line already.)
fn tell_kernel(slot: u8) {
    const CMDLINE_MAX: usize = 512;
    let extra = [b' ', b's', b'l', b'o', b't', b'=', slot];
    let len_at = ram(BOOTINFO + 16, 4);
    let len = u32::from_le_bytes([len_at[0], len_at[1], len_at[2], len_at[3]]) as usize;
    if len + extra.len() > CMDLINE_MAX {
        puts("nandboot: no room for slot= on the command line\n");
        return;
    }
    ram(BOOTINFO + 24 + len, extra.len()).copy_from_slice(&extra);
    ram(BOOTINFO + 16, 4).copy_from_slice(&((len + extra.len()) as u32).to_le_bytes());
}

/// The MMU's first-level table: 4096 sections of 1 MiB, an identity map.
#[repr(C, align(16384))]
struct Table([u32; 4096]);
static mut TABLE: Table = Table([0; 4096]);

/// Copying and checking 9 MiB with the D-cache off took 8 s (the whole
/// stage 10 s; 3 s with it on): turn the
/// MMU on with an identity map, RAM (0xa0000000-0xa7ffffff) cached and
/// write-back, everything else (the NFC, the UART) uncached; then the
/// D-cache. [`caches_off`] undoes it before the kernel starts.
fn caches_on() {
    unsafe {
        let t = &raw mut TABLE;
        for i in 0..4096u32 {
            let cached = if (0xa00..0xa80).contains(&i) { 0b11 << 2 } else { 0 };
            // Section, AP = 0b11 (read/write), domain 0, bit 4 set (ARMv5).
            (*t).0[i as usize] = i << 20 | 0b11 << 10 | 1 << 4 | cached | 0b10;
        }
        asm!("mcr p15, 0, {0}, c7, c7, 0", in(reg) 0u32); // I and D caches
        asm!("mcr p15, 0, {0}, c8, c7, 0", in(reg) 0u32); // TLBs
        asm!("mcr p15, 0, {0}, c2, c0, 0", in(reg) t as u32);
        asm!("mcr p15, 0, {0}, c3, c0, 0", in(reg) 1u32); // domain 0: client
        let mut c: u32;
        asm!("mrc p15, 0, {0}, c1, c0, 0", out(reg) c);
        c |= 1 | 1 << 2; // MMU, D-cache (the I-cache is on already)
        asm!("mcr p15, 0, {0}, c1, c0, 0", "nop", "nop", in(reg) c);
    }
}

/// Back to how U-Boot's `bootm` hands over: everything the D-cache holds
/// written to RAM, the D-cache and the MMU off.
fn caches_off() {
    unsafe {
        // ARM926 "test, clean and invalidate" until the cache is clean.
        asm!("2: mrc p15, 0, APSR_nzcv, c7, c14, 3", "bne 2b");
        asm!("mcr p15, 0, {0}, c7, c10, 4", in(reg) 0u32); // drain the write buffer
        let mut c: u32;
        asm!("mrc p15, 0, {0}, c1, c0, 0", out(reg) c);
        c &= !(1 | 1 << 2);
        asm!("mcr p15, 0, {0}, c1, c0, 0", "nop", "nop", in(reg) c);
        asm!("mcr p15, 0, {0}, c7, c7, 0", in(reg) 0u32);
        asm!("mcr p15, 0, {0}, c8, c7, 0", in(reg) 0u32);
    }
}

fn boot() -> ! {
    caches_on();
    let nfc = unsafe { Nfc::new(0xd800_0000) };
    puts("\nnandboot: touchos boot stage\n");
    let r = (|| -> Result<(), nfc::Error> {
        nfc.init()?;
        let bad = scan(&nfc, layout::SYSTEM.first)?;
        // Which copy (libs/nand's system::plan): the active one, one on
        // trial, or the other one when the active copy keeps failing.
        let newest = newest_state(&nfc, &bad).map(|(s, _)| s);
        let valid = |slot| system::header(&mut { &nfc }, &bad, slot).is_some();
        let (planned, mut record, why) = system::plan(newest, valid);
        say_plan(planned, &record, why);
        let mut loaded = None;
        for slot in [planned, system::other(planned)] {
            if let Some(h) = load_slot(&nfc, &bad, slot)? {
                loaded = Some((slot, h));
                break;
            }
        }
        let Some((slot, h)) = loaded else {
            puts("nandboot: no system copy loads; halting\n");
            loop {}
        };
        if slot != planned {
            record = system::loaded_instead(record, slot);
        }
        // This start counts (touchos confirms the copy when it runs well).
        if let Err(e) = append_state(&nfc, &bad, record) {
            err("state record", e);
        }
        tell_kernel(slot);
        // touchos.toml: the newest config record (none: the kernel goes
        // without, and the config server uses its defaults).
        let mut cfg_len = 0u32;
        if let Some((c, block)) = newest_config(&nfc, &bad) {
            if read_config(&nfc, &bad, block, &c, ram(CONFIG_RAM, c.len as usize))? {
                cfg_len = c.len;
            } else {
                puts("nandboot: the newest config fails its CRC\n");
            }
        }
        ram(BOOTINFO + 12, 4).copy_from_slice(&cfg_len.to_le_bytes());
        puts("nandboot: copy ");
        putc(slot);
        puts(" (build ");
        puts(h.build_str());
        puts("), config ");
        dec(cfg_len);
        puts(" bytes; starting the kernel\n");
        Ok(())
    })();
    if let Err(e) = r {
        err("boot", e);
        loop {}
    }
    caches_off();
    unsafe {
        // The copied kernel: whatever the I-cache holds is stale.
        asm!("mcr p15, 0, {0}, c7, c5, 0", in(reg) 0u32);
        asm!(
            "bx {entry}",
            entry = in(reg) KERNEL,
            in("r0") 0u32,
            in("r1") MACH_ED20,
            in("r2") ATAGS,
            options(noreturn)
        );
    }
}

// ---- flash ----

fn flash() -> Result<(), nfc::Error> {
    let nfc = unsafe { Nfc::new(0xd800_0000) };
    puts("\nnandboot: flashing\n");
    let Some(p) = FlashPayload::decode(ram(PAYLOAD, image::FLASH_HEADER_LEN)) else {
        puts("nandboot: no flash payload at 0xa2000000\n");
        return Ok(());
    };
    let part = |x: image::Part| ram(PAYLOAD + x.offset as usize, x.len as usize);
    for (name, x) in [("boot image", p.boot), ("system copy", p.system), ("config", p.config)] {
        if image::crc32(0, part(x)) != x.crc {
            puts("nandboot: the payload's ");
            puts(name);
            puts(" fails its CRC; nothing written\n");
            return Ok(());
        }
    }
    nfc.init()?;
    check_id(&nfc)?;
    let bad = scan(&nfc, layout::FIRST_WRITABLE)?;
    print_bad(&bad);

    // 1. The boot image, where U-Boot reads it.
    let boot = part(p.boot);
    if boot.len() > layout::BOOT_MAX {
        puts("nandboot: the boot image is over 3 MiB; nothing written\n");
        return Ok(());
    }
    write(&nfc, &bad, layout::BOOT, boot)?;
    report("boot image", boot.len(), verify(&nfc, &bad, layout::BOOT, boot)?);

    // 2. The system copy: its header block erased first (the copy is
    //    invalid from here), the data from the next good block on, the
    //    header page last.
    let slot = layout::slot(p.slot).ok_or(nfc::Error::Range)?;
    let sys = part(p.system);
    let mut blocks = slot.good(&bad);
    let header_block = blocks.next().ok_or(nfc::Error::Range)?;
    let data_first = blocks.next().ok_or(nfc::Error::Range)?;
    nfc.erase_block(header_block)?;
    let data_region = Region { first: data_first, end: slot.end };
    let data = &sys[BLOCK..];
    write(&nfc, &bad, data_region, data)?;
    let ok = verify(&nfc, &bad, data_region, data)?;
    report("system copy", sys.len(), ok);
    if !ok {
        return Ok(());
    }
    let mut page = [0xffu8; PAGE];
    page.copy_from_slice(&sys[..PAGE]);
    nfc.program_page(header_block * PAGES_PER_BLOCK, &page)?;
    let mut back = [0u8; PAGE];
    nfc.read_page(header_block * PAGES_PER_BLOCK, &mut back, None)?;
    match SysHeader::decode(&back) {
        Some(h) => {
            puts("nandboot: copy ");
            putc(p.slot);
            puts(" is build ");
            puts(h.build_str());
            puts("\n");
        }
        None => {
            puts("nandboot: the system header didn't read back\n");
            return Ok(());
        }
    }

    // 3. The config, as the newest record.
    let cfg = part(p.config);
    if !cfg.is_empty() {
        let (seq, next) = match newest_config(&nfc, &bad) {
            Some((c, b)) => (c.seq + 1, next_block(&bad, layout::CONFIG, b)),
            None => (1, layout::CONFIG.good(&bad).next().ok_or(nfc::Error::Range)?),
        };
        let h = Config { seq, kind: ConfigKind::Current, len: cfg.len() as u32, crc: image::crc32(0, cfg) };
        let mut rec = [0xffu8; image::CONFIG_HEADER_LEN + image::CONFIG_MAX];
        rec[..image::CONFIG_HEADER_LEN].copy_from_slice(&h.encode());
        rec[image::CONFIG_HEADER_LEN..image::CONFIG_HEADER_LEN + cfg.len()].copy_from_slice(cfg);
        let n = image::CONFIG_HEADER_LEN + cfg.len();
        let r = Region { first: next, end: next + 1 };
        write(&nfc, &bad, r, &rec[..n])?;
        report("config", cfg.len(), verify(&nfc, &bad, r, &rec[..n])?);
    }

    // 4. The state record: this copy is the active one (confirmed: a
    //    flash from the bench needs no trial).
    let s = append_state(&nfc, &bad, State { seq: 0, active: p.slot, trial: 0, starts: 0 })?;
    match newest_state(&nfc, &bad) {
        Some((n, _)) if n == s => puts("nandboot: state written; flash done\n"),
        _ => puts("nandboot: the state record didn't read back\n"),
    }
    Ok(())
}

/// The good block after `b` in `ring`, wrapping.
fn next_block(bad: &BadBlocks, ring: Region, b: u32) -> u32 {
    system::next_block(bad, ring, b)
}

/// Write a state record (its sequence number following the newest).
fn append_state(nfc: &Nfc, bad: &BadBlocks, s: State) -> Result<State, nfc::Error> {
    system::append_state(&mut { nfc }, bad, s)
}

fn report(what: &str, len: usize, ok: bool) {
    puts("nandboot: ");
    puts(what);
    puts(", ");
    dec(len as u32);
    puts(if ok { " bytes: written and verified\n" } else { " bytes: VERIFY FAILED\n" });
}

fn check_id(nfc: &Nfc) -> Result<(), nfc::Error> {
    let id = nfc.read_id()?;
    puts("nandboot: NAND ID ");
    hex(u32::from_le_bytes(id));
    puts("\n");
    if id[..2] != nfc::ID {
        puts("nandboot: not the Toon's chip (ec f1)\n");
        return Err(nfc::Error::Range);
    }
    Ok(())
}

fn print_bad(bad: &BadBlocks) {
    puts("nandboot: bad blocks:");
    for b in bad.iter() {
        puts(" ");
        dec(b);
    }
    puts("\n");
}

// ---- probe ----

fn probe() -> Result<(), nfc::Error> {
    let nfc = unsafe { Nfc::new(0xd800_0000) };
    puts("\nnandboot: probe (read-only)\n");
    nfc.init()?;
    check_id(&nfc)?;
    let bad = scan(&nfc, 0)?;
    print_bad(&bad);
    let mut page = [0u8; PAGE];
    for b in [0u32, 8, 24, 26, 72, 1000] {
        let mut crc = 0;
        let mut corrected = 0u32;
        for p in 0..PAGES_PER_BLOCK {
            let r = nfc.read_page(b * PAGES_PER_BLOCK + p, &mut page, None)?;
            corrected += r.corrected as u32;
            crc = image::crc32(crc, &page);
        }
        puts("nandboot: block ");
        dec(b);
        puts(" CRC ");
        hex(crc);
        puts(", ");
        dec(corrected);
        puts(" subpages corrected\n");
    }
    // An erased page, if the last block is one.
    match nfc.read_page(1023 * PAGES_PER_BLOCK + 63, &mut page, None) {
        Ok(_) => {
            puts("nandboot: last page reads ");
            puts(if page.iter().all(|&x| x == 0xff) { "erased\n" } else { "with data\n" });
        }
        Err(e) => err("last page", e),
    }
    puts("nandboot: probe done\n");
    Ok(())
}

#[unsafe(no_mangle)]
extern "C" fn main(mode: u32) -> ! {
    match mode {
        0 => boot(),
        1 => {
            if let Err(e) = flash() {
                err("flash", e);
            }
        }
        _ => {
            if let Err(e) = probe() {
                err("probe", e);
            }
        }
    }
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    puts("\nnandboot: panic\n");
    loop {}
}
