mod browser;
mod decide;
mod lock;
#[cfg(target_os = "macos")]
mod mac;
mod screen;
mod script;
mod surface;

use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::clip;
use crate::model::{Answers, Client, Questions};
use browser::Browser;
use decide::{Decision, Input, Verdict};
use lock::{Lock, Move, Recorded, Replay};
use screen::{MAX_ELEMENTS, Screen};
use script::Step;
use surface::{Action, Surface};

const MAX_ACTIONS: usize = 15;
const MAX_DECISIONS: usize = 30;
const SEE_WITHIN: Duration = Duration::from_secs(5);
const SEE_EVERY: Duration = Duration::from_millis(200);
const REPORT_SCREEN_CHARS: usize = 3000;
const CHANGED: &str =
    "the screen changed since the lock file was written; run without --locked to update it";
const NOT_LOCKED: &str = "this step is not in the lock file; run without --locked to record it";

pub struct Outcome {
    pub code: u8,
    pub report: Value,
}

struct Report {
    status: &'static str,
    via: &'static str,
    detail: Option<String>,
}

struct Progress {
    moves: Vec<Move>,
    via: &'static str,
    prefetched: Option<(Screen, Answers)>,
}

enum Replayed {
    Settled(Report),
    Afresh(Progress),
}

struct Run {
    client: Result<Client>,
    locked: bool,
    model_calls: usize,
    surface: Option<Surface>,
    inputs: Vec<Input>,
    old: Lock,
    new: Lock,
    screen: String,
    omitted: usize,
    cut: bool,
    done: Vec<String>,
}

pub async fn act(
    client: Result<Client>,
    text: &str,
    lock: Option<&Path>,
    locked: bool,
) -> Result<Outcome> {
    let script = script::parse(text)?;
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
    let mut run = Run {
        client,
        locked,
        model_calls: 0,
        surface: None,
        inputs: script.inputs,
        old,
        new: Lock::new(),
        screen: String::new(),
        omitted: 0,
        cut: false,
        done: Vec::new(),
    };
    let mut steps = Vec::new();
    let mut occurrences = std::collections::HashMap::new();
    let mut status = "passed";
    let mut error = None;
    for (line, step) in &script.steps {
        let text = step.text();
        let occurrence = occurrences
            .entry(text.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        let key = lock::key(&text, *occurrence);
        run.done.clear();
        let (settled, via, detail) = match run.step(&key, step).await {
            Ok(report) => (report.status, Some(report.via), report.detail),
            Err(failure) => {
                let message = format!("line {line}: {text}: {failure:#}");
                error = Some(message.clone());
                ("error", None, Some(message))
            }
        };
        let actions = std::mem::take(&mut run.done);
        steps.push(json!({
            "line": line,
            "step": text,
            "status": settled,
            "via": via,
            "actions": actions,
            "detail": detail,
        }));
        if settled != "passed" {
            status = if settled == "error" {
                "error"
            } else {
                "failed"
            };
            break;
        }
    }
    run.close().await;
    if let ("passed", false, Some(path)) = (status, locked, lock)
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
        "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "screen": clip(&run.screen, REPORT_SCREEN_CHARS),
    });
    if let Some(error) = error {
        report["error"] = error.into();
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

impl Run {
    async fn step(&mut self, key: &str, step: &Step) -> Result<Report> {
        match step {
            Step::Open(url) => {
                if !matches!(self.surface, Some(Surface::Browser(_))) {
                    self.close().await;
                    self.surface = Some(Surface::Browser(Browser::launch().await?));
                }
                let Some(Surface::Browser(browser)) = &mut self.surface else {
                    unreachable!()
                };
                browser.open(url).await?;
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
                    notes.push(decide::cut_note());
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
            Step::Expect(claim) => self.expect(key, claim).await,
            Step::See(text) => self.see(text).await,
        }
    }

    async fn observe(&mut self) -> Result<Screen> {
        let nodes = self
            .surface
            .as_mut()
            .context("start the script with open or app")?
            .observe()
            .await?;
        let secrets: Vec<(&str, String)> = self
            .inputs
            .iter()
            .filter(|input| input.secret)
            .map(|input| (input.text.as_str(), input.shown()))
            .collect();
        let screen = Screen::new(&nodes, &secrets);
        if let Some((_, label)) = secrets.iter().find(|(secret, _)| screen.reveals(secret)) {
            bail!("{label} shows on the screen in pieces; the step stopped before sending it");
        }
        self.screen.clone_from(&screen.text);
        self.omitted = screen.omitted;
        self.cut = decide::cut(&screen);
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
            tokio::time::sleep(SEE_EVERY).await;
        }
    }

    async fn act(&mut self, action: &Action) -> Result<()> {
        self.surface
            .as_mut()
            .context("start the script with open or app")?
            .act(action)
            .await?;
        self.done.push(action.describe());
        Ok(())
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
        let client = self.client.as_ref().map_err(|error| anyhow!("{error:#}"))?;
        self.model_calls += 1;
        client.ask(state, questions).await
    }

    async fn reach(&mut self, key: &str, goal: &str, inputs: &[Input]) -> Result<Report> {
        let progress = match self.replay(key, goal, inputs).await? {
            Replayed::Settled(report) => return Ok(report),
            Replayed::Afresh(progress) => progress,
        };
        if self.locked {
            return Ok(Report::failed("replay", self.not_replayed(key)));
        }
        self.plan(key, goal, inputs, progress).await
    }

    async fn replay(&mut self, key: &str, goal: &str, inputs: &[Input]) -> Result<Replayed> {
        let afresh = |moves, via, prefetched| {
            Ok(Replayed::Afresh(Progress {
                moves,
                via,
                prefetched,
            }))
        };
        let Some(recorded) = self.old.get(key).cloned() else {
            return afresh(Vec::new(), "model", None);
        };
        if let Some(value) = recorded.missing(inputs) {
            if self.locked {
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
        for recorded_move in &recorded.moves {
            if let Move::Wait { ms } = recorded_move {
                skipped += 1;
                skipped_ms += ms;
                moves.push(recorded_move.clone());
                continue;
            }
            if let Move::Enter { screen: pressed_on } | Move::Scroll { screen: pressed_on } =
                recorded_move
                && skipped > 0
            {
                match pressed_on {
                    Some(pressed_on) => {
                        let within = SEE_WITHIN + Duration::from_millis(skipped_ms);
                        self.until(within, true, |screen| screen.fingerprint() == *pressed_on)
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
                Replay::Missing(_) if self.locked => {
                    return Ok(Replayed::Settled(Report::failed("replay", CHANGED.into())));
                }
                Replay::Missing(control) => {
                    let step = decide::Step::new(goal, &screen, inputs);
                    let (mut questions, recognized) = decide::recognize(&control, &screen);
                    questions.extend(step.questions());
                    let state = step.state(&self.done);
                    let answers = self.ask(&state, &questions).await?;
                    match recognized(&answers)
                        .and_then(|element| recorded_move.with(element, inputs))
                    {
                        Some(action) => {
                            via = "healed";
                            action
                        }
                        None => return afresh(moves, via, Some((screen, answers))),
                    }
                }
            };
            let mut done = self.act(&action).await;
            if done.is_err() && skipped > 0 {
                self.wait_out(skipped).await?;
                screen = self.observe().await?;
                if let Replay::Exact(again) = recorded_move.replay(&screen, inputs) {
                    action = again;
                    done = self.act(&action).await;
                }
            }
            (skipped, skipped_ms) = (0, 0);
            if let Err(error) = done {
                if self.locked {
                    return Ok(Replayed::Settled(Report::failed(
                        "replay",
                        format!("{error:#}"),
                    )));
                }
                self.done.push(format!(
                    "tried but could not do this: {} ({error:#})",
                    action.describe()
                ));
                return afresh(moves, via, None);
            }
            moves.push(Move::of(&action, &screen));
        }
        let within = SEE_WITHIN + Duration::from_millis(skipped_ms);
        if let (screen, true) = self
            .until(within, self.locked || skipped > 0, |screen| {
                screen.fingerprint() == recorded.screen
            })
            .await?
        {
            self.record(key, moves, &screen);
            return Ok(Replayed::Settled(Report::passed(via, None)));
        }
        afresh(moves, via, None)
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
            mut prefetched,
        } = progress;
        if via == "replay" {
            via = "model";
        }
        let mut seen = HashSet::new();
        let mut unsure_before = false;
        for _ in 0..MAX_DECISIONS {
            let (screen, answers) = match prefetched.take() {
                Some(pair) => pair,
                None => {
                    let screen = self.observe().await?;
                    let step = decide::Step::new(goal, &screen, inputs);
                    let state = step.state(&self.done);
                    let answers = self.ask(&state, &step.questions()).await?;
                    (screen, answers)
                }
            };
            match decide::Step::new(goal, &screen, inputs).decide(&answers) {
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
                        seen.clear();
                        let ms = self.wait().await?;
                        moves.push(Move::Wait { ms });
                        continue;
                    }
                    let acted = moves
                        .iter()
                        .filter(|done| !matches!(done, Move::Wait { .. }))
                        .count();
                    if acted >= MAX_ACTIONS {
                        return Ok(Report::failed(
                            via,
                            format!("the goal took more than {MAX_ACTIONS} actions"),
                        ));
                    }
                    if !seen.insert((screen.fingerprint(), action.describe())) {
                        return Ok(Report::failed(
                            via,
                            format!(
                                "it would repeat this on an unchanged screen: {}",
                                action.describe()
                            ),
                        ));
                    }
                    match self.act(&action).await {
                        Ok(()) => moves.push(Move::of(&action, &screen)),
                        Err(error) => self.done.push(format!(
                            "tried but could not do this: {} ({error:#})",
                            action.describe()
                        )),
                    }
                }
            }
        }
        Ok(Report::failed(
            via,
            format!("no decision held after {MAX_DECISIONS} tries"),
        ))
    }

    async fn expect(&mut self, key: &str, claim: &str) -> Result<Report> {
        if let Some(recorded) = self.old.get(key).map(|recorded| recorded.screen.clone())
            && let (screen, true) = self
                .until(SEE_WITHIN, self.locked, |screen| {
                    screen.fingerprint() == recorded
                })
                .await?
        {
            self.record(key, Vec::new(), &screen);
            return Ok(Report::passed("cache", None));
        }
        if self.locked {
            return Ok(Report::failed("cache", self.not_replayed(key)));
        }
        let screen = self.observe().await?;
        let state = decide::screen_state(&screen);
        let answers = self.ask(&state, &decide::claim(claim)).await?;
        let (verdict, holds, contradicted) = decide::verdict(&answers);
        let mut detail = format!("holds {holds:.2}, contradicted {contradicted:.2}");
        if decide::cut(&screen) {
            write!(detail, "; {}", decide::cut_note()).ok();
        }
        Ok(match verdict {
            Verdict::Holds => {
                self.record(key, Vec::new(), &screen);
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

    async fn see(&mut self, text: &str) -> Result<Report> {
        let (_, shown) = self
            .until(SEE_WITHIN, true, |screen| screen.shows(text))
            .await?;
        Ok(if shown {
            Report::passed("check", None)
        } else {
            Report::failed("check", format!("{text:?} is not on the screen"))
        })
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
                screen: screen.fingerprint(),
            },
        );
    }

    async fn close(&mut self) {
        if let Some(surface) = self.surface.take() {
            surface.close().await;
        }
    }
}
