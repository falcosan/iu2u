use std::ops::Range;

pub(super) struct Excerpt {
    pub(super) file: String,
    pub(super) first_line: usize,
    pub(super) last_line: usize,
    pub(super) text: String,
}

impl Excerpt {
    pub(super) fn lines(&self) -> String {
        format!("{}-{}", self.first_line, self.last_line)
    }

    pub(super) fn state(&self) -> String {
        format!("{}:{}\n{}", self.file, self.lines(), self.text)
    }
}

pub(super) fn excerpts(file: &str, text: &str, budget: usize) -> Vec<Excerpt> {
    let lines: Vec<&str> = text.lines().collect();
    let mut sizes = vec![0];
    for line in &lines {
        sizes.push(sizes[sizes.len() - 1] + line.chars().filter(|c| !c.is_whitespace()).count());
    }
    let indents = lines
        .iter()
        .map(|line| line.len() - line.trim_start().len())
        .collect();
    let text = Text {
        file,
        indents,
        lines,
        sizes,
        budget: budget.max(1),
    };
    let mut excerpts = Vec::new();
    text.chunk(0..text.lines.len(), &mut excerpts);
    excerpts
}

struct Text<'a> {
    file: &'a str,
    lines: Vec<&'a str>,
    indents: Vec<usize>,
    sizes: Vec<usize>,
    budget: usize,
}

impl Text<'_> {
    fn chunk(&self, range: Range<usize>, excerpts: &mut Vec<Excerpt>) {
        match self.size(&range) {
            0 => return,
            size if size <= self.budget => return excerpts.push(self.excerpt(range)),
            _ => {}
        }
        let parts = self.split(&range);
        if parts.len() == 1 {
            let line = range.clone().find(|&line| !self.blank(line));
            return excerpts.extend(self.pieces(line.unwrap_or(range.start)));
        }
        let mut merged: Option<Range<usize>> = None;
        for part in parts {
            match merged.take() {
                Some(open) if self.size(&(open.start..part.end)) <= self.budget => {
                    merged = Some(open.start..part.end);
                }
                open => {
                    excerpts.extend(open.map(|open| self.excerpt(open)));
                    if (1..=self.budget).contains(&self.size(&part)) {
                        merged = Some(part);
                    } else {
                        self.chunk(part, excerpts);
                    }
                }
            }
        }
        excerpts.extend(merged.map(|open| self.excerpt(open)));
    }

    fn split(&self, range: &Range<usize>) -> Vec<Range<usize>> {
        let opens = |line: usize| !self.blank(line) && !self.closes(line);
        let outer = range
            .clone()
            .filter(|&line| opens(line))
            .map(|line| self.indent(line))
            .min();
        let outermost = |line: usize| opens(line) && Some(self.indent(line)) == outer;
        let blocks = self.cut(range, |line| outermost(line) && self.blank(line - 1));
        if blocks.len() > 1 {
            return blocks;
        }
        let statements = self.cut(range, outermost);
        if statements.len() > 1 {
            return statements;
        }
        if outermost(range.start) && range.len() > 1 {
            let body = range.start + 1..range.end;
            return std::iter::once(range.start..body.start)
                .chain(self.split(&body))
                .collect();
        }
        self.cut(range, |line| !self.blank(line))
    }

    fn cut(&self, range: &Range<usize>, starts: impl Fn(usize) -> bool) -> Vec<Range<usize>> {
        let mut parts = Vec::new();
        let mut start = range.start;
        for line in range.start + 1..range.end {
            if starts(line) && self.size(&(start..line)) > 0 {
                parts.push(start..line);
                start = line;
            }
        }
        parts.push(start..range.end);
        parts
    }

    fn pieces(&self, line: usize) -> Vec<Excerpt> {
        let mut pieces = vec![String::new()];
        let mut size = 0;
        for character in self.lines[line].chars() {
            if size == self.budget && !character.is_whitespace() {
                pieces.push(String::new());
                size = 0;
            }
            size += usize::from(!character.is_whitespace());
            pieces.last_mut().unwrap().push(character);
        }
        pieces
            .into_iter()
            .map(|text| self.excerpt_text(line..line + 1, text))
            .collect()
    }

    fn excerpt(&self, range: Range<usize>) -> Excerpt {
        let text = self.lines[range.clone()].join("\n");
        self.excerpt_text(range, text)
    }

    fn excerpt_text(&self, range: Range<usize>, text: String) -> Excerpt {
        Excerpt {
            file: self.file.to_string(),
            first_line: range.start + 1,
            last_line: range.end,
            text,
        }
    }

    fn size(&self, range: &Range<usize>) -> usize {
        self.sizes[range.end] - self.sizes[range.start]
    }

    fn indent(&self, line: usize) -> usize {
        self.indents[line]
    }

    fn blank(&self, line: usize) -> bool {
        self.indents[line] == self.lines[line].len()
    }

    fn closes(&self, line: usize) -> bool {
        self.lines[line][self.indents[line]..].starts_with([')', ']', '}'])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str, budget: usize) -> Vec<String> {
        excerpts("f", text, budget)
            .iter()
            .map(Excerpt::lines)
            .collect()
    }

    #[test]
    fn a_text_within_budget_stays_whole() {
        assert_eq!(spans("fn a() {\n    x();\n}\n", 100), ["1-3"]);
        assert!(excerpts("f", "\n  \n", 100).is_empty());
        assert!(
            excerpts("f", "\nabcdef", 3)
                .iter()
                .all(|part| part.lines() == "2-2")
        );
    }

    #[test]
    fn blocks_split_at_blank_lines_and_never_mid_function() {
        let text =
            "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n\nfn c() {\n    three();\n}";
        assert_eq!(spans(text, 30), ["1-8", "9-11"]);
        assert_eq!(spans(text, 18), ["1-4", "5-8", "9-11"]);
    }

    #[test]
    fn an_oversized_block_keeps_its_header_with_the_first_statements() {
        let text = "fn big() {\n    one();\n    two();\n    three();\n    four();\n}";
        assert_eq!(spans(text, 20), ["1-3", "4-6"]);
    }

    #[test]
    fn a_line_longer_than_the_budget_is_cut_by_characters() {
        let parts = excerpts("f", &"ab ".repeat(10), 7);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part.lines() == "1-1"));
        assert!(
            parts
                .iter()
                .all(|part| part.text.chars().filter(|c| !c.is_whitespace()).count() <= 7)
        );
        assert_eq!(
            parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<String>(),
            "ab ".repeat(10)
        );
    }

    #[test]
    fn excerpts_cover_every_line_once_and_respect_the_budget() {
        let text: String = (0..60)
            .map(|index| match index % 6 {
                0 => format!("fn f{index}() {{\n"),
                5 => "}\n\n".to_string(),
                _ => format!("    let v{index} = compute({index});\n"),
            })
            .collect();
        let parts = excerpts("f", &text, 80);
        let mut next = 1;
        for part in &parts {
            assert_eq!(part.first_line, next);
            assert!(part.text.chars().filter(|c| !c.is_whitespace()).count() <= 80);
            next = part.last_line + 1;
        }
        assert_eq!(next, text.lines().count() + 1);
    }
}
