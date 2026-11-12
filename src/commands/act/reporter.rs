use std::fmt::Write as _;
use std::io::{self, ErrorKind, Write as _};
use std::time::Instant;

use anstream::ColorChoice;
use anstyle::{AnsiColor, Color, Style};
use serde_json::Value;

const PASSED: Style = AnsiColor::Green.on_default();
const FAILED: Style = AnsiColor::Red.on_default();
const BADGE: Style = AnsiColor::White
    .on_default()
    .bg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();
const DIM: Style = Style::new().dimmed();
const BOLD: Style = Style::new().bold();
const LINES_BEFORE: usize = 2;
const LINES_AFTER: usize = 1;
const RULE: &str = "⎯⎯⎯⎯⎯⎯⎯";

#[derive(Default)]
struct Tally {
    passed: usize,
    failed: usize,
}

pub struct Reporter {
    out: anstream::Stdout,
    json: bool,
    started: Instant,
    scripts: Tally,
    steps: Tally,
    model_calls: u64,
    failures: Vec<String>,
    error: Option<io::Error>,
}

impl Reporter {
    pub fn new(json: bool) -> Self {
        Self {
            out: anstream::AutoStream::new(
                io::stdout(),
                if json {
                    ColorChoice::AlwaysAnsi
                } else {
                    ColorChoice::Auto
                },
            ),
            json,
            started: Instant::now(),
            scripts: Tally::default(),
            steps: Tally::default(),
            model_calls: 0,
            failures: Vec::new(),
            error: None,
        }
    }

    pub fn start(&mut self, script: &str) {
        if !self.json {
            self.emit(&format!("{BOLD}▶ {}{BOLD:#}\n", escaped(script)));
        }
    }

    pub fn step(&mut self, step: &Value) {
        if self.json {
            return;
        }
        let passed = step["status"] == "passed";
        let mut text = String::new();
        writeln!(
            text,
            "  {} {}{DIM}{}{DIM:#}",
            mark(passed),
            field(&step["step"]),
            duration(step)
        )
        .ok();
        for action in step["actions"].as_array().into_iter().flatten() {
            writeln!(text, "    {DIM}› {}{DIM:#}", field(action)).ok();
        }
        if let Some(detail) = step["detail"].as_str() {
            let style = if passed { DIM } else { FAILED };
            writeln!(text, "    {style}→ {}{style:#}", escaped(detail)).ok();
        }
        self.emit(&text);
    }

    pub fn finish(&mut self, report: &Value, script: Option<&str>) {
        if self.json {
            self.emit(&format!("{report}\n"));
            return;
        }
        let name = field(&report["script"]);
        let passed = report["status"] == "passed";
        let steps = report["steps"].as_array().map_or(&[][..], Vec::as_slice);
        for step in steps {
            count(&mut self.steps, step["status"] == "passed");
        }
        count(&mut self.scripts, passed);
        let calls = report["model_calls"].as_u64().unwrap_or_default();
        self.model_calls += calls;
        let failing = steps.iter().find(|step| step["status"] != "passed");
        let mut text = String::new();
        if let (None, Some(error)) = (failing, report["error"].as_str()) {
            writeln!(text, "  {FAILED}→ {}{FAILED:#}", escaped(error)).ok();
        }
        let calls = match calls {
            0 => String::new(),
            1 => " · 1 model call".into(),
            calls => format!(" · {calls} model calls"),
        };
        writeln!(
            text,
            "{} {name}{DIM}{}{calls}{DIM:#}\n",
            mark(passed),
            duration(report)
        )
        .ok();
        self.emit(&text);
        if !passed {
            self.failures.push(failure(&name, report, failing, script));
        }
    }

    pub fn summary(&mut self) {
        if self.json {
            return;
        }
        let mut text = String::new();
        if !self.failures.is_empty() {
            let count = self.failures.len();
            writeln!(text, "{FAILED}{RULE} Failures {count} {RULE}{FAILED:#}\n").ok();
            for failure in &self.failures {
                writeln!(text, "{failure}").ok();
            }
            writeln!(text, "{FAILED}{RULE}{RULE}{RULE}{FAILED:#}\n").ok();
        }
        writeln!(text, "    {DIM}Scripts{DIM:#}  {}", tally(&self.scripts)).ok();
        writeln!(text, "      {DIM}Steps{DIM:#}  {}", tally(&self.steps)).ok();
        writeln!(text, "{DIM}Model calls{DIM:#}  {}", self.model_calls).ok();
        writeln!(
            text,
            "   {DIM}Duration{DIM:#}  {:.1}s",
            self.started.elapsed().as_secs_f64()
        )
        .ok();
        self.emit(&text);
    }

    pub fn broken(&self) -> bool {
        self.error.is_some()
    }

    pub fn close(self) -> io::Result<()> {
        match self.error {
            Some(error) if error.kind() != ErrorKind::BrokenPipe => Err(error),
            _ => Ok(()),
        }
    }

    fn emit(&mut self, text: &str) {
        if self.error.is_none() {
            self.error = self.out.write_all(text.as_bytes()).err();
        }
    }
}

fn count(tally: &mut Tally, passed: bool) {
    if passed {
        tally.passed += 1;
    } else {
        tally.failed += 1;
    }
}

fn mark(passed: bool) -> String {
    if passed {
        format!("{PASSED}✓{PASSED:#}")
    } else {
        format!("{FAILED}✗{FAILED:#}")
    }
}

fn duration(report: &Value) -> String {
    report["seconds"]
        .as_f64()
        .map_or_else(String::new, |seconds| format!(" {seconds:.1}s"))
}

fn tally(tally: &Tally) -> String {
    let mut parts = Vec::new();
    if tally.failed > 0 {
        let style = FAILED.bold();
        parts.push(format!("{style}{} failed{style:#}", tally.failed));
    }
    if tally.passed > 0 || tally.failed == 0 {
        let style = PASSED.bold();
        parts.push(format!("{style}{} passed{style:#}", tally.passed));
    }
    format!(
        "{} {DIM}({}){DIM:#}",
        parts.join(&format!(" {DIM}|{DIM:#} ")),
        tally.passed + tally.failed
    )
}

fn failure(name: &str, report: &Value, failing: Option<&Value>, script: Option<&str>) -> String {
    let Some(step) = failing else {
        let error = field(&report["error"]);
        return format!("{BADGE} FAIL {BADGE:#} {name}\n{FAILED}{error}{FAILED:#}\n");
    };
    let line = step["line"]
        .as_u64()
        .and_then(|line| usize::try_from(line).ok())
        .unwrap_or_default();
    let mut text = format!(
        "{BADGE} FAIL {BADGE:#} {name}:{line} {DIM}›{DIM:#} {}\n",
        field(&step["step"])
    );
    if let Some(detail) = step["detail"].as_str() {
        writeln!(text, "{FAILED}{}{FAILED:#}", escaped(detail)).ok();
    }
    if let Some(script) = script {
        writeln!(text).ok();
        text.push_str(&frame(script, line));
    }
    text
}

fn frame(script: &str, line: usize) -> String {
    let lines: Vec<&str> = script.lines().collect();
    let first = line.saturating_sub(LINES_BEFORE + 1);
    let last = (line + LINES_AFTER).min(lines.len());
    let width = last.to_string().len();
    let mut text = String::new();
    for (index, content) in lines.iter().enumerate().take(last).skip(first) {
        let number = index + 1;
        let pointer = if number == line {
            format!("{FAILED}>{FAILED:#}")
        } else {
            " ".into()
        };
        let content = escaped(content);
        writeln!(text, "  {pointer} {DIM}{number:>width$} |{DIM:#} {content}").ok();
    }
    text
}

fn field(value: &Value) -> String {
    escaped(value.as_str().unwrap_or_default())
}

fn escaped(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() && character != '\t' {
            write!(shown, "{}", character.escape_default()).ok();
        } else {
            shown.push(character);
        }
    }
    shown
}
