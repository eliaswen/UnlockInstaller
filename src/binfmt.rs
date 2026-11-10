//! Tiny PE / ELF header readers used to tell 32-bit from 64-bit binaries.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub enum Arch {
    X86,
    X64,
}

impl Arch {
    pub fn bits(self) -> &'static str {
        match self {
            Arch::X86 => "32",
            Arch::X64 => "64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pe,
    Elf,
}

/// Inspect the header bytes of a Windows PE or Linux ELF image.
pub fn detect(bytes: &[u8]) -> Option<(Format, Arch)> {
    if bytes.len() >= 5 && &bytes[0..4] == b"\x7fELF" {
        return match bytes[4] {
            1 => Some((Format::Elf, Arch::X86)),
            2 => Some((Format::Elf, Arch::X64)),
            _ => None,
        };
    }
    if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(bytes[0x3C..0x40].try_into().ok()?) as usize;
    if bytes.get(off..off + 4)? != b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes(bytes.get(off + 4..off + 6)?.try_into().ok()?) {
        0x014c => Some((Format::Pe, Arch::X86)),
        0x8664 => Some((Format::Pe, Arch::X64)),
        _ => None,
    }
}

/// Read just enough of a file to determine its architecture.
pub fn file_arch(path: &Path) -> Option<Arch> {
    let mut f = File::open(path).ok()?;
    let mut head = Vec::with_capacity(0x40);
    (&mut f).take(0x40).read_to_end(&mut head).ok()?;
    if head.starts_with(b"\x7fELF") {
        return detect(&head).map(|(_, a)| a);
    }
    if head.len() < 0x40 || &head[0..2] != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(head[0x3C..0x40].try_into().ok()?) as u64;
    let mut pe = [0u8; 6];
    f.seek(SeekFrom::Start(off)).ok()?;
    f.read_exact(&mut pe).ok()?;
    if &pe[0..4] != b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes([pe[4], pe[5]]) {
        0x014c => Some(Arch::X86),
        0x8664 => Some(Arch::X64),
        _ => None,
    }
}

/// Case-insensitive ASCII substring search over binary data.
pub fn contains_ci(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    hay.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fake_pe(machine: u16) -> Vec<u8> {
        let mut img = vec![0u8; 0x100];
        img[0..2].copy_from_slice(b"MZ");
        img[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        img[0x80..0x84].copy_from_slice(b"PE\0\0");
        img[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        img
    }

    #[test]
    fn pe_and_elf() {
        assert_eq!(detect(&fake_pe(0x8664)), Some((Format::Pe, Arch::X64)));
        assert_eq!(detect(&fake_pe(0x014c)), Some((Format::Pe, Arch::X86)));
        assert_eq!(detect(b"\x7fELF\x02rest"), Some((Format::Elf, Arch::X64)));
        assert_eq!(detect(b"\x7fELF\x01rest"), Some((Format::Elf, Arch::X86)));
        assert_eq!(detect(b"garbage"), None);
    }

    #[test]
    fn file_arch_reads_pe() {
        let p = std::env::temp_dir().join(format!("binfmt-{}.exe", std::process::id()));
        std::fs::write(&p, fake_pe(0x8664)).unwrap();
        assert_eq!(file_arch(&p), Some(Arch::X64));
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn ci_search() {
        assert!(contains_ci(b"xx Koaloader yy", "koaloader"));
        assert!(!contains_ci(b"xx", "koaloader"));
    }
}
