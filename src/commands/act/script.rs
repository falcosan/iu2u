use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::decide::Input;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Step {
    Open(String),
    App(String),
    Do(String),
    Expect(String),
    See(String),
    Request(Request),
    Route(Route),
    Release(String),
    Back,
    Forward,
    Reload,
    SeeNo(String),
    Run(String),
    Start(Start),
    Logs(String),
    Inject(String),
    Extract(String, String),
    Wait(Duration),
    ClockInstall,
    ClockFreeze,
    ClockRun(Duration),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Start {
    pub(super) name: String,
    pub(super) command: String,
    pub(super) at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Route {
    pub(super) pattern: String,
    pub(super) once: bool,
    pub(super) reply: Reply,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Reply {
    To(String),
    Answer(u16, Option<Part>),
    Hold,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Request {
    pub(super) method: String,
    pub(super) url: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Option<Body>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Body {
    Json(Part),
    Form(Vec<(String, Part)>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Part {
    Text(String),
    File(String),
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Script {
    pub(super) steps: Vec<(usize, Step, Option<Duration>)>,
    pub(super) inputs: Vec<Input>,
    pub(super) files: Vec<Files>,
    pub(super) commands: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Files {
    pub(super) key: String,
    pub(super) paths: Vec<String>,
    pub(super) zip: bool,
}

impl Step {
    pub(super) fn text_within(&self, within: Option<Duration>) -> String {
        match within {
            Some(within) => format!("{} within {}s", self.text(), within.as_secs_f64()),
            None => self.text(),
        }
    }

    pub(super) fn text(&self) -> String {
        match self {
            Self::Open(url) => format!("open {url}"),
            Self::App(name) => format!("app {name}"),
            Self::Do(goal) => format!("do {goal}"),
            Self::Expect(claim) => format!("expect {claim}"),
            Self::See(text) => format!("see {text:?}"),
            Self::Request(request) => format!("request {} {}", request.method, request.url),
            Self::Route(route) => {
                let mut text = format!("route {}", route.pattern);
                if route.once {
                    text.push_str(" once");
                }
                match &route.reply {
                    Reply::To(url) => write!(text, " to {url}"),
                    Reply::Answer(status, None) => write!(text, " answer {status}"),
                    Reply::Answer(status, Some(Part::Text(body))) => {
                        write!(text, " answer {status} {body}")
                    }
                    Reply::Answer(status, Some(Part::File(path))) => {
                        write!(text, " answer {status} file {path}")
                    }
                    Reply::Hold => write!(text, " hold"),
                }
                .ok();
                text
            }
            Self::Release(pattern) => format!("release {pattern}"),
            Self::Back => "back".into(),
            Self::Forward => "forward".into(),
            Self::Reload => "reload".into(),
            Self::SeeNo(text) => format!("see no {text:?}"),
            Self::Run(command) => format!("run {command}"),
            Self::Start(start) => match &start.at {
                Some(at) => format!("start {} = {} at {at}", start.name, start.command),
                None => format!("start {} = {}", start.name, start.command),
            },
            Self::Logs(name) => format!("logs {name}"),
            Self::Inject(source) => format!("inject {source}"),
            Self::Extract(name, what) => format!("extract {name} = {what}"),
            Self::Wait(duration) => format!("wait {}s", duration.as_secs_f64()),
            Self::ClockInstall => "clock install".into(),
            Self::ClockFreeze => "clock freeze".into(),
            Self::ClockRun(duration) => format!("clock run {}s", duration.as_secs_f64()),
        }
    }
}

pub(super) fn inputs(goal: &str, given: &[Input]) -> Vec<Input> {
    let quoted = goal
        .split('"')
        .skip(1)
        .step_by(2)
        .take(goal.matches('"').count() / 2)
        .filter(|text| !text.is_empty())
        .map(|text| Input {
            key: format!("\"{text}\""),
            text: text.into(),
            secret: false,
        });
    let mut seen = HashSet::new();
    quoted
        .chain(given.iter().cloned())
        .filter(|input| seen.insert(input.text.clone()))
        .collect()
}

const MAX_USES: usize = 8;
const TIME_UNITS: [&str; 11] = [
    "ms", "s", "m", "sec", "secs", "second", "seconds", "min", "mins", "minute", "minutes",
];

#[cfg(test)]
pub(super) fn parse(text: &str) -> Result<Script> {
    parse_in(text, Path::new("."))
}

pub(super) fn parse_in(text: &str, base: &Path) -> Result<Script> {
    let mut script = Script::default();
    read(text, base, &mut script, 0, None)?;
    Ok(script)
}

fn read(
    text: &str,
    base: &Path,
    script: &mut Script,
    depth: usize,
    at_line: Option<usize>,
) -> Result<()> {
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let at = at_line.unwrap_or(number);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if raw.starts_with([' ', '\t'])
            && let Some((_, Step::Request(request), _)) = script.steps.last_mut()
        {
            detail(request, line).with_context(|| format!("line {number}"))?;
            continue;
        }
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let (rest, waits) = match verb {
            "see" | "expect" | "run" | "start" => {
                within(rest.trim(), matches!(verb, "run" | "start"))
                    .with_context(|| format!("line {number}"))?
            }
            _ => (rest.trim(), None),
        };
        if verb == "see"
            && let Some(absent) = rest.strip_prefix("no ").map(str::trim_start)
            && absent.starts_with('"')
        {
            anyhow::ensure!(
                absent.len() > 1 && absent.ends_with('"'),
                "line {number}: write it as: see no \"text\", or see no \"text\" within 30s"
            );
            let absent = unquoted(absent).trim();
            anyhow::ensure!(!absent.is_empty(), "line {number}: see no needs some text");
            script.steps.push((at, Step::SeeNo(absent.into()), waits));
            continue;
        }
        anyhow::ensure!(
            verb != "see" || !rest.starts_with("no "),
            "line {number}: write it as: see no \"text\" for text that must be gone, or see \"no text\" for text that starts with no"
        );
        anyhow::ensure!(
            verb != "see" || !rest.starts_with('"') || (rest.len() > 1 && rest.ends_with('"')),
            "line {number}: write it as: see \"text\", or see \"text\" within 30s"
        );
        let rest = if verb == "see" {
            unquoted(rest).trim()
        } else {
            rest
        }
        .to_string();
        let navigation = match verb {
            "back" => Some(Step::Back),
            "forward" => Some(Step::Forward),
            "reload" => Some(Step::Reload),
            _ => None,
        };
        if let Some(step) = navigation {
            anyhow::ensure!(
                rest.is_empty(),
                "line {number}: {verb:?} takes nothing after it"
            );
            script.steps.push((at, step, None));
            continue;
        }
        if rest.is_empty() {
            bail!("line {number}: {verb:?} needs some text after it");
        }
        let step = match verb {
            "open" => Step::Open(rest),
            "app" => Step::App(rest),
            "do" => Step::Do(rest),
            "expect" => Step::Expect(rest),
            "see" => Step::See(rest),
            "request" => Step::Request(request(&rest).with_context(|| format!("line {number}"))?),
            "route" => Step::Route(route(&rest).with_context(|| format!("line {number}"))?),
            "release" => Step::Release(rest),
            "run" => Step::Run(rest),
            "start" => Step::Start(start(&rest).with_context(|| format!("line {number}"))?),
            "logs" => Step::Logs(rest),
            "wait" => Step::Wait(duration(&rest).with_context(|| {
                format!("line {number}: write it as: wait 30s, wait 500ms or wait 2m")
            })?),
            "inject" => Step::Inject(rest),
            "clock" if rest == "install" => Step::ClockInstall,
            "clock" if rest == "freeze" => Step::ClockFreeze,
            "clock" => Step::ClockRun(rest.strip_prefix("run ").and_then(duration).with_context(
                || {
                    format!(
                        "line {number}: write it as: clock install, clock freeze or clock run 3s"
                    )
                },
            )?),
            "extract" => {
                let (name, what) = rest
                    .split_once('=')
                    .map(|(name, what)| (name.trim(), what.trim()))
                    .filter(|(name, what)| !what.is_empty() && is_name(name))
                    .with_context(|| {
                        format!("line {number}: write it as: extract name = what to take from the screen")
                    })?;
                Step::Extract(name.into(), what.into())
            }
            "use" => {
                anyhow::ensure!(
                    depth < MAX_USES,
                    "line {number}: use goes more than {MAX_USES} files deep"
                );
                let path = base.join(&rest);
                let used = std::fs::read_to_string(&path)
                    .with_context(|| format!("line {number}: cannot read {}", path.display()))?;
                read(
                    &used,
                    path.parent().unwrap_or(base),
                    script,
                    depth + 1,
                    Some(at),
                )
                .with_context(|| format!("line {number}: in {}", path.display()))?;
                continue;
            }
            "with" if command_value(&rest).is_some() => {
                let (key, command) = command_value(&rest).unwrap_or_default();
                anyhow::ensure!(
                    !key.is_empty() && !key.contains('"'),
                    "line {number}: write it as: with name = run command"
                );
                if script.commands.contains(&(key.clone(), command.clone())) {
                    continue;
                }
                anyhow::ensure!(
                    !taken(script, &key),
                    "line {number}: {key:?} already has a value"
                );
                script.commands.push((key, command));
                continue;
            }
            "with" => {
                let files = given_files(&rest).with_context(|| format!("line {number}"))?;
                let input = match files {
                    Some(_) => None,
                    None => Some(input(&rest).with_context(|| format!("line {number}"))?),
                };
                let key = files
                    .as_ref()
                    .map(|files| &files.key)
                    .or(input.as_ref().map(|input| &input.key))
                    .cloned()
                    .unwrap_or_default();
                if files
                    .as_ref()
                    .is_some_and(|files| script.files.contains(files))
                    || input
                        .as_ref()
                        .is_some_and(|input| script.inputs.contains(input))
                {
                    continue;
                }
                anyhow::ensure!(
                    !taken(script, &key),
                    "line {number}: {key:?} already has a value"
                );
                script.files.extend(files);
                script.inputs.extend(input);
                continue;
            }
            _ => bail!(
                "line {number}: unknown step {verb:?}; use open, app, with, do, expect, see, request, route, release, back, forward, reload, run, start, logs, inject, extract, wait, clock or use"
            ),
        };
        script.steps.push((at, step, waits));
    }
    Ok(())
}

fn command_value(definition: &str) -> Option<(String, String)> {
    let (key, value) = definition.split_once('=')?;
    let command = value.trim().strip_prefix("run ")?.trim();
    (!command.is_empty()).then(|| (key.trim().to_owned(), command.to_owned()))
}

fn start(definition: &str) -> Result<Start> {
    let (name, rest) = definition
        .split_once(" = ")
        .map(|(name, rest)| (name.trim(), rest.trim()))
        .filter(|(name, rest)| {
            !rest.is_empty()
                && !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .context("write it as: start name = command [at address]")?;
    let (command, at) = match rest.rsplit_once(" at ") {
        Some((command, at))
            if (at.trim().starts_with("http") || at.trim().starts_with('$'))
                && !command.trim().is_empty() =>
        {
            (command.trim(), Some(at.trim().to_owned()))
        }
        _ => (rest, None),
    };
    Ok(Start {
        name: name.into(),
        command: command.into(),
        at,
    })
}

pub(super) fn expand(text: &str, given: &[Input], read: &mut Vec<Input>) -> Result<String> {
    substitute(text, |name, _| value_of(name, given, read))
}

pub(super) fn expand_json(text: &str, given: &[Input], read: &mut Vec<Input>) -> Result<String> {
    let quoted = in_strings(text);
    substitute(text, |name, at| {
        let value = value_of(name, given, read)?;
        Ok(if quoted.contains(&at) {
            let json = serde_json::to_string(&value)?;
            json[1..json.len() - 1].to_owned()
        } else {
            value
        })
    })
}

fn in_strings(json: &str) -> HashSet<usize> {
    let (mut inside, mut escaped, mut dollars) = (false, false, HashSet::new());
    for (at, character) in json.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' if inside => escaped = true,
            '"' => inside = !inside,
            '$' if inside => {
                dollars.insert(at);
            }
            _ => {}
        }
    }
    dollars
}

fn value_of(name: &str, given: &[Input], read: &mut Vec<Input>) -> Result<String> {
    if let Some(value) = given
        .iter()
        .chain(read.iter())
        .find(|value| value.key == name)
    {
        return Ok(value.text.clone());
    }
    let text = std::env::var(name)
        .ok()
        .filter(|text| !text.is_empty())
        .with_context(|| {
            format!("${name} has no value; give it with `with {name} = ...` or set the environment variable {name}")
        })?;
    read.push(Input {
        key: name.into(),
        text: text.clone(),
        secret: true,
    });
    Ok(text)
}

pub(super) fn references(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    substitute(text, |name, _| {
        names.push(name.to_owned());
        Ok(String::new())
    })
    .ok();
    names
}

fn substitute(text: &str, mut value: impl FnMut(&str, usize) -> Result<String>) -> Result<String> {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        shown.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(after) = after.strip_prefix('$') {
            shown.push('$');
            rest = after;
            continue;
        }
        let length = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..length];
        if !is_name(name) {
            shown.push('$');
            rest = after;
            continue;
        }
        shown.push_str(&value(name, text.len() - rest.len() + at)?);
        rest = &after[length..];
    }
    shown.push_str(rest);
    Ok(shown)
}

fn route(definition: &str) -> Result<Route> {
    const USAGE: &str = "write it as: route <address> [once] to <address>, route <address> [once] answer <status> [<body> | file <path>], or route <address> [once] hold";
    let (pattern, rest) = definition.split_once(char::is_whitespace).context(USAGE)?;
    let rest = rest.trim_start();
    let (once, rest) = match rest.strip_prefix("once") {
        Some(after) if after.starts_with(char::is_whitespace) => (true, after.trim_start()),
        _ => (false, rest),
    };
    let (action, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let argument = argument.trim();
    let reply = match action {
        "to" if !argument.is_empty() => Reply::To(argument.into()),
        "answer" => {
            let (status, body) = argument
                .split_once(char::is_whitespace)
                .unwrap_or((argument, ""));
            let status = status
                .parse::<u16>()
                .ok()
                .filter(|status| (200..=599).contains(status))
                .context("route ... answer needs a status from 200 to 599")?;
            let body = body.trim();
            let body = match body.strip_prefix("file ") {
                Some(path) if !path.trim().is_empty() => Some(Part::File(path.trim().into())),
                _ if body.is_empty() => None,
                _ => Some(Part::Text(body.into())),
            };
            Reply::Answer(status, body)
        }
        "hold" if argument.is_empty() => Reply::Hold,
        _ => bail!(USAGE),
    };
    Ok(Route {
        pattern: pattern.into(),
        once,
        reply,
    })
}

const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

fn request(definition: &str) -> Result<Request> {
    let (method, url) = definition
        .split_once(char::is_whitespace)
        .map(|(method, url)| (method, url.trim()))
        .filter(|(method, url)| METHODS.contains(method) && !url.is_empty())
        .with_context(|| {
            format!(
                "write it as: request METHOD address, with METHOD one of {}",
                METHODS.join(", ")
            )
        })?;
    Ok(Request {
        method: method.into(),
        url: url.into(),
        headers: Vec::new(),
        body: None,
    })
}

fn detail(request: &mut Request, line: &str) -> Result<()> {
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    match (word, &mut request.body) {
        ("json", None) if !rest.is_empty() => {
            request.body = Some(Body::Json(
                match rest.strip_prefix("file ").map(str::trim) {
                    Some(path) if !path.is_empty() => Part::File(path.into()),
                    _ => Part::Text(rest.into()),
                },
            ));
        }
        ("form", None | Some(Body::Form(_))) => {
            let (name, value) = rest
                .split_once('=')
                .map(|(name, value)| (name.trim(), value.trim()))
                .filter(|(name, _)| !name.is_empty())
                .context("write a form field as: form name = value, or form name = file path")?;
            let part = match value.strip_prefix("file ") {
                Some(path) if !path.trim().is_empty() => Part::File(path.trim().into()),
                _ => Part::Text(unquoted(value).into()),
            };
            match &mut request.body {
                Some(Body::Form(fields)) => fields.push((name.into(), part)),
                _ => request.body = Some(Body::Form(vec![(name.into(), part)])),
            }
        }
        ("json" | "form", Some(_)) => bail!("the request already has a body"),
        _ => {
            let (name, value) = line
                .split_once(':')
                .map(|(name, value)| (name.trim(), value.trim()))
                .filter(|(name, _)| {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
                })
                .context("under a request, write a header as name: value, a body as json {...} or json file path, or a form field as form name = value")?;
            request.headers.push((name.into(), value.into()));
        }
    }
    Ok(())
}

fn given_files(definition: &str) -> Result<Option<Files>> {
    let Some((key, value)) = definition.split_once('=') else {
        return Ok(None);
    };
    let value = value.trim();
    let (paths, zip) = match (value.strip_prefix("file "), value.strip_prefix("zip ")) {
        (Some(paths), _) => (paths, false),
        (_, Some(folder)) => (folder, true),
        _ => return Ok(None),
    };
    let key = key.trim();
    anyhow::ensure!(
        !key.is_empty() && !key.contains('"'),
        "write it as: with name = file path, or with name = zip folder"
    );
    let paths = paths
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::path::absolute(path)
                .map(|absolute| absolute.to_string_lossy().into_owned())
                .with_context(|| format!("cannot find {path}"))
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!paths.is_empty(), "write it as: with name = file path");
    anyhow::ensure!(!zip || paths.len() == 1, "with name = zip takes one folder");
    Ok(Some(Files {
        key: key.into(),
        paths,
        zip,
    }))
}

pub(super) fn named<'a>(goal: &str, files: &'a [Files]) -> Vec<&'a Files> {
    files
        .iter()
        .filter(|files| {
            goal.match_indices(&files.key).any(|(at, key)| {
                let word = |c: char| c.is_alphanumeric() || c == '_';
                let before = goal[..at].chars().next_back();
                let after = goal[at + key.len()..].chars().next();
                !before.is_some_and(word) && !after.is_some_and(word)
            })
        })
        .collect()
}

fn within(text: &str, command: bool) -> Result<(&str, Option<Duration>)> {
    let Some((before, after)) = text.rsplit_once(" within ") else {
        return Ok((text, None));
    };
    if let Some(waits) = duration(after) {
        return Ok((before.trim_end(), Some(waits)));
    }
    let unit = after
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ',')
        .trim_start();
    anyhow::ensure!(
        !after.starts_with(|c: char| c.is_ascii_digit())
            || !(TIME_UNITS.contains(&unit) || command && unit.is_empty()),
        "write it as: within 30s, within 500ms or within 2m"
    );
    Ok((text, None))
}

pub(super) fn is_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(|c: char| c.is_ascii_digit())
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn taken(script: &Script, key: &str) -> bool {
    script.inputs.iter().any(|given| given.key == key)
        || script.files.iter().any(|given| given.key == key)
        || script.commands.iter().any(|(given, _)| given == key)
}

fn duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (number, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let number = number.parse::<u64>().ok()?;
    match unit {
        "ms" => Some(Duration::from_millis(number)),
        "s" => Some(Duration::from_secs(number)),
        "m" => number.checked_mul(60).map(Duration::from_secs),
        _ => None,
    }
}

fn unquoted(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(text)
}

fn input(definition: &str) -> Result<Input> {
    let (key, value) = definition
        .split_once('=')
        .context("write it as: with name = value")?;
    let (key, value) = (key.trim(), value.trim());
    anyhow::ensure!(
        !key.is_empty() && !key.contains('"'),
        "write it as: with name = value"
    );
    match value.strip_prefix('$') {
        Some(variable) => Ok(Input {
            key: key.into(),
            text: std::env::var(variable)
                .ok()
                .filter(|text| !text.is_empty())
                .with_context(|| format!("set the environment variable {variable}"))?,
            secret: true,
        }),
        None => Ok(Input {
            key: key.into(),
            text: unquoted(value).into(),
            secret: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goal_names_a_file_only_by_its_whole_name() {
        let files = ["doc", "doc_v2"].map(|key| Files {
            key: key.into(),
            paths: vec![format!("{key}.pdf")],
            zip: false,
        });
        let keys = |goal: &str| -> Vec<String> {
            named(goal, &files)
                .iter()
                .map(|files| files.key.clone())
                .collect()
        };
        assert_eq!(keys("upload doc_v2"), ["doc_v2"]);
        assert_eq!(keys("upload doc, then doc_v2"), ["doc", "doc_v2"]);
    }

    #[test]
    fn a_script_reads_steps_values_and_secrets() {
        let script = parse(
            "# checkout\nopen http://shop.test\nwith email = qa@example.com\nwith password = $PATH\n\ndo sign in\nexpect the cart is empty\nsee \"Total\"\n",
        )
        .unwrap();
        assert_eq!(
            script
                .steps
                .iter()
                .map(|(line, step, _)| (*line, step.clone()))
                .collect::<Vec<_>>(),
            [
                (2, Step::Open("http://shop.test".into())),
                (6, Step::Do("sign in".into())),
                (7, Step::Expect("the cart is empty".into())),
                (8, Step::See("Total".into())),
            ]
        );
        assert_eq!(script.inputs[0].text, "qa@example.com");
        assert!(script.inputs[1].secret && !script.inputs[1].text.is_empty());
        let quoted =
            parse("with query = say \"hi\"\nwith height = \"6'2\"\"\nsee \"He said \"hi\"\"")
                .unwrap();
        let texts: Vec<_> = quoted
            .inputs
            .iter()
            .map(|input| input.text.as_str())
            .collect();
        assert_eq!(texts, ["say \"hi\"", "6'2\""]);
        assert_eq!(
            parse("with price = \"$5\"\ndo pay").unwrap().inputs[0].text,
            "$5"
        );
        assert_eq!(quoted.steps[0].1, Step::See("He said \"hi\"".into()));
    }

    #[test]
    fn quoted_text_in_a_goal_can_be_typed_once_beside_the_given_values() {
        let given = [
            Input {
                key: "email".into(),
                text: "qa@example.com".into(),
                secret: false,
            },
            Input {
                key: "shop".into(),
                text: "Shop".into(),
                secret: false,
            },
        ];
        let inputs = inputs("search for \"red shirt\" in \"Shop\" as \"email\"", &given);
        let texts: Vec<_> = inputs.iter().map(|input| input.text.as_str()).collect();
        assert_eq!(texts, ["red shirt", "Shop", "email", "qa@example.com"]);
        let keys: HashSet<_> = inputs.iter().map(|input| input.key.as_str()).collect();
        assert_eq!(keys.len(), inputs.len());
        assert!(super::inputs("set the height to 6'2\" tall", &[]).is_empty());
    }

    #[test]
    fn mistakes_name_their_line() {
        assert!(
            parse("open x\nclick y")
                .unwrap_err()
                .to_string()
                .contains("line 2")
        );
        assert!(parse("do").unwrap_err().to_string().contains("line 1"));
        assert!(parse("see \"\"").is_err() && parse("see \"  \"").is_err());
        assert!(
            parse("with email = a@x\nwith email = b@x\ndo sign in")
                .unwrap_err()
                .to_string()
                .contains("line 2")
        );
        assert!(parse("with \"city\" = Paris\ndo go").is_err());
        let quoted =
            parse("with reply = \"run late\"\nwith label = \"file not found\"\ndo go").unwrap();
        assert!(quoted.commands.is_empty() && quoted.files.is_empty());
        assert_eq!(
            quoted
                .inputs
                .iter()
                .map(|input| input.text.as_str())
                .collect::<Vec<_>>(),
            ["run late", "file not found"]
        );
        assert!(format!("{:#}", parse("with nothing").unwrap_err()).contains("line 1"));
        assert!(
            format!(
                "{:#}",
                parse("with key = $IU2U_UNSET_FOR_TEST").unwrap_err()
            )
            .contains("IU2U_UNSET_FOR_TEST")
        );
        for wrong in [
            "see \"Saved\" within 1.5s",
            "expect it is saved within 5 seconds",
            "run ./check.sh within 1,5m",
            "see no \"x\" within 400000000000000000m",
        ] {
            assert!(
                format!("{:#}", parse(wrong).unwrap_err())
                    .contains("line 1: write it as: within 30s"),
                "{wrong}"
            );
        }
        assert!(parse("expect the order ships within 24h").is_ok());
        assert!(parse("see \"Ships within 2 days\"").is_ok());
        for (wrong, written) in [
            (
                "see no Loading",
                "see no \"text\" for text that must be gone",
            ),
            ("see no \"Error\" within 1h", "see no \"text\""),
            ("see \"Saved\" witin 5s", "see \"text\""),
        ] {
            assert!(
                format!("{:#}", parse(wrong).unwrap_err())
                    .contains(&format!("line 1: write it as: {written}")),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_request_reads_its_headers_and_body_from_the_indented_lines_under_it() {
        let script = parse(
            "request POST /api/people\n  authorization: Bearer $TOKEN\n  json {\"role\": \"admin\"}\nrequest PUT /upload\n\tform name = \"Campus 2026\"\n\tform files = file fixtures/a.zip\nsee \"HTTP 201\"\n  do go home\n",
        )
        .unwrap();
        assert_eq!(
            script
                .steps
                .iter()
                .map(|(line, step, _)| (*line, step.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    1,
                    Step::Request(Request {
                        method: "POST".into(),
                        url: "/api/people".into(),
                        headers: vec![("authorization".into(), "Bearer $TOKEN".into())],
                        body: Some(Body::Json(Part::Text("{\"role\": \"admin\"}".into()))),
                    })
                ),
                (
                    4,
                    Step::Request(Request {
                        method: "PUT".into(),
                        url: "/upload".into(),
                        headers: Vec::new(),
                        body: Some(Body::Form(vec![
                            ("name".into(), Part::Text("Campus 2026".into())),
                            ("files".into(), Part::File("fixtures/a.zip".into())),
                        ])),
                    })
                ),
                (7, Step::See("HTTP 201".into())),
                (8, Step::Do("go home".into())),
            ]
        );
        for (wrong, says) in [
            ("request FETCH /x", "METHOD"),
            ("request GET", "METHOD"),
            ("request GET /x\n  json {}\n  json {}", "already has a body"),
            (
                "request GET /x\n  json {}\n  form a = b",
                "already has a body",
            ),
            ("request GET /x\n  not a header", "name: value"),
            ("request GET /x\n  form = b", "form name = value"),
        ] {
            let error = format!("{:#}", parse(wrong).unwrap_err());
            assert!(
                error.contains(says) && error.contains("line"),
                "{wrong}: {error}"
            );
        }
    }

    #[test]
    fn a_route_redirects_answers_or_holds_and_may_apply_once() {
        let routes = parse(
            "route https://login.example.com to http://127.0.0.1:5199\nroute */api/x* once answer 500 {\"error\": \"boom\"}\nroute */captures/* answer 404\nroute */api/y answer 200 file fixtures/y.json\nroute */diff hold\nrelease */diff\n",
        )
        .unwrap();
        let steps: Vec<_> = routes.steps.into_iter().map(|(_, step, _)| step).collect();
        assert_eq!(
            steps,
            [
                Step::Route(Route {
                    pattern: "https://login.example.com".into(),
                    once: false,
                    reply: Reply::To("http://127.0.0.1:5199".into()),
                }),
                Step::Route(Route {
                    pattern: "*/api/x*".into(),
                    once: true,
                    reply: Reply::Answer(500, Some(Part::Text("{\"error\": \"boom\"}".into()))),
                }),
                Step::Route(Route {
                    pattern: "*/captures/*".into(),
                    once: false,
                    reply: Reply::Answer(404, None),
                }),
                Step::Route(Route {
                    pattern: "*/api/y".into(),
                    once: false,
                    reply: Reply::Answer(200, Some(Part::File("fixtures/y.json".into()))),
                }),
                Step::Route(Route {
                    pattern: "*/diff".into(),
                    once: false,
                    reply: Reply::Hold,
                }),
                Step::Release("*/diff".into()),
            ]
        );
        assert_eq!(
            steps[1].text(),
            "route */api/x* once answer 500 {\"error\": \"boom\"}"
        );
        for (wrong, says) in [
            ("route */x answer 99", "200 to 599"),
            ("route */x answer", "200 to 599"),
            ("route */x hold now", "write it as"),
            ("route */x", "write it as"),
            ("route */x go http://a", "write it as"),
        ] {
            let error = format!("{:#}", parse(wrong).unwrap_err());
            assert!(
                error.contains(says) && error.contains("line 1"),
                "{wrong}: {error}"
            );
        }
    }

    #[test]
    fn see_and_expect_may_wait_longer_within_a_time() {
        let steps: Vec<(Step, Option<Duration>)> = parse(
            "see \"Publicado\" within 45s\nexpect the build failed within 2m\nsee \"Done within 5s\"\nexpect it ends within soon\nsee \"x\" within 1500ms\n",
        )
        .unwrap()
        .steps
        .into_iter()
        .map(|(_, step, within)| (step, within))
        .collect();
        assert_eq!(
            steps,
            [
                (Step::See("Publicado".into()), Some(Duration::from_secs(45))),
                (
                    Step::Expect("the build failed".into()),
                    Some(Duration::from_secs(120))
                ),
                (Step::See("Done within 5s".into()), None),
                (Step::Expect("it ends within soon".into()), None),
                (Step::See("x".into()), Some(Duration::from_millis(1500))),
            ]
        );
        assert_eq!(
            steps[0].0.text_within(steps[0].1),
            "see \"Publicado\" within 45s"
        );
        assert_eq!(steps[4].0.text_within(steps[4].1), "see \"x\" within 1.5s");
        for command in [
            "run ./check.sh within 5",
            "start api = ./api at http://api within 5",
        ] {
            let error = parse(command).err().map(|error| format!("{error:#}"));
            assert!(
                error
                    .as_deref()
                    .is_some_and(|error| error.contains("within 30s")),
                "{command}: {error:?}"
            );
        }
        assert_eq!(
            parse("expect the total is within 5").unwrap().steps[0].1,
            Step::Expect("the total is within 5".into())
        );
    }

    #[test]
    fn a_value_inside_a_json_string_is_escaped_and_one_outside_is_not() {
        let value = |key: &str, text: &str| Input {
            key: key.into(),
            text: text.into(),
            secret: false,
        };
        let given = [
            value("password", "Pa\"ss\\1"),
            value("count", "3"),
            value("tags", "[\"a\"]"),
        ];
        let json = expand_json(
            r#"{"password": "$password", "count": $count, "tags": $tags, "note": "a \"$count\" b"}"#,
            &given,
            &mut Vec::new(),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["password"], "Pa\"ss\\1");
        assert_eq!(parsed["count"], 3);
        assert_eq!(parsed["tags"][0], "a");
        assert_eq!(parsed["note"], "a \"3\" b");
    }

    #[test]
    fn a_used_file_counts_its_own_lines_and_can_be_used_twice() {
        let base = std::env::temp_dir().join(format!("iu2u-use-twice-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("sign-in.part"),
            "with email = qa@example.com\nwith token = run node token.mjs\ndo sign in as $email\n",
        )
        .unwrap();
        std::fs::write(
            base.join("typo.part"),
            "open http://hub.test\n\nclick Save\n",
        )
        .unwrap();
        let script = parse_in("use sign-in.part\nrun echo out\nuse sign-in.part\n", &base).unwrap();
        assert_eq!(script.inputs.len(), 1);
        assert_eq!(script.commands.len(), 1);
        assert_eq!(
            script
                .steps
                .iter()
                .map(|(line, _, _)| *line)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let error = |text: &str| format!("{:#}", parse_in(text, &base).unwrap_err());
        let typo = error("open http://hub.test\nuse typo.part\n");
        assert!(
            typo.contains("line 2: in") && typo.contains("line 3: unknown step \"click\""),
            "{typo}"
        );
        let clash = error("with email = other@example.com\nuse sign-in.part\n");
        std::fs::remove_dir_all(&base).ok();
        assert!(
            clash.contains("line 1: \"email\" already has a value"),
            "{clash}"
        );
    }

    #[test]
    fn commands_services_injections_absences_and_used_files_are_steps() {
        let base = std::env::temp_dir().join(format!("iu2u-use-{}", std::process::id()));
        std::fs::create_dir_all(base.join("parts")).unwrap();
        std::fs::write(
            base.join("parts/sign-in.e2e"),
            "open http://hub.test\nuse ../more.e2e\n",
        )
        .unwrap();
        std::fs::write(base.join("more.e2e"), "do sign in\n").unwrap();
        let script = parse_in(
            "with token = run node token.mjs viewer\nrun curl -s http://x | wc -l within 30s\nstart api = PT_KEY=1 node api.js at http://127.0.0.1:5300/api/health\nstart linear = node fake.mjs\nlogs api\ninject delete window.showDirectoryPicker\nsee no \"Error\" within 2s\nsee \"no results\"\nuse parts/sign-in.e2e\n",
            &base,
        )
        .unwrap();
        assert_eq!(
            script.commands,
            [("token".into(), "node token.mjs viewer".into())]
        );
        assert_eq!(
            parse("extract slug = the prototype address").unwrap().steps,
            [(
                1,
                Step::Extract("slug".into(), "the prototype address".into()),
                None
            )]
        );
        assert_eq!(
            parse("wait 30s\nwait 500ms").unwrap().steps,
            [
                (1, Step::Wait(Duration::from_secs(30)), None),
                (2, Step::Wait(Duration::from_millis(500)), None)
            ]
        );
        assert!(
            parse("wait a bit")
                .unwrap_err()
                .to_string()
                .contains("write it as: wait 30s")
        );
        assert_eq!(
            parse("clock install\nclock freeze\nclock run 3s")
                .unwrap()
                .steps,
            [
                (1, Step::ClockInstall, None),
                (2, Step::ClockFreeze, None),
                (3, Step::ClockRun(Duration::from_secs(3)), None)
            ]
        );
        assert!(
            parse("clock forward")
                .unwrap_err()
                .to_string()
                .contains("write it as: clock install, clock freeze or clock run 3s")
        );
        let steps: Vec<_> = script.steps.into_iter().collect();
        assert_eq!(
            steps,
            [
                (
                    2,
                    Step::Run("curl -s http://x | wc -l".into()),
                    Some(Duration::from_secs(30))
                ),
                (
                    3,
                    Step::Start(Start {
                        name: "api".into(),
                        command: "PT_KEY=1 node api.js".into(),
                        at: Some("http://127.0.0.1:5300/api/health".into()),
                    }),
                    None
                ),
                (
                    4,
                    Step::Start(Start {
                        name: "linear".into(),
                        command: "node fake.mjs".into(),
                        at: None,
                    }),
                    None
                ),
                (5, Step::Logs("api".into()), None),
                (
                    6,
                    Step::Inject("delete window.showDirectoryPicker".into()),
                    None
                ),
                (7, Step::SeeNo("Error".into()), Some(Duration::from_secs(2))),
                (8, Step::See("no results".into()), None),
                (9, Step::Open("http://hub.test".into()), None),
                (9, Step::Do("sign in".into()), None),
            ]
        );
        for (wrong, says) in [
            ("start node api.js", "start name = command"),
            ("start my api = node api.js", "start name = command"),
            ("use missing.e2e", "cannot read"),
            ("see no \"\"", "see no needs some text"),
            ("extract = the id", "extract name = what"),
            ("extract 1d = the id", "extract name = what"),
        ] {
            let error = format!("{:#}", parse_in(wrong, &base).unwrap_err());
            assert!(
                error.contains(says) && error.contains("line 1"),
                "{wrong}: {error}"
            );
        }
        std::fs::write(base.join("loop.e2e"), "use loop.e2e\n").unwrap();
        assert!(format!("{:#}", parse_in("use loop.e2e", &base).unwrap_err()).contains("deep"));
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn dollar_names_take_script_values_then_secret_environment_variables() {
        let values = vec![Input {
            key: "slug".into(),
            text: "campus".into(),
            secret: false,
        }];
        let mut read = Vec::new();
        assert_eq!(
            expand("/p/$slug/$$5/$1/$-$PATH", &values, &mut read).unwrap(),
            format!("/p/campus/$5/$1/$-{}", std::env::var("PATH").unwrap())
        );
        assert!(read.iter().all(|value| value.key == "PATH" && value.secret));
        assert_eq!(read.len(), 1);
        let shared = [Input {
            key: "hub".into(),
            text: "http://127.0.0.1:5191".into(),
            secret: false,
        }];
        assert_eq!(
            expand("$hub/sign-in", &shared, &mut read).unwrap(),
            "http://127.0.0.1:5191/sign-in"
        );
        assert!(
            format!(
                "{:#}",
                expand("$IU2U_UNSET_FOR_TEST", &[], &mut read).unwrap_err()
            )
            .contains("with IU2U_UNSET_FOR_TEST = ...")
        );
    }
}
