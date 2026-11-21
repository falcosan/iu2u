use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const TAIL_LINES: usize = 20;
pub(crate) const KEPT_BYTES: usize = 1 << 20;

pub(crate) fn tail(log: &Path) -> String {
    let (bytes, _) = read_tail(log, 0);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

pub(crate) fn read_tail(path: &Path, shown_from: u64) -> (Vec<u8>, bool) {
    let (mut bytes, mut cut) = (Vec::new(), false);
    if let Ok(mut file) = std::fs::File::open(path)
        && let Ok(length) = file.metadata().map(|meta| meta.len())
    {
        let kept = u64::try_from(KEPT_BYTES).unwrap_or(u64::MAX);
        let from = shown_from.max(length.saturating_sub(kept));
        cut = from > shown_from;
        if file.seek(SeekFrom::Start(from)).is_ok() {
            file.take(kept).read_to_end(&mut bytes).ok();
        }
    }
    if cut {
        bytes = from_a_line(bytes);
    }
    (bytes, cut)
}

pub(crate) fn from_a_line(mut bytes: Vec<u8>) -> Vec<u8> {
    let start = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(0, |at| at + 1);
    bytes.drain(..start);
    bytes
}
