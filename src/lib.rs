pub mod commands;
pub mod hooks;
pub mod mcp;
pub mod model;
pub mod tools;

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
