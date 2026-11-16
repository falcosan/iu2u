mod browser;
mod decide;
mod http;
mod lock;
#[cfg(target_os = "macos")]
mod mac;
mod reporter;
mod screen;
mod script;
mod shell;
mod surface;
mod zip;

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::join_all;
use serde_json::{Value, json};

use crate::clip;
use crate::model::{Answers, Client, Questions, setting};
use browser::{Browser, Handling, file_names};
use decide::{Decision, Input, Verdict};
use lock::{Lock, Move, Recorded, Replay};
pub use reporter::Reporter;
use screen::{Candidate, MAX_ELEMENTS, Node, Origin, Screen};
use script::{Body, Files, Part, Reply, Request, Route, Start, Step};
use surface::{Action, Surface};

const MAX_ACTIONS: usize = 15;
const MAX_DECISIONS: usize = 30;
const SEE_WITHIN: Duration = Duration::from_secs(5);
const SEE_EVERY: Duration = Duration::from_millis(200);
const REMOVE_WITHIN: Duration = Duration::from_secs(2);
const REPORT_SCREEN_CHARS: usize = 3000;
const FLOAT_SLACK: f64 = 1e-9;
const TEXT_ONLY: &str =
    "The screen is given as text only: it does not show colours, positions, sizes or pictures.";
const CHANGED: &str = "the screen does not match the lock file; run without --locked to update it";
const RUN_WITHIN: Duration = Duration::from_secs(120);
const AGAIN_EVERY: Duration = Duration::from_millis(500);
const MAX_RECENT: usize = 30;
const LASTING_ASKS: usize = 2;
const ASK_PAUSE: Duration = Duration::from_secs(1);
const MAX_ASK_PAUSE: Duration = Duration::from_secs(8);
const MODEL_TRIES: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(1);

const NOT_LOCKED: &str = "this step is not in the lock file; run without --locked to record it";

pub struct Outcome {
    pub code: u8,
    pub report: Value,
}

pub struct Suite {
    locked: bool,
    headful: bool,
    vision: bool,
    services: Vec<shell::Service>,
    values: Vec<Input>,
    files: Vec<script::Files>,
    dir: std::path::PathBuf,
    http: reqwest::Client,
}

impl Suite {
    pub fn new(locked: bool, headful: bool) -> Result<Self> {
        let vision = match setting("VISION").as_deref() {
            None | Some("false" | "0") => false,
            Some("true" | "1") => true,
            Some(_) => bail!("IU2U_VISION must be true or false"),
        };
        browser::sweep(&std::env::temp_dir());
        let dir =
            std::env::temp_dir().join(format!("{}{}", browser::SUITE_PREFIX, std::process::id()));
        browser::private_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        Ok(Self {
            locked,
            headful,
            vision,
            services: Vec::new(),
            values: Vec::new(),
            files: Vec::new(),
            dir,
            http: http::client()?,
        })
    }

    pub async fn stop(self) {
        join_all(self.services.into_iter().map(shell::stop)).await;
        let started = Instant::now();
        while std::fs::remove_dir_all(&self.dir).is_err()
            && self.dir.exists()
            && started.elapsed() < REMOVE_WITHIN
        {
            tokio::time::sleep(SEE_EVERY).await;
        }
    }
}

struct Report {
    status: &'static str,
    via: &'static str,
    detail: Option<String>,
}

struct Progress {
    moves: Vec<Move>,
    via: &'static str,
    pressed: Option<(String, (String, String))>,
}

enum Replayed {
    Settled(Report),
    Afresh(Progress),
}

#[derive(Clone)]
enum Again {
    Request(Request),
    Run(String, Duration),
    Logs(String),
}

struct Run<'a> {
    client: Result<Client>,
    suite: &'a mut Suite,
    model_calls: usize,
    surface: Option<Surface>,
    response: Option<Vec<String>>,
    json: Option<Value>,
    again: Option<Again>,
    recent: Vec<String>,
    inputs: Vec<Input>,
    env: Vec<Input>,
    files: Vec<Files>,
    chosen: Vec<Files>,
    numbers: Vec<String>,
    named: Vec<String>,
    old: Lock,
    new: Lock,
    screen: String,
    omitted: usize,
    cut: bool,
    shows_secret: bool,
    shot: Option<String>,
    done: Vec<String>,
    files_refused: bool,
}

pub async fn act(
    client: Result<Client>,
    text: &str,
    base: &Path,
    lock: Option<&Path>,
    suite: &mut Suite,
    before: bool,
    mut on_step: impl FnMut(&Value),
) -> Result<Outcome> {
    let mut script = script::parse_in(text, base)?;
    anyhow::ensure!(
        before || !script.steps.is_empty(),
        "the script has no steps"
    );
    let shared: Vec<Input> = suite
        .values
        .iter()
        .filter(|value| !script::taken(&script, &value.key))
        .cloned()
        .collect();
    let shared_files: Vec<script::Files> = suite
        .files
        .iter()
        .filter(|files| !script::taken(&script, &files.key))
        .cloned()
        .collect();
    script.files.extend(shared_files);
    script.inputs.extend(shared);
    for (key, command) in std::mem::take(&mut script.commands) {
        let ran = shell::run(&command, &environment(&script.inputs), RUN_WITHIN)
            .await
            .with_context(|| format!("with {key}"))?;
        let said = ran.lines[ran.stdout.lines().count()..].last().map(|line| {
            format!(
                ": {}",
                screen::Hider::new(&secrets(&script.inputs)).hide(line)
            )
        });
        anyhow::ensure!(
            ran.code == Some(0),
            "with {key}: the command ended with {}{}",
            ran.exit(),
            said.unwrap_or_default()
        );
        anyhow::ensure!(
            !ran.cut,
            "with {key}: the command printed more than {} bytes, too much for a value",
            shell::KEPT_BYTES
        );
        let text = ran.stdout.trim().to_owned();
        anyhow::ensure!(!text.is_empty(), "with {key}: the command printed nothing");
        script.inputs.push(Input {
            key,
            text,
            secret: true,
        });
    }
    let old: Lock = match lock.map(|path| (path, std::fs::read_to_string(path))) {
        Some((path, Ok(saved))) => serde_json::from_str(&saved)
            .with_context(|| format!("{} is not valid; delete it to start over", path.display()))?,
        Some((path, Err(error))) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(
                anyhow::Error::new(error).context(format!("cannot read {}", path.display()))
            );
        }
        _ => Lock::new(),
    };
    let started = Instant::now();
    let inherited_services = suite.services.len();
    suite
        .services
        .iter_mut()
        .for_each(shell::Service::begin_script);
    let mut run = Run {
        client,
        suite,
        model_calls: 0,
        surface: None,
        response: None,
        json: None,
        again: None,
        recent: Vec::new(),
        inputs: script.inputs,
        env: Vec::new(),
        files: script.files,
        chosen: Vec::new(),
        numbers: Vec::new(),
        named: Vec::new(),
        old,
        new: Lock::new(),
        screen: String::new(),
        omitted: 0,
        cut: false,
        shows_secret: false,
        shot: None,
        done: Vec::new(),
        files_refused: false,
    };
    let mut steps = Vec::new();
    let mut occurrences = std::collections::HashMap::new();
    let mut status = "passed";
    let mut error = None;
    for (line, step, within) in &script.steps {
        let text = step.text_within(*within);
        let checked = step.text();
        let occurrence = occurrences
            .entry(checked.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        let key = lock::key(&checked, *occurrence);
        run.done.clear();
        run.chosen.clear();
        run.files_refused = false;
        run.numbers = screen::numbers(&checked);
        run.named = script::references(&checked);
        let began = Instant::now();
        let (settled, via, detail) = match run.step(&key, step, *within).await {
            Ok(report) => (report.status, Some(report.via), report.detail),
            Err(failure) => {
                error = Some(run.hidden(&format!("line {line}: {text}: {failure:#}")));
                ("error", None, Some(format!("{failure:#}")))
            }
        };
        let detail = detail.map(|detail| run.hidden(&detail));
        let actions = std::mem::take(&mut run.done);
        let reported = json!({
            "line": line,
            "step": text,
            "status": settled,
            "via": via,
            "actions": actions,
            "detail": detail,
            "seconds": seconds(began),
        });
        on_step(&reported);
        steps.push(reported);
        if settled != "passed" {
            status = if settled == "error" {
                "error"
            } else {
                "failed"
            };
            break;
        }
    }
    let mut screenshot = None;
    if let Some(path) = lock.map(|path| path.with_extension("failed.png")) {
        std::fs::remove_file(&path).ok();
        if status != "passed"
            && matches!(run.surface, Some(Surface::Browser(_)))
            && !run.page_shows_secret().await
            && let Ok(Some(png)) = run.fresh_shot().await
            && let Ok(png) = STANDARD.decode(png)
            && std::fs::write(&path, png).is_ok()
        {
            screenshot = Some(path.display().to_string());
        }
    }
    run.close().await;
    if before {
        run.suite.values = std::mem::take(&mut run.inputs);
        run.suite.files = std::mem::take(&mut run.files);
    } else {
        join_all(
            run.suite
                .services
                .split_off(inherited_services)
                .into_iter()
                .map(shell::stop),
        )
        .await;
    }
    if let ("passed", false, Some(path)) = (status, run.suite.locked, lock)
        && run.new != run.old
    {
        let mut partial = path.as_os_str().to_owned();
        partial.push(format!(".{}.tmp", std::process::id()));
        let recorded = format!("{}\n", serde_json::to_string_pretty(&run.new)?);
        if let Err(failure) =
            std::fs::write(&partial, recorded).and_then(|()| std::fs::rename(&partial, path))
        {
            std::fs::remove_file(&partial).ok();
            status = "error";
            error = Some(format!("cannot write {}: {failure}", path.display()));
        }
    }
    let mut report = json!({
        "status": status,
        "steps": steps,
        "model_calls": run.model_calls,
        "seconds": seconds(started),
        "screen": clip(&run.screen, REPORT_SCREEN_CHARS),
    });
    if let Some(error) = error {
        report["error"] = error.into();
    }
    if let Some(screenshot) = screenshot {
        report["screenshot"] = screenshot.into();
    }
    Ok(Outcome {
        code: match status {
            "passed" => 0,
            "failed" => 1,
            _ => 2,
        },
        report,
    })
}

fn headers(lines: &[String]) -> &[String] {
    let end = lines
        .iter()
        .skip(1)
        .position(String::is_empty)
        .map_or(0, |at| at + 1);
    lines.get(1..end).unwrap_or_default()
}

fn text_nodes(lines: &[String], http: bool) -> Vec<Node> {
    let headers = if http { headers(lines).len() } else { 0 };
    let mut nodes = vec![Node {
        children: (1..=lines.len()).collect(),
        ..Node::default()
    }];
    nodes.extend(lines.iter().enumerate().map(|(at, line)| {
        Node {
            role: if (1..=headers).contains(&at) {
                screen::HEADER
            } else {
                "text"
            }
            .into(),
            name: line.clone(),
            ..Node::default()
        }
    }));
    nodes
}

fn percent(probability: f64) -> u32 {
    (probability * 100.0 + FLOAT_SLACK).floor() as u32
}

fn same_place<'a>(
    recorded: &Origin,
    candidates: &'a [Candidate],
    kept: &[String],
) -> Option<&'a Candidate> {
    let mut exact = candidates
        .iter()
        .filter(|candidate| candidate.origin == *recorded)
        .peekable();
    if exact.peek().is_some() {
        return exact.next().filter(|_| exact.next().is_none());
    }
    let Origin::Screen { role, name, field } = recorded else {
        return None;
    };
    let name = screen::masked(name, kept);
    let mut alike = candidates.iter().filter(|candidate| {
        matches!(&candidate.origin, Origin::Screen { role: shown, name: other, field: at }
            if shown == role && at == field && screen::masked(other, kept) == name)
    });
    alike.next().filter(|_| alike.next().is_none())
}

fn keeps_response(step: &Step) -> bool {
    matches!(
        step,
        Step::See(_) | Step::SeeNo(_) | Step::Expect(_) | Step::Extract(..)
    )
}

fn transient(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
        error.is_timeout()
            || error.is_connect()
            || error.status().is_some_and(|status| {
                status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
            })
    })
}

fn seconds(since: Instant) -> f64 {
    (since.elapsed().as_secs_f64() * 10.0).round() / 10.0
}

impl Report {
    fn passed(via: &'static str, detail: Option<String>) -> Self {
        Self {
            status: "passed",
            via,
            detail,
        }
    }

    fn failed(via: &'static str, detail: String) -> Self {
        Self {
            status: "failed",
            via,
            detail: Some(detail),
        }
    }
}

impl Run<'_> {
    async fn step(&mut self, key: &str, step: &Step, within: Option<Duration>) -> Result<Report> {
        if let Some(Surface::Browser(browser)) = &mut self.surface {
            browser.follow().await?;
        }
        if !keeps_response(step) {
            self.response = None;
            self.json = None;
            self.again = None;
            self.recent.clear();
        }
        match step {
            Step::Open(url) => {
                let url = self.expanded(url)?;
                self.browser().await?.open(&url).await?;
                Ok(Report::passed("browser", None))
            }
            #[cfg(target_os = "macos")]
            Step::App(name) => {
                self.close().await;
                self.surface = Some(Surface::Mac(mac::Mac::launch(name).await?));
                Ok(Report::passed("app", None))
            }
            #[cfg(not(target_os = "macos"))]
            Step::App(name) => Err(anyhow!("driving the app {name:?} needs macOS")),
            Step::Do(goal) => {
                self.chosen = given(script::named(goal, &self.files), &self.suite.dir)?;
                if let Some(Surface::Browser(browser)) = &self.surface {
                    browser.forget_given();
                }
                let inputs = script::inputs(goal, &self.inputs);
                let mut report = self.reach(key, goal, &inputs).await?;
                let mut notes = Vec::new();
                if self.omitted > 0 {
                    notes.push(format!(
                        "{} controls after the first {MAX_ELEMENTS} were not offered",
                        self.omitted
                    ));
                }
                if self.cut {
                    notes.push(decide::cut_note(false));
                }
                if report.status == "failed" && !notes.is_empty() {
                    report.detail = Some(
                        report
                            .detail
                            .into_iter()
                            .chain(notes)
                            .collect::<Vec<_>>()
                            .join("; "),
                    );
                }
                Ok(report)
            }
            Step::Expect(claim) => self.expect(key, claim, within).await,
            Step::See(text) => self.see(text, true, within).await,
            Step::SeeNo(text) => self.see(text, false, within).await,
            Step::Run(command) => self.run(command, within.unwrap_or(RUN_WITHIN)).await,
            Step::Start(start) => self.start(start, within.unwrap_or(RUN_WITHIN)).await,
            Step::Logs(name) => {
                self.logs(name)?;
                self.again = Some(Again::Logs(name.clone()));
                self.observe().await?;
                Ok(Report::passed("shell", None))
            }
            Step::Extract(name, what) => self.extract(key, name, what).await,
            Step::Wait(duration) => {
                tokio::time::sleep(*duration).await;
                Ok(Report::passed("wait", None))
            }
            Step::Inject(source) => {
                let source = source.clone();
                self.browser().await?.inject(source).await?;
                Ok(Report::passed("browser", None))
            }
            Step::ClockInstall | Step::ClockFreeze => {
                let frozen = matches!(step, Step::ClockFreeze);
                self.browser().await?.install_clock(frozen).await?;
                Ok(Report::passed("browser", None))
            }
            Step::Request(request) => self.request(request).await,
            Step::Route(route) => self.route(route).await,
            Step::Back | Step::Forward | Step::Reload | Step::ClockRun(_) => {
                let Some(Surface::Browser(browser)) = &mut self.surface else {
                    bail!("{} needs a page; start the script with open", step.text());
                };
                match step {
                    Step::Back => browser.go(-1).await?,
                    Step::Forward => browser.go(1).await?,
                    Step::ClockRun(by) => browser.run_clock(*by).await?,
                    _ => browser.reload().await?,
                }
                Ok(Report::passed("browser", None))
            }
            Step::Release(pattern) => {
                let pattern = self.expanded(pattern)?;
                let released = self.browser().await?.release(&pattern).await?;
                Ok(Report::passed(
                    "browser",
                    Some(match released {
                        1 => "let 1 held request go".into(),
                        released => format!("let {released} held requests go"),
                    }),
                ))
            }
        }
    }

    async fn browser(&mut self) -> Result<&mut Browser> {
        if !matches!(self.surface, Some(Surface::Browser(_))) {
            self.close().await;
            self.surface = Some(Surface::Browser(
                Browser::launch(self.suite.headful, &self.suite.dir).await?,
            ));
        }
        let Some(Surface::Browser(browser)) = &mut self.surface else {
            unreachable!()
        };
        Ok(browser)
    }

    async fn route(&mut self, route: &Route) -> Result<Report> {
        let pattern = self.expanded(&route.pattern)?;
        let handling = match &route.reply {
            Reply::To(_) if pattern.contains('*') => {
                bail!("route ... to needs an address without *, whose start it replaces")
            }
            Reply::To(url) => Handling::To(self.expanded(url)?),
            Reply::Hold => Handling::Hold,
            Reply::Answer(status, body) => {
                let (body, content_type) = match body {
                    None => (Vec::new(), "text/plain; charset=utf-8".to_owned()),
                    Some(Part::Text(text)) => {
                        let text = if text.trim_start().starts_with(['{', '[']) {
                            self.expanded_json(text)?
                        } else {
                            self.expanded(text)?
                        };
                        let kind = if serde_json::from_str::<Value>(&text).is_ok() {
                            "application/json"
                        } else if text.trim_start().starts_with('<') {
                            "text/html; charset=utf-8"
                        } else {
                            "text/plain; charset=utf-8"
                        };
                        (text.into_bytes(), kind.to_owned())
                    }
                    Some(Part::File(path)) => {
                        let path = self.expanded(path)?;
                        let body =
                            std::fs::read(&path).with_context(|| format!("cannot read {path}"))?;
                        (body, browser::content_type(&path))
                    }
                };
                Handling::Answer {
                    status: *status,
                    body,
                    content_type,
                }
            }
        };
        self.browser()
            .await?
            .route(pattern, route.once, handling)
            .await?;
        Ok(Report::passed("browser", None))
    }

    async fn request(&mut self, request: &Request) -> Result<Report> {
        let address = self.expanded(&request.url)?;
        let url = match reqwest::Url::parse(&address) {
            Ok(url) => url,
            Err(_) => {
                let page = match &mut self.surface {
                    Some(Surface::Browser(browser)) => browser.url().await?,
                    _ => bail!(
                        "{address:?} is relative; open a page first or write the whole address"
                    ),
                };
                reqwest::Url::parse(&page)
                    .and_then(|page| page.join(&address))
                    .with_context(|| format!("cannot resolve {address:?} against {page:?}"))?
            }
        };
        let mut expanded = Request {
            method: request.method.clone(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        };
        for (name, value) in &request.headers {
            expanded.headers.push((name.clone(), self.expanded(value)?));
        }
        expanded.body = match &request.body {
            Some(Body::Json(Part::Text(text))) => {
                Some(Body::Json(Part::Text(self.expanded_json(text)?)))
            }
            Some(Body::Json(Part::File(path))) => {
                let path = self.expanded(path)?;
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {path}"))?;
                Some(Body::Json(Part::Text(text)))
            }
            Some(Body::Form(fields)) => {
                let mut form = Vec::new();
                for (name, part) in fields {
                    form.push((
                        name.clone(),
                        match part {
                            Part::Text(text) => Part::Text(self.expanded(text)?),
                            Part::File(path) => Part::File(self.expanded(path)?),
                        },
                    ));
                }
                Some(Body::Form(form))
            }
            None => None,
        };
        let (lines, json) = http::send(&self.suite.http, &expanded).await?;
        let status = lines.first().cloned();
        self.response = Some(lines);
        self.json = json;
        self.again = ["GET", "HEAD", "OPTIONS"]
            .iter()
            .any(|safe| expanded.method.eq_ignore_ascii_case(safe))
            .then_some(Again::Request(expanded));
        self.observe().await?;
        Ok(Report::passed("http", status))
    }

    fn logs(&mut self, name: &str) -> Result<()> {
        let service = self
            .suite
            .services
            .iter()
            .find(|service| service.name == name)
            .with_context(|| format!("no service named {name} was started"))?;
        self.response = Some(shell::logs(service));
        self.json = None;
        Ok(())
    }

    async fn again(&mut self, left: Duration) -> Result<()> {
        if left.is_zero() {
            return Ok(());
        }
        let again = async {
            match self.again.clone() {
                Some(Again::Request(request)) => {
                    (self.response, self.json) = match http::send(&self.suite.http, &request).await
                    {
                        Ok((lines, json)) => (Some(lines), json),
                        Err(error) => (Some(vec![format!("{error:#}")]), None),
                    };
                }
                Some(Again::Run(command, within)) => {
                    let ran = shell::run(&command, &environment(&self.inputs), within).await;
                    self.response = Some(match ran {
                        Ok(ran) => ran.shown(),
                        Err(error) => vec![format!("{error:#}")],
                    });
                    self.json = None;
                }
                Some(Again::Logs(name)) => self.logs(&name)?,
                None => {}
            }
            Ok(())
        };
        tokio::time::timeout(left, again).await.unwrap_or(Ok(()))
    }

    fn expanded(&mut self, text: &str) -> Result<String> {
        script::expand(text, &self.inputs, &mut self.env)
    }

    fn expanded_json(&mut self, text: &str) -> Result<String> {
        script::expand_json(text, &self.inputs, &mut self.env)
    }

    fn surface(&mut self) -> Result<&mut Surface> {
        self.surface
            .as_mut()
            .context("start the script with open or app")
    }

    async fn observe(&mut self) -> Result<Screen> {
        self.hand_files().await.ok();
        let shell = self.shell();
        let nodes = match &self.response {
            Some(lines) => text_nodes(lines, !shell),
            None => self.surface()?.observe().await?,
        };
        let secrets = self.secrets();
        let mut screen = Screen::new(&nodes, &secrets);
        screen.kept.clone_from(&self.numbers);
        screen.named = self
            .inputs
            .iter()
            .filter(|input| !input.secret && self.named.contains(&input.key))
            .map(|input| (input.text.clone(), format!("${}", input.key)))
            .collect();
        screen.hovers =
            self.response.is_none() && matches!(self.surface, Some(Surface::Browser(_)));
        screen.tail = shell;
        if self.response.is_none()
            && let Some(Surface::Browser(browser)) = &self.surface
        {
            screen.shows_secret |= secrets.iter().any(|(secret, _)| browser.paints(secret));
        }
        if let Some((_, label)) = secrets.iter().find(|(secret, _)| screen.reveals(secret)) {
            bail!("{label} shows on the screen in pieces; the step stopped before sending it");
        }
        self.screen.clone_from(&screen.text);
        if self.response.is_none() && self.recent.last() != Some(&screen.visible) {
            self.recent.push(screen.visible.clone());
            if self.recent.len() > MAX_RECENT {
                self.recent.remove(0);
            }
        }
        self.omitted = screen.omitted;
        self.cut = decide::cut(&screen);
        self.shows_secret = screen.shows_secret;
        screen.spots = screen.hovers && self.suite.vision && !screen.shows_secret;
        self.shot = None;
        Ok(screen)
    }

    async fn until(
        &mut self,
        within: Duration,
        patient: bool,
        shows: impl Fn(&Screen) -> bool,
    ) -> Result<(Screen, bool)> {
        let started = Instant::now();
        let mut last = None;
        loop {
            let screen = self.observe().await?;
            let shown = shows(&screen);
            let fingerprint = screen.fingerprint();
            let still = !patient && last.as_ref() == Some(&fingerprint);
            if shown || still || started.elapsed() >= within {
                return Ok((screen, shown));
            }
            last = Some(fingerprint);
            if self.again.is_some() {
                tokio::time::sleep(AGAIN_EVERY).await;
                self.again(within.saturating_sub(started.elapsed())).await?;
            } else {
                tokio::time::sleep(SEE_EVERY).await;
            }
        }
    }

    async fn hand_files(&mut self) -> Result<()> {
        let given = self.give_files().await;
        if let Err(error) = &given {
            self.done.push(format!(
                "a file chooser or picker could not take the files ({error:#})"
            ));
            self.files_refused = true;
        }
        given
    }

    async fn press(&mut self, action: &Action) -> Result<()> {
        self.recent.clear();
        self.surface()?.act(action).await?;
        self.done.push(action.describe());
        Ok(())
    }

    async fn give_files(&mut self) -> Result<()> {
        let Some(Surface::Browser(browser)) = &self.surface else {
            return Ok(());
        };
        let (chooser, picker) = (browser.chooser(), browser.picker());
        if chooser.is_none() && picker.is_none() {
            return Ok(());
        }
        let paths: Vec<String> = self
            .chosen
            .iter()
            .flat_map(|files| files.paths.iter().cloned())
            .collect();
        let gave = |given: &[String]| {
            let keys: Vec<&str> = self
                .chosen
                .iter()
                .filter(|files| files.paths.iter().any(|path| given.contains(path)))
                .map(|files| files.key.as_str())
                .collect();
            format!("{} ({})", keys.join(" and "), file_names(given))
        };
        if paths.is_empty() {
            if let Some(picker) = &picker {
                browser.refuse(picker).await?;
            }
            self.done
                .push("a file chooser opened, but the goal names no file to give it".into());
            return Ok(());
        }
        if let Some(chooser) = chooser {
            let given = browser.choose(chooser, &paths).await?;
            self.done
                .push(format!("gave the file chooser {}", gave(&given)));
        }
        if let Some(picker) = picker {
            match browser.pick(&picker, &paths).await {
                Ok(given) => self
                    .done
                    .push(format!("gave the file picker {}", gave(&given))),
                Err(error) => {
                    browser.refuse(&picker).await?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn refused(&mut self, action: &Action, error: &anyhow::Error) {
        self.done.push(format!(
            "tried but could not do this: {} ({error:#})",
            action.describe()
        ));
    }

    async fn wait(&mut self) -> Result<u64> {
        let started = Instant::now();
        let (mut last, mut changed) = (self.observe().await?.fingerprint(), false);
        while started.elapsed() < SEE_WITHIN {
            tokio::time::sleep(SEE_EVERY).await;
            let now = self.observe().await?.fingerprint();
            if now != last {
                (last, changed) = (now, true);
            } else if changed {
                break;
            }
        }
        self.done.push(Action::Wait.describe());
        Ok(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    async fn wait_out(&mut self, waits: u32) -> Result<()> {
        for _ in 0..waits {
            self.wait().await?;
        }
        Ok(())
    }

    async fn ask(&mut self, state: &str, questions: &Questions) -> Result<Answers> {
        let sees = self.suite.vision && !self.shows_secret;
        let screenshot = match &self.shot {
            _ if !sees || self.response.is_some() => None,
            Some(shot) => Some(shot.clone()),
            None => self.fresh_shot().await?,
        };
        self.ask_with(state, questions, screenshot).await
    }

    async fn ask_grid(
        &mut self,
        state: &str,
        questions: &Questions,
        grid: decide::Grid,
    ) -> Result<Answers> {
        let shot = self.browser().await?.grid_shot(grid).await?;
        anyhow::ensure!(
            !self.page_shows_secret().await,
            "a secret showed on the page while the model looked for a spot, so its screenshot was not sent"
        );
        self.ask_with(state, questions, Some(shot)).await
    }

    async fn ask_with(
        &mut self,
        state: &str,
        questions: &Questions,
        screenshot: Option<String>,
    ) -> Result<Answers> {
        let client = self.client.as_ref().map_err(|error| anyhow!("{error:#}"))?;
        let text_only;
        let state = if screenshot.is_none() {
            text_only = format!("{state}\n\n{TEXT_ONLY}");
            &text_only
        } else {
            state
        };
        let mut tried = 0;
        loop {
            tried += 1;
            self.model_calls += 1;
            match client
                .ask_seeing(state, questions, screenshot.as_deref())
                .await
            {
                Err(error) if tried < MODEL_TRIES && transient(&error) => {
                    tokio::time::sleep(RETRY_AFTER * tried).await;
                }
                answers => return answers,
            }
        }
    }

    async fn reach(&mut self, key: &str, goal: &str, inputs: &[Input]) -> Result<Report> {
        let progress = match self.replay(key, goal, inputs).await? {
            Replayed::Settled(report) => return Ok(report),
            Replayed::Afresh(progress) => progress,
        };
        if self.suite.locked {
            return Ok(Report::failed("replay", self.not_replayed(key)));
        }
        self.plan(key, goal, inputs, progress).await
    }

    async fn replay(&mut self, key: &str, goal: &str, inputs: &[Input]) -> Result<Replayed> {
        let afresh = |moves, via, pressed| {
            Ok(Replayed::Afresh(Progress {
                moves,
                via,
                pressed,
            }))
        };
        let Some(recorded) = self.old.get(key).cloned() else {
            return afresh(Vec::new(), "model", None);
        };
        if let Some(value) = recorded.missing(inputs) {
            if self.suite.locked {
                return Ok(Replayed::Settled(Report::failed(
                    "replay",
                    format!(
                        "the lock file types the value {value:?}, which the script no longer has; run without --locked to update it"
                    ),
                )));
            }
            return afresh(Vec::new(), "model", None);
        }
        let (mut moves, mut via, mut skipped, mut skipped_ms) = (Vec::new(), "replay", 0, 0);
        let mut last = None;
        for recorded_move in &recorded.moves {
            if let Move::Wait { ms } = recorded_move {
                skipped += 1;
                skipped_ms += ms;
                moves.push(recorded_move.clone());
                continue;
            }
            if let Move::Enter { screen: pressed_on }
            | Move::Escape { screen: pressed_on }
            | Move::Scroll { screen: pressed_on } = recorded_move
                && skipped > 0
            {
                match pressed_on {
                    Some(pressed_on) => {
                        let within = SEE_WITHIN + Duration::from_millis(skipped_ms);
                        self.until(within, true, |screen| screen.signature() == *pressed_on)
                            .await?;
                    }
                    None => self.wait_out(skipped).await?,
                }
                (skipped, skipped_ms) = (0, 0);
            }
            let within = SEE_WITHIN + Duration::from_millis(skipped_ms);
            let (mut screen, _) = self
                .until(within, true, |screen| {
                    matches!(recorded_move.replay(screen, inputs), Replay::Exact(_))
                })
                .await?;
            let mut action = match recorded_move.replay(&screen, inputs) {
                Replay::Exact(action) => action,
                Replay::Missing(_) | Replay::Changed if self.suite.locked => {
                    return Ok(Replayed::Settled(Report::failed("replay", CHANGED.into())));
                }
                Replay::Changed => return afresh(moves, via, last),
                Replay::Missing(control) => {
                    let state =
                        decide::Step::new(goal, &screen, inputs).state(&self.done, &self.chosen);
                    let recognized = decide::recognize(&control, &screen, async |questions| {
                        self.ask(&state, &questions).await
                    })
                    .await?;
                    match recognized.and_then(|element| recorded_move.with(element, inputs)) {
                        Some(action) => {
                            via = "healed";
                            action
                        }
                        None => return afresh(moves, via, last),
                    }
                }
            };
            let mut done = self.press(&action).await;
            if done.is_err() {
                let (started, within) = (
                    Instant::now(),
                    SEE_WITHIN + Duration::from_millis(skipped_ms),
                );
                self.wait_out(skipped).await?;
                loop {
                    screen = self.observe().await?;
                    let Replay::Exact(again) = recorded_move.replay(&screen, inputs) else {
                        break;
                    };
                    action = again;
                    done = self.press(&action).await;
                    if done.is_ok() || started.elapsed() >= within {
                        break;
                    }
                    tokio::time::sleep(SEE_EVERY).await;
                }
            }
            let pressed = done.is_ok();
            if pressed
                && let Err(error) = self.hand_files().await
                && !recorded.files_refused
            {
                done = Err(error);
            }
            (skipped, skipped_ms) = (0, 0);
            if let Err(error) = &done {
                if self.suite.locked {
                    return Ok(Replayed::Settled(Report::failed(
                        "replay",
                        format!("{error:#}"),
                    )));
                }
                if !pressed {
                    self.refused(&action, error);
                    return afresh(moves, via, last);
                }
            }
            moves.push(Move::of(&action, &screen));
            let unfocused = screen.unfocused();
            last = Some((unfocused.clone(), tried(&screen, unfocused, &action)));
            if done.is_err() {
                return afresh(moves, via, last);
            }
        }
        let within = SEE_WITHIN + Duration::from_millis(skipped_ms);
        if let (screen, true) = self
            .until(within, self.suite.locked || skipped > 0, |screen| {
                screen.signature() == recorded.screen
            })
            .await?
        {
            self.record(key, moves, &screen);
            return Ok(Replayed::Settled(Report::passed(via, None)));
        }
        afresh(moves, via, last)
    }

    async fn plan(
        &mut self,
        key: &str,
        goal: &str,
        inputs: &[Input],
        progress: Progress,
    ) -> Result<Report> {
        let Progress {
            mut moves,
            mut via,
            pressed,
        } = progress;
        if via == "replay" {
            via = "model";
        }
        let (mut shown, mut seen) = (String::new(), HashSet::new());
        if let Some((on, tried)) = pressed {
            shown = on;
            seen.insert(tried);
        }
        let mut waited = HashSet::new();
        let mut retried = HashSet::new();
        let mut blocked = HashMap::new();
        let mut unsure_before = false;
        for _ in 0..MAX_DECISIONS {
            let screen = self.observe().await?;
            let unfocused = screen.unfocused();
            if shown != unfocused {
                seen.clear();
                waited.clear();
                shown.clone_from(&unfocused);
            }
            let step = decide::Step::new(goal, &screen, inputs);
            let state = step.state(&self.done, &self.chosen);
            let acted = moves
                .iter()
                .filter(|done| !matches!(done, Move::Wait { .. }))
                .count();
            let decision = step
                .decide(acted > 0, async |questions, grid| match grid {
                    Some(grid) => self.ask_grid(&state, &questions, grid).await,
                    None => self.ask(&state, &questions).await,
                })
                .await?;
            self.hand_files().await.ok();
            match decision {
                Decision::Done => {
                    self.record(key, moves, &screen);
                    return Ok(Report::passed(via, None));
                }
                Decision::Stuck => {
                    return Ok(Report::failed(
                        via,
                        "the model sees no way to reach the goal from this screen".into(),
                    ));
                }
                Decision::Unsure(reason) if unsure_before => {
                    return Ok(Report::failed(
                        via,
                        format!("the model was unsure {reason}"),
                    ));
                }
                Decision::Unsure(_) => {
                    unsure_before = true;
                    let ms = self.wait().await?;
                    moves.push(Move::Wait { ms });
                }
                Decision::Act(action) => {
                    unsure_before = false;
                    via = "model";
                    if *action == Action::Wait {
                        blocked.clear();
                        let ms = self.wait().await?;
                        moves.push(Move::Wait { ms });
                        continue;
                    }
                    if acted >= MAX_ACTIONS {
                        return Ok(Report::failed(
                            via,
                            format!("the goal took more than {MAX_ACTIONS} actions"),
                        ));
                    }
                    let tried = tried(&screen, unfocused, &action);
                    let refusal = (screen.signature(), tried.1.clone());
                    if (seen.contains(&tried) || blocked.contains_key(&refusal))
                        && !(*action == Action::Scroll && self.surface()?.can_scroll().await?)
                    {
                        if let Some(reason) = blocked.get(&refusal) {
                            return Ok(Report::failed(
                                via,
                                format!(
                                    "it could not do this, even after trying again: {} ({reason})",
                                    action.describe()
                                ),
                            ));
                        }
                        if !waited.insert(tried) {
                            return Ok(Report::failed(
                                via,
                                format!(
                                    "it would repeat this on an unchanged screen: {}",
                                    action.describe()
                                ),
                            ));
                        }
                        let ms = self.wait().await?;
                        moves.push(Move::Wait { ms });
                        continue;
                    }
                    match self.press(&action).await {
                        Ok(()) => {
                            seen.insert(tried);
                            moves.push(Move::of(&action, &screen));
                        }
                        Err(error) => {
                            self.refused(&action, &error);
                            if !retried.insert(refusal.clone()) {
                                blocked.insert(refusal, format!("{error:#}"));
                            }
                        }
                    }
                }
            }
        }
        Ok(Report::failed(
            via,
            format!("the goal was not reached in {MAX_DECISIONS} decisions"),
        ))
    }

    async fn expect(&mut self, key: &str, claim: &str, within: Option<Duration>) -> Result<Report> {
        let claim = &self.expanded(claim)?;
        let claim = &self.hidden(claim);
        let locked = self.suite.locked;
        let patience = self.patience(within);
        let started = Instant::now();
        if let Some((recorded, exact, sight)) = self.old.get(key).map(|recorded| {
            (
                recorded.screen.clone(),
                recorded.exact.clone(),
                recorded.sight,
            )
        }) && (locked || !sight || !self.suite.vision)
            && let (screen, true) = self
                .until(patience, locked, |screen| {
                    screen.signature() == recorded && (locked || screen.exact() == exact)
                })
                .await?
        {
            self.record_claim(key, &screen, sight);
            return Ok(Report::passed(
                "cache",
                sight.then(|| {
                    "replayed from the text of the screen: how the page looks was not checked without the model".into()
                }),
            ));
        }
        if self.suite.locked {
            return Ok(Report::failed("cache", self.not_replayed(key)));
        }
        let mut screen = self.observe().await?;
        let (mut verdict, mut holds, mut sight) = self.judge(&screen, claim).await?;
        let mut judged = self.shot.clone();
        let (mut asked, mut pause) = (Instant::now(), Duration::ZERO);
        while verdict != Verdict::Holds && started.elapsed() < patience {
            tokio::time::sleep(if self.again.is_some() {
                AGAIN_EVERY
            } else {
                SEE_EVERY
            })
            .await;
            if self.response.is_some() {
                self.again(patience.saturating_sub(started.elapsed()))
                    .await?;
            }
            let now = self.observe().await?;
            if asked.elapsed() < pause && started.elapsed() < patience {
                continue;
            }
            let looks = sight && self.suite.vision && self.response.is_none();
            if now.text != screen.text || (looks && self.fresh_shot().await? != judged) {
                (verdict, holds, sight) = self.judge(&now, claim).await?;
                judged = self.shot.clone();
                screen = now;
                asked = Instant::now();
                pause = (pause * 2).clamp(ASK_PAUSE, MAX_ASK_PAUSE);
            }
        }
        if verdict == Verdict::Holds {
            for _ in 0..LASTING_ASKS {
                let now = self.observe().await?;
                if now.signature() == screen.signature() {
                    break;
                }
                let (again, again_holds, again_sight) = self.judge(&now, claim).await?;
                if again != Verdict::Holds {
                    break;
                }
                (screen, holds, sight) = (now, again_holds, again_sight);
            }
        }
        let sure = percent(holds);
        let mut detail = match verdict {
            Verdict::Holds => format!("the model is {sure}% sure it holds"),
            Verdict::Contradicted => {
                format!("the model says it does not hold: {sure}% sure it holds")
            }
            Verdict::Inconclusive => format!(
                "the model is unsure: {sure}% sure it holds, a pass needs {}%",
                percent(decide::HOLDS_AT)
            ),
        };
        if decide::cut(&screen) {
            write!(detail, "; {}", decide::cut_note(screen.tail)).ok();
        }
        let sees = self.suite.vision;
        if sight && !sees {
            detail.push_str("; this claim needs to see the page, but the model only read its text: set IU2U_VISION=true with a model that sees");
        } else if sight && self.shows_secret {
            detail.push_str("; this claim needs to see the page, but the model only read its text: no screenshot is sent while a secret shows on the screen");
        } else if sight
            && (self.response.is_some() || !matches!(self.surface, Some(Surface::Browser(_))))
        {
            detail.push_str("; this claim needs to see the screen, but the model only read its text: only a page gets a screenshot, not an app, a response or a command's output");
        }
        Ok(match verdict {
            Verdict::Holds => {
                self.record_claim(key, &screen, sight);
                Report::passed("model", Some(detail))
            }
            Verdict::Contradicted => Report::failed("model", detail),
            Verdict::Inconclusive => Report {
                status: "inconclusive",
                via: "model",
                detail: Some(detail),
            },
        })
    }

    async fn judge(&mut self, screen: &Screen, claim: &str) -> Result<(Verdict, f64, bool)> {
        let state = decide::screen_state(screen);
        let answers = self.ask(&state, &decide::claim(claim)).await?;
        let (verdict, holds) = decide::verdict(&answers)?;
        Ok((verdict, holds, decide::needs_sight(&answers)))
    }

    async fn see(&mut self, text: &str, present: bool, within: Option<Duration>) -> Result<Report> {
        let text = &self.expanded(text)?;
        let text = &self.hidden(text);
        if present
            && self.response.is_none()
            && self
                .recent
                .iter()
                .any(|visible| screen::shows(visible, text))
        {
            return Ok(Report::passed("check", None));
        }
        let holds = |screen: &Screen| screen.shows(text) == present;
        let held = self.until(self.patience(within), true, holds).await?.1;
        if held && !present {
            self.recent.retain(|visible| !screen::shows(visible, text));
        }
        Ok(if held {
            Report::passed("check", None)
        } else if present {
            Report::failed("check", format!("{text:?} is not on the screen"))
        } else {
            Report::failed("check", format!("{text:?} is still on the screen"))
        })
    }

    fn patience(&self, within: Option<Duration>) -> Duration {
        match (&self.response, &self.again) {
            (Some(_), None) => Duration::ZERO,
            (Some(_), Some(_)) => within.unwrap_or_default(),
            (None, _) => within.unwrap_or(SEE_WITHIN),
        }
    }

    fn not_replayed(&self, key: &str) -> String {
        if self.old.contains_key(key) {
            CHANGED
        } else {
            NOT_LOCKED
        }
        .into()
    }

    fn record(&mut self, key: &str, moves: Vec<Move>, screen: &Screen) {
        self.new.insert(
            key.into(),
            Recorded {
                moves,
                screen: screen.signature(),
                picked: None,
                sight: false,
                exact: String::new(),
                files_refused: self.files_refused,
            },
        );
    }

    fn record_claim(&mut self, key: &str, screen: &Screen, sight: bool) {
        self.record(key, Vec::new(), screen);
        if let Some(recorded) = self.new.get_mut(key) {
            recorded.sight = sight;
            recorded.exact = screen.exact();
        }
    }

    fn candidates(&self, screen: &Screen) -> Vec<Candidate> {
        let secrets = self.secrets();
        let hider = screen::Hider::new(&secrets);
        let public = |candidate: &Candidate| {
            hider.hide(&candidate.text) == candidate.text
                && hider.hide(&candidate.shown) == candidate.shown
        };
        let Some(lines) = &self.response else {
            return screen.candidates.clone();
        };
        let line = |(at, line): (usize, &String)| Candidate {
            text: line.trim().to_owned(),
            shown: format!(
                "line {}: {:?}",
                at + 1,
                clip(line.trim(), screen::VALUE_CHARS)
            ),
            origin: Origin::Screen {
                role: "line".into(),
                name: line.trim().to_owned(),
                field: "name".into(),
            },
        };
        let numbered = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty());
        if self.shell() {
            return numbered.map(line).filter(public).collect();
        }
        let headers = headers(lines);
        let header = |line: &String| {
            line.split_once(": ").map(|(name, value)| Candidate {
                text: value.to_owned(),
                shown: format!("{} {name:?}: {value:?}", screen::HEADER),
                origin: Origin::Screen {
                    role: screen::HEADER.into(),
                    name: name.to_owned(),
                    field: "value".into(),
                },
            })
        };
        let body: Vec<Candidate> = match &self.json {
            Some(json) => http::leaves(json)
                .into_iter()
                .map(|(path, leaf)| Candidate {
                    shown: http::line(&path, leaf),
                    text: match leaf {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    },
                    origin: Origin::Json { path },
                })
                .collect(),
            None => numbered.clone().skip(1 + headers.len()).map(line).collect(),
        };
        numbered
            .take(1)
            .map(line)
            .chain(headers.iter().filter_map(header))
            .chain(body)
            .filter(public)
            .collect()
    }

    fn shell(&self) -> bool {
        matches!(self.again, Some(Again::Run(..) | Again::Logs(_)))
    }

    async fn extract(&mut self, key: &str, name: &str, what: &str) -> Result<Report> {
        anyhow::ensure!(
            !self
                .inputs
                .iter()
                .chain(&self.env)
                .any(|input| input.key == name && input.secret),
            "${name} is a secret, so extract cannot replace it; give the value another name"
        );
        let mut screen = self.observe().await?;
        let mut candidates = self.candidates(&screen);
        let recorded = self
            .old
            .get(key)
            .and_then(|recorded| recorded.picked.clone());
        if let Some(origin) = &recorded {
            let (started, patience) = (Instant::now(), self.patience(None));
            while same_place(origin, &candidates, &self.numbers).is_none()
                && started.elapsed() < patience
            {
                tokio::time::sleep(SEE_EVERY).await;
                screen = self.observe().await?;
                candidates = self.candidates(&screen);
            }
        }
        let (chosen, via) = match recorded
            .as_ref()
            .and_then(|origin| same_place(origin, &candidates, &self.numbers))
        {
            Some(candidate) => (candidate.clone(), "replay"),
            None if self.suite.locked && self.old.contains_key(key) && recorded.is_none() => {
                return Ok(Report::failed(
                    "replay",
                    "the value comes from a place another value shares, such as two Edit links, so --locked can never replay it".into(),
                ));
            }
            None if self.suite.locked => {
                return Ok(Report::failed("replay", self.not_replayed(key)));
            }
            None => {
                let left = candidates.len().saturating_sub(MAX_ELEMENTS);
                let start = if screen.tail { left } else { 0 };
                let offered: Vec<String> = candidates[start..]
                    .iter()
                    .take(MAX_ELEMENTS)
                    .map(|candidate| candidate.shown.clone())
                    .collect();
                let state = decide::screen_state(&screen);
                let picked = decide::pick(what, &offered, async |questions| {
                    self.ask(&state, &questions).await
                })
                .await?;
                match picked {
                    Ok(index) => (candidates[start + index].clone(), "model"),
                    Err(reason) => {
                        let mut detail =
                            format!("the model could not tell which is {what}: {reason}");
                        if left > 0 {
                            let (side, part) = if screen.tail {
                                ("before", "last")
                            } else {
                                ("after", "first")
                            };
                            write!(
                                detail,
                                "; {left} values {side} the {part} {MAX_ELEMENTS} were not offered"
                            )
                            .ok();
                        }
                        return Ok(Report::failed("model", detail));
                    }
                }
            }
        };
        let origin = match chosen.origin.clone() {
            Origin::Screen { role, name, field } if field == "name" => Origin::Screen {
                role,
                name: screen::masked(&name, &self.numbers),
                field,
            },
            origin => origin,
        };
        let replayable = same_place(&origin, &candidates, &self.numbers) == Some(&chosen);
        self.record(key, Vec::new(), &screen);
        if let Some(recorded) = self.new.get_mut(key) {
            recorded.picked = replayable.then_some(origin);
        }
        let detail = format!("{name} = {:?}", chosen.text);
        self.inputs.retain(|input| input.key != name);
        self.inputs.push(Input {
            key: name.into(),
            text: chosen.text,
            secret: false,
        });
        Ok(Report::passed(via, Some(detail)))
    }

    async fn close(&mut self) {
        if let Some(surface) = self.surface.take() {
            surface.close().await;
        }
    }

    async fn fresh_shot(&mut self) -> Result<Option<String>> {
        let Some(Surface::Browser(browser)) = &self.surface else {
            return Ok(None);
        };
        let shot = browser.screenshot().await?;
        if self.page_shows_secret().await {
            return Ok(None);
        }
        self.shot = Some(shot.clone());
        Ok(Some(shot))
    }

    async fn page_shows_secret(&mut self) -> bool {
        let screen = std::mem::take(&mut self.screen);
        self.response = None;
        let shows = self.observe().await.map_or(true, |page| page.shows_secret);
        self.screen = screen;
        shows
    }

    fn hidden(&self, text: &str) -> String {
        screen::Hider::new(&self.secrets()).hide(text)
    }

    fn secrets(&self) -> Vec<(&str, String)> {
        secrets(self.inputs.iter().chain(&self.env))
    }

    async fn run(&mut self, command: &str, within: Duration) -> Result<Report> {
        let command = &format!("exec 2>&1; {command}");
        let ran = shell::run(command, &environment(&self.inputs), within).await?;
        let (exit, code) = (ran.exit(), ran.code);
        let last = ran.lines.last().cloned().unwrap_or_default();
        self.response = Some(ran.shown());
        self.json = None;
        self.again = Some(Again::Run(command.to_owned(), within));
        self.observe().await?;
        Ok(if code == Some(0) {
            Report::passed("shell", Some(exit))
        } else {
            Report::failed(
                "shell",
                match last.trim() {
                    "" => exit,
                    last => format!("{exit}: {last}"),
                },
            )
        })
    }

    async fn start(&mut self, start: &Start, within: Duration) -> Result<Report> {
        anyhow::ensure!(
            self.suite
                .services
                .iter()
                .all(|service| service.name != start.name),
            "{} is already started",
            start.name
        );
        let start = Start {
            at: start
                .at
                .as_deref()
                .map(|at| self.expanded(at))
                .transpose()?,
            ..start.clone()
        };
        let service = shell::start(
            &self.suite.http,
            &start,
            &environment(&self.inputs),
            &self.suite.dir,
            within,
        )
        .await?;
        self.suite.services.push(service);
        Ok(Report::passed("shell", None))
    }
}

fn tried(screen: &Screen, unfocused: String, action: &Action) -> (String, String) {
    let keyed = matches!(*action, Action::Enter | Action::Escape);
    (
        if keyed {
            screen.fingerprint()
        } else {
            unfocused
        },
        screen::without_focus(&action.describe()),
    )
}

fn given(named: Vec<&Files>, dir: &Path) -> Result<Vec<Files>> {
    named
        .into_iter()
        .map(|files| {
            let paths = if files.zip {
                let folder = Path::new(&files.paths[0]);
                anyhow::ensure!(
                    folder.is_dir(),
                    "with {} = zip takes one folder, and {} is not one",
                    files.key,
                    folder.display()
                );
                vec![
                    zip::zip(folder, &dir.join("zips").join(&files.key))?
                        .to_string_lossy()
                        .into_owned(),
                ]
            } else {
                for path in &files.paths {
                    anyhow::ensure!(Path::new(path).exists(), "{path} does not exist");
                }
                files.paths.clone()
            };
            Ok(Files {
                key: files.key.clone(),
                paths,
                zip: false,
            })
        })
        .collect()
}

fn secrets<'a>(inputs: impl IntoIterator<Item = &'a Input>) -> Vec<(&'a str, String)> {
    inputs
        .into_iter()
        .filter(|input| input.secret)
        .map(|input| (input.text.as_str(), input.shown()))
        .collect()
}

fn environment(inputs: &[Input]) -> Vec<(String, String)> {
    inputs
        .iter()
        .filter(|input| script::is_name(&input.key))
        .map(|input| (input.key.clone(), input.text.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percents_round_down_without_float_noise() {
        assert_eq!(percent(0.29), 29);
        assert_eq!(percent(0.58), 58);
        assert_eq!(percent(0.74996), 74);
        assert_eq!(percent(decide::HOLDS_AT), 75);
    }
}
