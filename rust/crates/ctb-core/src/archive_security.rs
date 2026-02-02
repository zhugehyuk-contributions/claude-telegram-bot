//! Safe archive extraction utilities (zip/tar) for the Rust port.
//!
//! This module exists to defend against common archive attacks:
//! - Path traversal (`../`, absolute paths, Windows drive prefixes)
//! - Symlink/hardlink entries that escape the extraction directory
//! - Resource exhaustion (too many files / too much total content)

use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use tar::Archive;
use zip::ZipArchive;

use crate::{errors::Error, Result};

#[derive(Clone, Copy, Debug)]
pub struct ExtractLimits {
    /// Maximum number of regular files extracted.
    pub max_files: usize,
    /// Maximum total bytes extracted across all regular files.
    pub max_total_bytes: u64,
    /// Maximum bytes extracted per file.
    pub max_file_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_files: 200,
            max_total_bytes: 10 * 1024 * 1024, // 10MB
            max_file_bytes: 512 * 1024,        // 512KB per file
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExtractReport {
    pub extracted_files: Vec<PathBuf>, // relative paths
    pub total_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

pub fn detect_archive_kind(file_name: &str) -> Option<ArchiveKind> {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".zip") {
        return Some(ArchiveKind::Zip);
    }
    if lower.ends_with(".tar") {
        return Some(ArchiveKind::Tar);
    }
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        return Some(ArchiveKind::TarGz);
    }
    None
}

pub fn safe_extract_archive(
    archive_path: &Path,
    file_name: &str,
    dest_dir: &Path,
    limits: ExtractLimits,
) -> Result<ExtractReport> {
    fs::create_dir_all(dest_dir)?;

    match detect_archive_kind(file_name) {
        Some(ArchiveKind::Zip) => safe_extract_zip(archive_path, dest_dir, limits),
        Some(ArchiveKind::Tar) => safe_extract_tar(archive_path, dest_dir, limits),
        Some(ArchiveKind::TarGz) => safe_extract_tar_gz(archive_path, dest_dir, limits),
        None => Err(Error::External(format!(
            "Unknown archive type for file: {file_name}"
        ))),
    }
}

fn safe_extract_zip(
    archive_path: &Path,
    dest_dir: &Path,
    limits: ExtractLimits,
) -> Result<ExtractReport> {
    let f = std::fs::File::open(archive_path)?;
    let mut zip = ZipArchive::new(f).map_err(|e| Error::External(format!("zip error: {e}")))?;

    let mut report = ExtractReport::default();
    let mut file_count = 0usize;
    let mut total = 0u64;

    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| Error::External(format!("zip error: {e}")))?;
        let name = entry.name().replace('\\', "/");
        if name.is_empty() {
            continue;
        }

        // Zip symlinks are commonly encoded via unix mode bits. Disallow them.
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind == 0o120000 {
                return Err(Error::Security(format!(
                    "archive contains symlink entry: {name}"
                )));
            }
        }

        let rel = sanitize_rel_path(Path::new(&name))?;
        let out_path = dest_dir.join(&rel);

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        file_count += 1;
        if file_count > limits.max_files {
            return Err(Error::Security(format!(
                "archive exceeds max_files limit ({})",
                limits.max_files
            )));
        }

        let size = entry.size();
        if size > limits.max_file_bytes {
            return Err(Error::Security(format!(
                "archive file too large: {} bytes (max {}) for {name}",
                size, limits.max_file_bytes
            )));
        }
        if total.saturating_add(size) > limits.max_total_bytes {
            return Err(Error::Security(format!(
                "archive exceeds max_total_bytes limit ({})",
                limits.max_total_bytes
            )));
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut out = std::fs::File::create(&out_path)?;
        // Enforce an upper bound even if zip metadata lies.
        let mut limited = entry.take(limits.max_file_bytes + 1);
        let copied = std::io::copy(&mut limited, &mut out)?;
        if copied > limits.max_file_bytes {
            return Err(Error::Security(format!(
                "archive entry exceeds max_file_bytes while extracting: {name}"
            )));
        }
        total += copied;

        report.extracted_files.push(rel);
        report.total_bytes = total;
    }

    Ok(report)
}

fn safe_extract_tar(
    archive_path: &Path,
    dest_dir: &Path,
    limits: ExtractLimits,
) -> Result<ExtractReport> {
    let f = std::fs::File::open(archive_path)?;
    safe_extract_tar_reader(f, dest_dir, limits)
}

fn safe_extract_tar_gz(
    archive_path: &Path,
    dest_dir: &Path,
    limits: ExtractLimits,
) -> Result<ExtractReport> {
    let f = std::fs::File::open(archive_path)?;
    let gz = GzDecoder::new(f);
    safe_extract_tar_reader(gz, dest_dir, limits)
}

fn safe_extract_tar_reader<R: Read>(
    r: R,
    dest_dir: &Path,
    limits: ExtractLimits,
) -> Result<ExtractReport> {
    let mut archive = Archive::new(r);
    let mut report = ExtractReport::default();
    let mut file_count = 0usize;
    let mut total = 0u64;

    for entry in archive.entries()? {
        let entry = entry?;
        let entry_type = entry.header().entry_type();

        // Disallow symlinks/hardlinks/devices/etc.
        if !entry_type.is_file() && !entry_type.is_dir() {
            let p = entry
                .path()
                .ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "<unknown>".to_string());
            return Err(Error::Security(format!(
                "archive contains non-file/non-dir entry: {p}"
            )));
        }

        let rel = sanitize_rel_path(&entry.path()?)?;
        let out_path = dest_dir.join(&rel);

        if entry_type.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        file_count += 1;
        if file_count > limits.max_files {
            return Err(Error::Security(format!(
                "archive exceeds max_files limit ({})",
                limits.max_files
            )));
        }

        let size = entry.header().size().unwrap_or(0);
        if size > limits.max_file_bytes {
            return Err(Error::Security(format!(
                "archive file too large: {} bytes (max {}) for {}",
                size,
                limits.max_file_bytes,
                rel.display()
            )));
        }
        if total.saturating_add(size) > limits.max_total_bytes {
            return Err(Error::Security(format!(
                "archive exceeds max_total_bytes limit ({})",
                limits.max_total_bytes
            )));
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut out = std::fs::File::create(&out_path)?;
        let mut limited = entry.take(limits.max_file_bytes + 1);
        let copied = std::io::copy(&mut limited, &mut out)?;
        if copied > limits.max_file_bytes {
            return Err(Error::Security(format!(
                "archive entry exceeds max_file_bytes while extracting: {}",
                rel.display()
            )));
        }
        total += copied;

        report.extracted_files.push(rel);
        report.total_bytes = total;
    }

    Ok(report)
}

fn sanitize_rel_path(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(os) => out.push(os),
            Component::ParentDir => {
                return Err(Error::Security(format!(
                    "archive contains path traversal: {}",
                    p.display()
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Security(format!(
                    "archive contains absolute path: {}",
                    p.display()
                )));
            }
        }
    }

    if out.as_os_str().is_empty() {
        return Err(Error::Security("archive contains empty path".to_string()));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(prefix: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let pid = std::process::id();
        let dir = PathBuf::from(format!("/tmp/{prefix}-{pid}-{ts}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn zip_blocks_path_traversal() {
        use zip::write::{FileOptions, ZipWriter};

        let base = tmp("zip");
        let zip_path = base.join("a.zip");
        let out_dir = base.join("out");
        fs::create_dir_all(&out_dir).unwrap();

        let f = std::fs::File::create(&zip_path).unwrap();
        let mut zw = ZipWriter::new(f);
        zw.start_file("../evil.txt", FileOptions::default())
            .unwrap();
        zw.write_all(b"x").unwrap();
        zw.finish().unwrap();

        let err = safe_extract_archive(&zip_path, "a.zip", &out_dir, ExtractLimits::default())
            .unwrap_err();
        assert!(matches!(err, Error::Security(_)));
    }

    #[test]
    fn tar_blocks_path_traversal() {
        let base = tmp("tar");
        let tar_path = base.join("a.tar");
        let out_dir = base.join("out");
        fs::create_dir_all(&out_dir).unwrap();

        write_raw_tar(&tar_path, "../evil.txt", b"x");

        let err = safe_extract_archive(&tar_path, "a.tar", &out_dir, ExtractLimits::default())
            .unwrap_err();
        assert!(matches!(err, Error::Security(_)));
    }

    #[test]
    fn tar_gz_blocks_path_traversal() {
        let base = tmp("targz");
        let tgz_path = base.join("a.tgz");
        let out_dir = base.join("out");
        fs::create_dir_all(&out_dir).unwrap();

        // Build a raw tar in-memory then gzip it.
        let raw = build_raw_tar_bytes("../evil.txt", b"x");
        let f = std::fs::File::create(&tgz_path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();

        let err = safe_extract_archive(&tgz_path, "a.tgz", &out_dir, ExtractLimits::default())
            .unwrap_err();
        assert!(matches!(err, Error::Security(_)));
    }

    #[test]
    fn enforces_per_file_size_limit() {
        use zip::write::{FileOptions, ZipWriter};

        let base = tmp("sizelimit");
        let zip_path = base.join("a.zip");
        let out_dir = base.join("out");
        fs::create_dir_all(&out_dir).unwrap();

        let f = std::fs::File::create(&zip_path).unwrap();
        let mut zw = ZipWriter::new(f);
        zw.start_file("big.txt", FileOptions::default()).unwrap();
        zw.write_all(b"hello").unwrap(); // 5 bytes
        zw.finish().unwrap();

        let limits = ExtractLimits {
            max_files: 10,
            max_total_bytes: 100,
            max_file_bytes: 4,
        };
        let err = safe_extract_archive(&zip_path, "a.zip", &out_dir, limits).unwrap_err();
        assert!(matches!(err, Error::Security(_)));
    }

    #[test]
    fn enforces_total_size_limit() {
        use zip::write::{FileOptions, ZipWriter};

        let base = tmp("totallimit");
        let zip_path = base.join("a.zip");
        let out_dir = base.join("out");
        fs::create_dir_all(&out_dir).unwrap();

        let f = std::fs::File::create(&zip_path).unwrap();
        let mut zw = ZipWriter::new(f);
        zw.start_file("a.txt", FileOptions::default()).unwrap();
        zw.write_all(b"hello").unwrap(); // 5
        zw.start_file("b.txt", FileOptions::default()).unwrap();
        zw.write_all(b"world").unwrap(); // 5
        zw.finish().unwrap();

        let limits = ExtractLimits {
            max_files: 10,
            max_total_bytes: 9, // < 10
            max_file_bytes: 10,
        };
        let err = safe_extract_archive(&zip_path, "a.zip", &out_dir, limits).unwrap_err();
        assert!(matches!(err, Error::Security(_)));
    }

    fn write_raw_tar(path: &Path, name: &str, data: &[u8]) {
        let bytes = build_raw_tar_bytes(name, data);
        std::fs::write(path, bytes).unwrap();
    }

    fn build_raw_tar_bytes(name: &str, data: &[u8]) -> Vec<u8> {
        // Minimal ustar header (512 bytes) + file data padded to 512 + end markers.
        let mut header = [0u8; 512];

        // name (0..100)
        let name_bytes = name.as_bytes();
        let n = name_bytes.len().min(100);
        header[0..n].copy_from_slice(&name_bytes[0..n]);

        // mode (100..108)
        write_octal(&mut header[100..108], 0o644);
        // uid/gid
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        // size (124..136)
        write_octal12(&mut header[124..136], data.len() as u64);
        // mtime (136..148)
        write_octal12(&mut header[136..148], 0);

        // checksum field treated as spaces for calculation (148..156)
        for b in &mut header[148..156] {
            *b = b' ';
        }

        // typeflag (156)
        header[156] = b'0';

        // magic + version
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");

        // compute checksum
        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        write_checksum(&mut header[148..156], sum);

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        // pad to 512 boundary
        let pad = (512 - (data.len() % 512)) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        // end-of-archive: two 512-byte blocks of zeros
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }

    fn write_octal(dst: &mut [u8], val: u64) {
        // NUL-terminated octal with trailing space if it fits.
        let width = dst.len();
        let s = format!("{val:0width$o}\0", width = width - 1);
        dst.copy_from_slice(&s.as_bytes()[0..width]);
    }

    fn write_octal12(dst: &mut [u8], val: u64) {
        // 11 digits + NUL (tar size/mtime fields are 12 bytes).
        let s = format!("{val:011o}\0");
        dst.copy_from_slice(s.as_bytes());
    }

    fn write_checksum(dst: &mut [u8], sum: u32) {
        // 6 digits, NUL, space
        let s = format!("{sum:06o}\0 ");
        dst.copy_from_slice(s.as_bytes());
    }
}
