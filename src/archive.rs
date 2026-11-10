//! Format-independent archive extraction.
//!
//! Supports zip, 7z, rar and tar (plain, .gz, .bz2, .xz, .zst). The format
//! is detected from the file's magic bytes, not its extension. zip, 7z and
//! rar archives may be password-protected.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveError {
    /// The archive is encrypted and no password was given.
    NeedPassword,
    /// The given password is wrong.
    WrongPassword,
    Failed(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::NeedPassword => f.write_str("The archive is password-protected"),
            ArchiveError::WrongPassword => f.write_str("Wrong password"),
            ArchiveError::Failed(s) => f.write_str(s),
        }
    }
}

pub struct ArchiveFile {
    /// Path inside the archive, with `/` separators.
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
}

/// File name patterns for file pickers.
pub const PATTERNS: &[&str] = &[
    "*.zip", "*.7z", "*.rar", "*.tar", "*.tar.gz", "*.tgz", "*.tar.bz2", "*.tbz2", "*.tar.xz",
    "*.txz", "*.tar.zst", "*.tzst",
];

/// Whether a file name looks like an archive we can read.
pub fn has_archive_ext(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    PATTERNS.iter().any(|p| n.ends_with(&p[1..]))
}

fn detect(path: &Path) -> Result<Format, ArchiveError> {
    let mut head = Vec::with_capacity(512);
    File::open(path)
        .and_then(|f| f.take(512).read_to_end(&mut head))
        .map_err(|e| ArchiveError::Failed(format!("Cannot read {}: {e}", path.display())))?;
    let starts = |m: &[u8]| head.starts_with(m);
    Ok(if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        Format::Zip
    } else if starts(b"7z\xBC\xAF\x27\x1C") {
        Format::SevenZ
    } else if starts(b"Rar!\x1A\x07") {
        Format::Rar
    } else if starts(b"\x1F\x8B") {
        Format::TarGz
    } else if starts(b"BZh") {
        Format::TarBz2
    } else if starts(b"\xFD7zXZ\x00") {
        Format::TarXz
    } else if starts(b"\x28\xB5\x2F\xFD") {
        Format::TarZst
    } else if head.get(257..262) == Some(b"ustar") {
        Format::Tar
    } else {
        return Err(ArchiveError::Failed(
            "Unsupported file: expected a zip, 7z, rar or tar archive".into(),
        ));
    })
}

/// Extract every file whose in-archive path satisfies `want`.
pub fn extract(
    path: &Path,
    password: Option<&str>,
    want: impl Fn(&str) -> bool,
) -> Result<Vec<ArchiveFile>, ArchiveError> {
    match detect(path)? {
        Format::Zip => extract_zip(path, password, &want),
        Format::SevenZ => extract_7z(path, password, &want),
        Format::Rar => extract_rar(path, password, &want),
        f => {
            let file = open(path)?;
            let reader: Box<dyn Read> = match f {
                Format::Tar => Box::new(file),
                Format::TarGz => Box::new(flate2::read::MultiGzDecoder::new(file)),
                Format::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
                Format::TarXz => Box::new(lzma_rust2::XzReader::new(file, true)),
                Format::TarZst => Box::new(zstd::stream::read::Decoder::new(file).map_err(fail)?),
                _ => unreachable!(),
            };
            extract_tar(reader, &want)
        }
    }
}

fn fail(e: impl std::fmt::Display) -> ArchiveError {
    ArchiveError::Failed(e.to_string())
}

fn open(path: &Path) -> Result<File, ArchiveError> {
    File::open(path).map_err(|e| fail(format!("Cannot open {}: {e}", path.display())))
}

fn norm(name: &str) -> String {
    name.replace('\\', "/")
}

fn extract_zip(
    path: &Path,
    password: Option<&str>,
    want: &dyn Fn(&str) -> bool,
) -> Result<Vec<ArchiveFile>, ArchiveError> {
    let mut archive = zip::ZipArchive::new(open(path)?).map_err(|e| fail(format!("Bad zip: {e}")))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let (name, encrypted) = {
            let raw = archive.by_index_raw(i).map_err(fail)?;
            if raw.is_dir() {
                continue;
            }
            (norm(&raw.name().map_err(fail)?), raw.encrypted())
        };
        if !want(&name) {
            continue;
        }
        if encrypted && password.is_none() {
            return Err(ArchiveError::NeedPassword);
        }
        let mut entry = match password {
            Some(pw) if encrypted => archive.by_index_decrypt(i, pw.as_bytes()),
            _ => archive.by_index(i),
        }
        .map_err(|e| match e {
            zip::result::ZipError::InvalidPassword => ArchiveError::WrongPassword,
            e => fail(format!("Cannot extract {name}: {e}")),
        })?;
        let mut bytes = Vec::new();
        if let Err(e) = entry.read_to_end(&mut bytes) {
            // ZipCrypto only has a 1-byte password check; a wrong password
            // usually shows up as a checksum error while reading.
            return Err(if encrypted { ArchiveError::WrongPassword } else { fail(format!("{name}: {e}")) });
        }
        out.push(ArchiveFile { name, bytes });
    }
    Ok(out)
}

fn extract_7z(
    path: &Path,
    password: Option<&str>,
    want: &dyn Fn(&str) -> bool,
) -> Result<Vec<ArchiveFile>, ArchiveError> {
    use sevenz_rust2::{ArchiveReader, Error, Password};
    let map = |e: Error| match e {
        Error::PasswordRequired if password.is_none() => ArchiveError::NeedPassword,
        Error::PasswordRequired | Error::MaybeBadPassword(_) => ArchiveError::WrongPassword,
        // Garbage from a wrong key usually breaks decompression or the CRC.
        _ if password.is_some() => ArchiveError::WrongPassword,
        e => fail(format!("Bad 7z archive: {e}")),
    };
    let pw = password.map(Password::from).unwrap_or_else(Password::empty);
    let mut reader = ArchiveReader::new(open(path)?, pw).map_err(map)?;
    let mut out = Vec::new();
    reader
        .for_each_entries(|entry, data| {
            let name = norm(entry.name());
            if !entry.is_directory() && want(&name) {
                let mut bytes = Vec::new();
                data.read_to_end(&mut bytes)?;
                out.push(ArchiveFile { name, bytes });
            } else {
                io::copy(data, &mut io::sink())?;
            }
            Ok(true)
        })
        .map_err(map)?;
    Ok(out)
}

/// Builds without the `rar` feature (cross-compiled Windows builds) cannot
/// read rar archives.
#[cfg(not(feature = "rar"))]
fn extract_rar(
    _path: &Path,
    _password: Option<&str>,
    _want: &dyn Fn(&str) -> bool,
) -> Result<Vec<ArchiveFile>, ArchiveError> {
    Err(fail("This build cannot read rar archives; use a zip, 7z or tar archive instead"))
}

#[cfg(feature = "rar")]
fn extract_rar(
    path: &Path,
    password: Option<&str>,
    want: &dyn Fn(&str) -> bool,
) -> Result<Vec<ArchiveFile>, ArchiveError> {
    use unrar_ng::error::{Code, UnrarError};
    let map = |e: UnrarError| match e.code {
        Code::MissingPassword if password.is_none() => ArchiveError::NeedPassword,
        Code::MissingPassword | Code::BadPassword => ArchiveError::WrongPassword,
        // RAR4 has no password check; a wrong key surfaces as a CRC error.
        Code::BadData if password.is_some() => ArchiveError::WrongPassword,
        _ => fail(format!("Bad rar archive: {e}")),
    };
    let archive = match password {
        Some(pw) => unrar_ng::Archive::with_password(path, pw),
        None => unrar_ng::Archive::new(path),
    };
    let mut cursor = archive.open_for_processing().map_err(map)?;
    let mut out = Vec::new();
    while let Some(header) = cursor.read_header().map_err(map)? {
        let entry = header.entry();
        let name = norm(&entry.filename.to_string_lossy());
        if entry.is_file() && want(&name) {
            if entry.is_encrypted() && password.is_none() {
                return Err(ArchiveError::NeedPassword);
            }
            let (bytes, next) = header.read().map_err(map)?;
            out.push(ArchiveFile { name, bytes });
            cursor = next;
        } else {
            cursor = header.skip().map_err(map)?;
        }
    }
    Ok(out)
}

fn extract_tar(reader: Box<dyn Read>, want: &dyn Fn(&str) -> bool) -> Result<Vec<ArchiveFile>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries().map_err(|e| fail(format!("Bad tar archive: {e}")))? {
        let mut entry = entry.map_err(|e| fail(format!("Bad tar archive: {e}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = norm(&entry.path().map_err(fail)?.to_string_lossy());
        if want(&name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(fail)?;
            out.push(ArchiveFile { name, bytes });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ci-arch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(files: &[ArchiveFile]) -> Vec<&str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn zip_encrypted() {
        use zip::write::SimpleFileOptions;
        let dir = tmp("zip");
        let p = dir.join("a.zip");
        let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
        let opts = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "secret");
        w.start_file("x64/steam_api64.dll", opts).unwrap();
        w.write_all(b"data").unwrap();
        w.finish().unwrap();

        assert_eq!(extract(&p, None, |_| true).err(), Some(ArchiveError::NeedPassword));
        assert_eq!(extract(&p, Some("nope"), |_| true).err(), Some(ArchiveError::WrongPassword));
        let files = extract(&p, Some("secret"), |_| true).unwrap();
        assert_eq!(names(&files), ["x64/steam_api64.dll"]);
        assert_eq!(files[0].bytes, b"data");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sevenz_plain_and_encrypted() {
        let dir = tmp("7z");
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/a.dll"), b"hello").unwrap();
        fs::write(src.join("readme.txt"), b"skip me").unwrap();

        let plain = dir.join("plain.7z");
        sevenz_rust2::compress_to_path(&src, &plain).unwrap();
        let files = extract(&plain, None, |n| n.ends_with(".dll")).unwrap();
        assert_eq!(names(&files), ["sub/a.dll"]);
        assert_eq!(files[0].bytes, b"hello");

        let enc = dir.join("enc.7z");
        sevenz_rust2::compress_to_path_encrypted(&src, &enc, "pw".into()).unwrap();
        assert_eq!(extract(&enc, None, |_| true).err(), Some(ArchiveError::NeedPassword));
        assert_eq!(extract(&enc, Some("bad"), |_| true).err(), Some(ArchiveError::WrongPassword));
        let files = extract(&enc, Some("pw"), |n| n.ends_with(".dll")).unwrap();
        assert_eq!(files[0].bytes, b"hello");
        fs::remove_dir_all(dir).unwrap();
    }

    fn make_tar() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "dir/lib.so", &b"elf"[..]).unwrap();
        b.into_inner().unwrap()
    }

    #[test]
    fn tar_variants() {
        let dir = tmp("tar");
        let raw = make_tar();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        let zst = zstd::stream::encode_all(&raw[..], 3).unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bz.write_all(&raw).unwrap();
        let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
        xz.write_all(&raw).unwrap();
        for (name, bytes) in [
            ("a.tar", raw.clone()),
            ("a.tar.gz", gz.finish().unwrap()),
            ("a.tar.zst", zst),
            ("a.tar.bz2", bz.finish().unwrap()),
            ("a.tar.xz", xz.finish().unwrap()),
        ] {
            let p = dir.join(name);
            fs::write(&p, bytes).unwrap();
            let files = extract(&p, None, |_| true).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(names(&files), ["dir/lib.so"], "{name}");
            assert_eq!(files[0].bytes, b"elf", "{name}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_non_archives() {
        let dir = tmp("bad");
        let p = dir.join("x.zip");
        fs::write(&p, b"not an archive").unwrap();
        assert!(matches!(extract(&p, None, |_| true), Err(ArchiveError::Failed(_))));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_ext() {
        assert!(has_archive_ext("SmokeAPI-v4.1.3.zip"));
        assert!(has_archive_ext("x.TAR.GZ"));
        assert!(!has_archive_ext("SmokeAPI.dll"));
    }
}
