use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const VERSION: u16 = 20;
const UTF8_NAMES: u16 = 1 << 11;
const STORED: u16 = 0;
const MIDNIGHT: u16 = 0;
const JANUARY_1980: u16 = (1 << 5) | 1;
const LOCAL_HEADER: u64 = 30;
const CENTRAL_HEADER: u64 = 46;
const END_RECORD: u64 = 22;
const CHUNK: usize = 1 << 16;
const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut at = 0;
    while at < 256 {
        let mut crc = at as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            bit += 1;
        }
        table[at] = crc;
        at += 1;
    }
    table
};

pub(super) fn zip(folder: &Path, into: &Path) -> Result<PathBuf> {
    let mut names = Vec::new();
    walk(folder, Path::new(""), &mut names)?;
    names.sort();
    too_big(u16::try_from(names.len()), folder)?;
    too_big(u32::try_from(planned(folder, &names)), folder)?;
    std::fs::create_dir_all(into).with_context(|| format!("cannot create {}", into.display()))?;
    let mut name = folder
        .file_name()
        .map(ToOwned::to_owned)
        .or_else(|| Some(std::fs::canonicalize(folder).ok()?.file_name()?.to_owned()))
        .with_context(|| format!("{} has no name to give the zip", folder.display()))?;
    name.push(".zip");
    let archive = into.join(name);
    let partial = archive.with_extension("zip.partial");
    let written = write(folder, &names, &partial);
    if written.is_err() || same_bytes(&partial, &archive) {
        std::fs::remove_file(&partial).ok();
        written?;
    } else {
        std::fs::rename(&partial, &archive)
            .with_context(|| format!("cannot write {}", archive.display()))?;
    }
    Ok(archive)
}

fn write(folder: &Path, names: &[PathBuf], partial: &Path) -> Result<()> {
    let mut out = BufWriter::new(
        File::create(partial).with_context(|| format!("cannot write {}", partial.display()))?,
    );
    let (mut offset, mut central) = (0_u64, Vec::new());
    for name in names {
        let path = folder.join(name);
        let cannot_read = || format!("cannot read {}", path.display());
        let mut file = File::open(&path).with_context(cannot_read)?;
        let size = file.metadata().with_context(cannot_read)?.len();
        let size = too_big(u32::try_from(size), folder)?;
        let name = name.to_string_lossy();
        let mut fields = Vec::with_capacity(26);
        for half in [VERSION, UTF8_NAMES, STORED, MIDNIGHT, JANUARY_1980] {
            fields.extend(half.to_le_bytes());
        }
        let crc_at = fields.len();
        for word in [0, size, size] {
            fields.extend(word.to_le_bytes());
        }
        let length = too_big(u16::try_from(name.len()), folder)?;
        fields.extend(length.to_le_bytes());
        fields.extend(0_u16.to_le_bytes());
        out.write_all(&LOCAL.to_le_bytes())?;
        out.write_all(&fields)?;
        out.write_all(name.as_bytes())?;
        let (crc, copied) = copy(&mut file, &mut out).with_context(cannot_read)?;
        anyhow::ensure!(
            copied == u64::from(size),
            "{} changed while it was zipped",
            path.display()
        );
        fields[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
        out.seek(SeekFrom::Start(
            offset + (size_of_val(&LOCAL) + crc_at) as u64,
        ))?;
        out.write_all(&crc.to_le_bytes())?;
        out.seek(SeekFrom::End(0))?;
        central.extend(CENTRAL.to_le_bytes());
        central.extend(VERSION.to_le_bytes());
        central.extend(&fields);
        central.extend([0; 10]);
        central.extend(too_big(u32::try_from(offset), folder)?.to_le_bytes());
        central.extend(name.as_bytes());
        offset += LOCAL_HEADER + u64::from(length) + copied;
    }
    let count = too_big(u16::try_from(names.len()), folder)?;
    out.write_all(&central)?;
    out.write_all(&END.to_le_bytes())?;
    out.write_all(&[0; 4])?;
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&too_big(u32::try_from(central.len()), folder)?.to_le_bytes())?;
    out.write_all(&too_big(u32::try_from(offset), folder)?.to_le_bytes())?;
    out.write_all(&[0; 2])?;
    out.into_inner()?.sync_all()?;
    Ok(())
}

fn copy(from: &mut File, to: &mut impl Write) -> std::io::Result<(u32, u64)> {
    let (mut buffer, mut crc, mut size) = (vec![0; CHUNK], !0, 0);
    loop {
        let read = from.read(&mut buffer)?;
        if read == 0 {
            return Ok((!crc, size));
        }
        crc = update(crc, &buffer[..read]);
        to.write_all(&buffer[..read])?;
        size += read as u64;
    }
}

fn same_bytes(one: &Path, other: &Path) -> bool {
    let (Ok(one), Ok(other)) = (File::open(one), File::open(other)) else {
        return false;
    };
    let length = |file: &File| file.metadata().ok().map(|metadata| metadata.len());
    if length(&one).is_none() || length(&one) != length(&other) {
        return false;
    }
    let (mut one, mut other) = (BufReader::new(one), BufReader::new(other));
    let (mut mine, mut theirs) = (vec![0; CHUNK], vec![0; CHUNK]);
    loop {
        match one.read(&mut mine) {
            Ok(0) => return true,
            Ok(read) => {
                if other.read_exact(&mut theirs[..read]).is_err() || mine[..read] != theirs[..read]
                {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
}

fn walk(root: &Path, at: &Path, names: &mut Vec<PathBuf>) -> Result<()> {
    let folder = root.join(at);
    for entry in
        std::fs::read_dir(&folder).with_context(|| format!("cannot read {}", folder.display()))?
    {
        let entry = entry?;
        let name = at.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &name, names)?;
        } else if kind.is_file() || (kind.is_symlink() && entry.path().is_file()) {
            names.push(name);
        }
    }
    Ok(())
}

fn planned(folder: &Path, names: &[PathBuf]) -> u64 {
    names
        .iter()
        .map(|name| {
            let size = std::fs::metadata(folder.join(name)).map_or(0, |file| file.len());
            let name = name.to_string_lossy().len() as u64;
            LOCAL_HEADER + CENTRAL_HEADER + 2 * name + size
        })
        .sum::<u64>()
        + END_RECORD
}

fn too_big<T, E>(size: Result<T, E>, folder: &Path) -> Result<T> {
    size.map_err(|_| anyhow::anyhow!("{} is too big for a zip", folder.display()))
}

fn update(crc: u32, data: &[u8]) -> u32 {
    data.iter().fold(crc, |crc, &byte| {
        CRC_TABLE[usize::from(crc.to_le_bytes()[0] ^ byte)] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_is_kept_only_when_it_holds_the_same_bytes() {
        let dir = std::env::temp_dir().join(format!("iu2u-zip-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("archive.zip");
        std::fs::write(&archive, b"zip bytes").unwrap();
        let same = [
            b"zip bytes".as_slice(),
            b"zip byte",
            b"zip bytes!",
            b"zip bytez",
        ]
        .map(|body| {
            std::fs::write(dir.join("new.zip"), body).unwrap();
            same_bytes(&dir.join("new.zip"), &archive)
        });
        let missing = same_bytes(&dir.join("new.zip"), &dir.join("missing.zip"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(same, [true, false, false, false]);
        assert!(!missing);
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(!update(!0, b"123456789"), 0xCBF4_3926);
        assert_eq!(!update(!0, b""), 0);
    }

    #[test]
    fn a_folder_too_big_for_a_zip_is_known_before_it_is_read() {
        let folder = std::env::temp_dir().join(format!("iu2u-zip-big-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let big = std::fs::File::create(folder.join("big.bin")).unwrap();
        big.set_len(u64::from(u32::MAX) + 1).unwrap();
        let names = [PathBuf::from("big.bin")];
        let planned = planned(&folder, &names);
        std::fs::remove_dir_all(&folder).ok();
        assert!(planned > u64::from(u32::MAX));
    }

    #[test]
    fn a_folder_named_by_dots_zips_under_its_own_name() {
        let root = std::env::temp_dir().join(format!("iu2u-zip-dots-{}", std::process::id()));
        std::fs::create_dir_all(root.join("site/assets")).unwrap();
        std::fs::write(root.join("site/index.html"), "x").unwrap();
        let archive = zip(&root.join("site/assets/.."), &root.join("out"));
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(archive.unwrap().file_name().unwrap(), "site.zip");
    }

    #[test]
    fn a_folder_zips_with_its_structure() {
        let root = std::env::temp_dir().join(format!("iu2u-zip-test-{}", std::process::id()));
        let folder = root.join("alta-v1.2");
        std::fs::create_dir_all(folder.join("assets")).unwrap();
        std::fs::write(folder.join("index.html"), "<h1>Alta</h1>").unwrap();
        std::fs::write(folder.join("assets/estilo.css"), "body{}").unwrap();
        std::os::unix::fs::symlink(folder.join("assets"), folder.join("linked")).unwrap();
        std::fs::write(root.join("shared.txt"), "shared copy").unwrap();
        std::os::unix::fs::symlink(root.join("shared.txt"), folder.join("shared.txt")).unwrap();
        std::os::unix::net::UnixListener::bind(folder.join("s.sock")).unwrap();
        let archive = zip(&folder, &root.join("out")).unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(archive.file_name().unwrap(), "alta-v1.2.zip");
        assert_eq!(bytes[..4], LOCAL.to_le_bytes());
        let end = bytes.len() - 22;
        assert_eq!(bytes[end..end + 4], END.to_le_bytes());
        assert_eq!(bytes[end + 10..end + 12], 3_u16.to_le_bytes());
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let half =
            |at: usize| usize::from(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()));
        let mut entry = word(end + 16);
        assert_eq!(word(end + 12), end - entry);
        for _ in 0..3 {
            assert_eq!(bytes[entry..entry + 4], CENTRAL.to_le_bytes());
            let length = half(entry + 28);
            let local = word(entry + 42);
            assert_eq!(bytes[local..local + 4], LOCAL.to_le_bytes());
            assert_eq!(
                bytes[local + 30..local + 30 + length],
                bytes[entry + 46..entry + 46 + length]
            );
            let data = local + 30 + length;
            let crc = !update(!0, &bytes[data..data + word(entry + 20)]) as usize;
            assert_eq!([word(entry + 16), word(local + 14)], [crc, crc]);
            entry += 46 + length;
        }
        assert_eq!(entry, end);
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches("assets/estilo.css").count(), 2);
        assert!(text.find("assets/estilo.css") < text.find("index.html"));
        assert!(text.contains("<h1>Alta</h1>"));
        assert!(text.contains("shared copy"));
    }
}
