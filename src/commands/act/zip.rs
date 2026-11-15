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

pub(super) fn zip(folder: &Path, into: &Path) -> Result<PathBuf> {
    let mut names = Vec::new();
    walk(folder, Path::new(""), &mut names)?;
    names.sort();
    let (mut body, mut central) = (Vec::new(), Vec::new());
    for name in &names {
        let data = std::fs::read(folder.join(name))
            .with_context(|| format!("cannot read {}", folder.join(name).display()))?;
        let name = name.to_string_lossy();
        let offset = too_big(u32::try_from(body.len()), folder)?;
        let mut fields = Vec::with_capacity(26);
        for half in [VERSION, UTF8_NAMES, STORED, MIDNIGHT, JANUARY_1980] {
            fields.extend(half.to_le_bytes());
        }
        let size = too_big(u32::try_from(data.len()), folder)?;
        for word in [crc32(&data), size, size] {
            fields.extend(word.to_le_bytes());
        }
        fields.extend(too_big(u16::try_from(name.len()), folder)?.to_le_bytes());
        fields.extend(0_u16.to_le_bytes());
        body.extend(LOCAL.to_le_bytes());
        body.extend(&fields);
        body.extend(name.as_bytes());
        body.extend(&data);
        central.extend(CENTRAL.to_le_bytes());
        central.extend(VERSION.to_le_bytes());
        central.extend(&fields);
        central.extend([0; 10]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let count = too_big(u16::try_from(names.len()), folder)?;
    let start = too_big(u32::try_from(body.len()), folder)?;
    let length = too_big(u32::try_from(central.len()), folder)?;
    body.append(&mut central);
    body.extend(END.to_le_bytes());
    body.extend([0; 4]);
    body.extend(count.to_le_bytes());
    body.extend(count.to_le_bytes());
    body.extend(length.to_le_bytes());
    body.extend(start.to_le_bytes());
    body.extend([0; 2]);
    std::fs::create_dir_all(into).with_context(|| format!("cannot create {}", into.display()))?;
    let mut name = folder
        .file_name()
        .with_context(|| format!("{} has no name to give the zip", folder.display()))?
        .to_owned();
    name.push(".zip");
    let archive = into.join(name);
    if std::fs::read(&archive).ok().as_deref() != Some(body.as_slice()) {
        std::fs::write(&archive, body)
            .with_context(|| format!("cannot write {}", archive.display()))?;
    }
    Ok(archive)
}

fn walk(root: &Path, at: &Path, names: &mut Vec<PathBuf>) -> Result<()> {
    let folder = root.join(at);
    for entry in
        std::fs::read_dir(&folder).with_context(|| format!("cannot read {}", folder.display()))?
    {
        let entry = entry?;
        let name = at.join(entry.file_name());
        let kind = entry.file_type()?;
        if !kind.is_dir() && !kind.is_file() {
            continue;
        }
        if kind.is_dir() {
            walk(root, &name, names)?;
        } else {
            names.push(name);
        }
    }
    Ok(())
}

fn too_big<T, E>(size: Result<T, E>, folder: &Path) -> Result<T> {
    size.map_err(|_| anyhow::anyhow!("{} is too big for a zip", folder.display()))
}

fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0, |crc, &byte| {
        (0..8).fold(crc ^ u32::from(byte), |crc, _| {
            (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_folder_zips_with_its_structure() {
        let root = std::env::temp_dir().join(format!("iu2u-zip-test-{}", std::process::id()));
        let folder = root.join("alta-v1.2");
        std::fs::create_dir_all(folder.join("assets")).unwrap();
        std::fs::write(folder.join("index.html"), "<h1>Alta</h1>").unwrap();
        std::fs::write(folder.join("assets/estilo.css"), "body{}").unwrap();
        std::os::unix::fs::symlink(folder.join("assets"), folder.join("linked")).unwrap();
        std::os::unix::net::UnixListener::bind(folder.join("s.sock")).unwrap();
        let archive = zip(&folder, &root.join("out")).unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(archive.file_name().unwrap(), "alta-v1.2.zip");
        assert_eq!(bytes[..4], LOCAL.to_le_bytes());
        let end = bytes.len() - 22;
        assert_eq!(bytes[end..end + 4], END.to_le_bytes());
        assert_eq!(bytes[end + 10..end + 12], 2_u16.to_le_bytes());
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches("assets/estilo.css").count(), 2);
        assert!(text.find("assets/estilo.css") < text.find("index.html"));
        assert!(text.contains("<h1>Alta</h1>"));
    }
}
