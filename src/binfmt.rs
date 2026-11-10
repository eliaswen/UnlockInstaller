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

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Names of the DLLs a PE image links against, from both its import and
/// delay-import tables, lowercased (e.g. `version.dll`). Only the headers
/// and the bytes the tables point at are read, so large executables are
/// cheap. Returns `None` for anything that isn't a readable PE file.
pub fn pe_imports(path: &Path) -> Option<Vec<String>> {
    let mut f = File::open(path).ok()?;
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> {
        let mut buf = Vec::with_capacity(len);
        f.seek(SeekFrom::Start(off)).ok()?;
        (&mut f).take(len as u64).read_to_end(&mut buf).ok()?;
        Some(buf)
    };

    let dos = read(0, 0x40)?;
    if dos.len() < 0x40 || &dos[0..2] != b"MZ" {
        return None;
    }
    let pe_off = u32_at(&dos, 0x3C)? as u64;
    let coff = read(pe_off, 24)?;
    if coff.get(0..4)? != b"PE\0\0" {
        return None;
    }
    let n_sections = u16_at(&coff, 6)? as usize;
    let opt_size = u16_at(&coff, 20)? as usize;
    let opt = read(pe_off + 24, opt_size)?;
    let (image_base, n_dirs_off, dirs_off) = match u16_at(&opt, 0)? {
        0x10b => (u32_at(&opt, 28)? as u64, 92, 96),
        0x20b => (u64_at(&opt, 24)?, 108, 112),
        _ => return None,
    };
    let n_dirs = u32_at(&opt, n_dirs_off)? as usize;
    let dir = |i: usize| -> Option<(u32, u32)> {
        if i >= n_dirs {
            return None;
        }
        let o = dirs_off + i * 8;
        Some((u32_at(&opt, o)?, u32_at(&opt, o + 4)?))
    };

    // (virtual address, virtual size, raw size, raw offset)
    let sec_bytes = read(pe_off + 24 + opt_size as u64, n_sections * 40)?;
    let sections: Vec<(u32, u32, u32, u32)> = sec_bytes
        .as_chunks::<40>()
        .0
        .iter()
        .filter_map(|s| Some((u32_at(s, 12)?, u32_at(s, 8)?, u32_at(s, 16)?, u32_at(s, 20)?)))
        .collect();
    let rva_to_off = |rva: u32| -> Option<u64> {
        sections.iter().find_map(|&(va, vsize, rsize, raw)| {
            let size = vsize.max(rsize);
            (rva >= va && rva < va.checked_add(size)?).then(|| raw as u64 + (rva - va) as u64)
        })
    };

    let mut name_rvas: Vec<u32> = Vec::new();

    // Bound the walk so a corrupt table can't make us read forever.
    const MAX_DESCRIPTORS: usize = 4096;

    let mut table = |idx: usize, entry: usize| -> Vec<u8> {
        dir(idx)
            .filter(|&(rva, size)| rva != 0 && size != 0)
            .and_then(|(rva, _)| rva_to_off(rva))
            .and_then(|off| read(off, entry * MAX_DESCRIPTORS))
            .unwrap_or_default()
    };

    // Import directory: 20-byte descriptors, name RVA at +12.
    for d in table(1, 20).as_chunks::<20>().0 {
        if d.iter().all(|&b| b == 0) {
            break;
        }
        if let Some(name) = u32_at(d, 12).filter(|&n| n != 0) {
            name_rvas.push(name);
        }
    }

    // Delay-import directory: 32-byte descriptors, name at +4. Old-style
    // descriptors (attribute bit 0 clear) hold VAs instead of RVAs.
    for d in table(13, 32).as_chunks::<32>().0 {
        if d.iter().all(|&b| b == 0) {
            break;
        }
        let (Some(attrs), Some(name)) = (u32_at(d, 0), u32_at(d, 4)) else { break };
        let name = if attrs & 1 == 0 {
            (name as u64).checked_sub(image_base).and_then(|r| u32::try_from(r).ok())
        } else {
            Some(name)
        };
        if let Some(name) = name.filter(|&n| n != 0) {
            name_rvas.push(name);
        }
    }

    let mut names: Vec<String> = Vec::new();
    for rva in name_rvas {
        let Some(bytes) = rva_to_off(rva).and_then(|off| read(off, 256)) else { continue };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let Ok(s) = std::str::from_utf8(&bytes[..end]) else { continue };
        let s = s.to_ascii_lowercase();
        if !s.is_empty() && !names.contains(&s) {
            names.push(s);
        }
    }
    Some(names)
}

/// Case-insensitive ASCII substring search over binary data.
pub fn contains_ci(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    let Some(&first) = n.first() else { return true };
    if hay.len() < n.len() {
        return false;
    }
    // Only positions where a whole needle still fits can match; memchr
    // jumps between candidates for the first byte (either case) with SIMD,
    // which matters for the multi-megabyte DLLs some games ship.
    let last_start = hay.len() - n.len();
    memchr::memchr2_iter(first.to_ascii_lowercase(), first.to_ascii_uppercase(), &hay[..=last_start])
        .any(|i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// A minimal PE32+ image with one `.idata` section holding an import
    /// table for `dlls` and a delay-import table for `delayed`.
    pub fn fake_pe_with_imports(dlls: &[&str], delayed: &[&str]) -> Vec<u8> {
        const SEC_VA: u32 = 0x1000;
        const SEC_RAW: u32 = 0x200;
        let mut img = vec![0u8; SEC_RAW as usize];
        img[0..2].copy_from_slice(b"MZ");
        img[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        img[0x40..0x44].copy_from_slice(b"PE\0\0");
        img[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        img[0x46..0x48].copy_from_slice(&1u16.to_le_bytes()); // sections
        let opt_size: u16 = 112 + 16 * 8;
        img[0x54..0x56].copy_from_slice(&opt_size.to_le_bytes());
        let opt = 0x58;
        img[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        img[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        img[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());

        // section body: import descriptors, delay descriptors, then names
        let mut body = vec![0u8; 0x400];
        let imp_at = 0usize;
        let delay_at = 20 * (dlls.len() + 1);
        let mut name_at = delay_at + 32 * (delayed.len() + 1);
        let mut put_name = |body: &mut Vec<u8>, n: &str| -> u32 {
            let at = name_at;
            body[at..at + n.len()].copy_from_slice(n.as_bytes());
            name_at += n.len() + 1;
            SEC_VA + at as u32
        };
        for (i, d) in dlls.iter().enumerate() {
            let rva = put_name(&mut body, d);
            let o = imp_at + 20 * i + 12;
            body[o..o + 4].copy_from_slice(&rva.to_le_bytes());
        }
        for (i, d) in delayed.iter().enumerate() {
            let rva = put_name(&mut body, d);
            let o = delay_at + 32 * i;
            body[o..o + 4].copy_from_slice(&1u32.to_le_bytes());
            body[o + 4..o + 8].copy_from_slice(&rva.to_le_bytes());
        }
        let dirs = opt + 112;
        img[dirs + 8..dirs + 12].copy_from_slice(&(SEC_VA + imp_at as u32).to_le_bytes());
        img[dirs + 12..dirs + 16].copy_from_slice(&(20 * (dlls.len() as u32 + 1)).to_le_bytes());
        if !delayed.is_empty() {
            let d13 = dirs + 13 * 8;
            img[d13..d13 + 4].copy_from_slice(&(SEC_VA + delay_at as u32).to_le_bytes());
            img[d13 + 4..d13 + 8].copy_from_slice(&(32 * (delayed.len() as u32 + 1)).to_le_bytes());
        }
        let sec = opt + opt_size as usize;
        img[sec..sec + 6].copy_from_slice(b".idata");
        img[sec + 8..sec + 12].copy_from_slice(&(body.len() as u32).to_le_bytes());
        img[sec + 12..sec + 16].copy_from_slice(&SEC_VA.to_le_bytes());
        img[sec + 16..sec + 20].copy_from_slice(&(body.len() as u32).to_le_bytes());
        img[sec + 20..sec + 24].copy_from_slice(&SEC_RAW.to_le_bytes());
        img.extend_from_slice(&body);
        img
    }

    #[test]
    fn reads_pe_imports() {
        let p = std::env::temp_dir().join(format!("binfmt-imp-{}.exe", std::process::id()));
        std::fs::write(&p, fake_pe_with_imports(&["KERNEL32.dll", "VERSION.dll"], &["winmm.dll"]))
            .unwrap();
        assert_eq!(
            pe_imports(&p),
            Some(vec!["kernel32.dll".into(), "version.dll".into(), "winmm.dll".into()])
        );
        std::fs::write(&p, b"not a pe").unwrap();
        assert_eq!(pe_imports(&p), None);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn ci_search() {
        assert!(contains_ci(b"xx Koaloader yy", "koaloader"));
        assert!(!contains_ci(b"xx", "koaloader"));
        assert!(contains_ci(b"KOALOADER", "koaloader"));
        assert!(contains_ci(b"kkoaloader", "koaloader"));
        assert!(!contains_ci(b"xx koaloade", "koaloader"));
    }
}
