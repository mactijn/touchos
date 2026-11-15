//! Build script: link the kernel with linker-arm.ld and board.ld, written here for the `board-*` feature from
//! `touchos_abi::imx_boot` (where xtask, which loads the kernel, and the
//! kernel's `soc` get them too): the board's RAM base and UART, the
//! kernel's virtual base and its load offset in RAM.

use touchos_abi::imx_boot;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "arm" {
        let toon = std::env::var("CARGO_FEATURE_BOARD_TOON").is_ok();
        let imx25 = std::env::var("CARGO_FEATURE_BOARD_IMX25").is_ok();
        let board = match (toon, imx25) {
            (true, false) => imx_boot::TOON,
            (false, true) => imx_boot::IMX25,
            _ => panic!("enable exactly one of the features board-toon, board-imx25"),
        };
        let out = std::env::var("OUT_DIR").unwrap();
        std::fs::write(
            format!("{out}/board.ld"),
            format!(
                "__ram_base = {:#x};\n__uart_phys = {:#x};\n__kernel_virt = {:#x};\n__kernel_offset = {:#x};\n",
                board.ram_base,
                board.uart,
                imx_boot::KERNEL_VIRT,
                imx_boot::KERNEL,
            ),
        )
        .unwrap();
        println!("cargo:rustc-link-arg-bin=touchos=-L{out}");
        println!("cargo:rustc-link-arg-bin=touchos=-T{dir}/linker-arm.ld");
    }
    println!("cargo:rerun-if-changed=linker-arm.ld");
    println!("cargo:rerun-if-changed=build.rs");
}
