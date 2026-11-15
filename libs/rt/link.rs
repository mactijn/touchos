// The linker script of user programs, for their build scripts:
//
//     include!("../../libs/rt/link.rs");
//     fn main() { link_user_program(); }
//
// (`include!`, not a build dependency: a build script can't depend on a
// no_std crate cheaply, and this is one rule.)

/// Link the program's binaries with the user-program layout: libs/rt/user.ld.
fn link_user_program() {
    let ld = concat!(env!("CARGO_MANIFEST_DIR"), "/../../libs/rt/user.ld");
    println!("cargo:rustc-link-arg-bins=-T{ld}");
    println!("cargo:rerun-if-changed={ld}");
}
