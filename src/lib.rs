pub mod commands;
pub mod config;
pub mod hooks;
pub mod mcp;
pub mod model;
pub mod tools;

pub(crate) fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut partial = path.as_os_str().to_owned();
    partial.push(format!(".{}.tmp", std::process::id()));
    let partial = std::path::PathBuf::from(partial);
    replace(&path, &partial, bytes).inspect_err(|_| {
        std::fs::remove_file(&partial).ok();
    })
}

fn replace(path: &std::path::Path, partial: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(partial)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(partial, path)
}

pub(crate) fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_counts_characters_not_bytes() {
        assert_eq!(clip("ñandú", 3), "ñan…");
        assert_eq!(clip("abc", 3), "abc");
    }
}
