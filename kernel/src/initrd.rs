//! The initrd: a ustar archive of server programs, loaded next to the kernel.
//! The format is libs/tar's, which init and xtask share.

use alloc::vec::Vec;

/// Files in the archive: (name, contents).
pub fn files(tar: &[u8]) -> Vec<(&str, &[u8])> {
    touchos_tar::files(tar).map(|f| (f.name, f.data)).collect()
}

pub fn find<'a>(tar: &'a [u8], name: &str) -> Option<&'a [u8]> {
    touchos_tar::find(tar, name)
}
