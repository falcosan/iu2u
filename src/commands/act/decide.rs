use std::collections::BTreeMap;
use std::fmt::Write;
use std::mem::discriminant;
use std::path::Path;

use anyhow::{Context, Result};

use super::screen::{Element, Kind, MAX_ELEMENTS, Screen};
use super::script::Files;
use super::surface::Action;
use crate::clip;
use crate::model::{Answer, Answers, Question, Questions};

pub(super) const SCREEN_CHARS: usize = 6000;
pub(super) const HOLDS_AT: f64 = 0.75;
const SIGHT_AT: f64 = 0.5;
const OPERATION_AT: f64 = 0.5;
const TARGET_AT: f64 = 0.3;
const RECOGNIZED_AT: f64 = 0.6;
const MARGIN: f64 = 0.1;
const NONE: &str = "none";
const NONE_FITS: &str = "none of the options fits";
const REVERSED: &str = "_reversed";
const COARSE: (usize, usize) = (8, 6);
const FINE: (usize, usize) = (8, 8);
const CLOSER: f64 = 1.5;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Input {
    pub(super) key: String,
    pub(super) text: String,
    pub(super) secret: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Act(Box<Action>),
    Done,
    Stuck,
    Unsure(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Area {
    pub(super) left: f64,
    pub(super) top: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Grid {
    pub(super) area: Area,
    pub(super) columns: usize,
    pub(super) rows: usize,
    pub(super) reversed: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    Holds,
    Contradicted,
    Inconclusive,
}

struct Operation<'a> {
    name: &'static str,
    description: &'static str,
    targets: Vec<&'a Element>,
}

pub(super) struct Step<'a> {
    goal: &'a str,
    screen: &'a Screen,
    inputs: &'a [Input],
    operations: Vec<Operation<'a>>,
}

impl Input {
    pub(super) fn shown(&self) -> String {
        if self.secret {
            format!("(secret {:?})", self.key)
        } else {
            format!("{:?}", self.text)
        }
    }
}

impl<'a> Step<'a> {
    pub(super) fn new(goal: &'a str, screen: &'a Screen, inputs: &'a [Input]) -> Self {
        let elements = &screen.elements;
        let press: Vec<&Element> = elements
            .iter()
            .filter(|element| !matches!(element.kind, Kind::Choose(_)))
            .collect();
        let typable = elements.iter().filter(|element| element.kind == Kind::Type);
        let type_ = typable.clone().filter(|_| !inputs.is_empty()).collect();
        let choose = elements
            .iter()
            .filter(|element| matches!(element.kind, Kind::Choose(_)))
            .collect();
        let focused_field = typable
            .into_iter()
            .any(|element| element.states.contains(&"focused"));
        let operations = [
            (
                "press",
                "Press, click or toggle one control; a menu or list that is still closed opens this way",
                press.clone(),
            ),
            (
                "hover",
                "Point at one control without pressing it, to show its tooltip or menu",
                if screen.hovers { press } else { Vec::new() },
            ),
            ("type", "Replace the whole text of a field with one of the given values", type_),
            (
                "choose",
                "Choose an option in a drop-down list that already offers its options",
                choose,
            ),
        ]
        .into_iter()
        .map(|(name, description, targets)| Operation { name, description, targets })
        .filter(|operation| !operation.targets.is_empty())
        .chain(
            [
                (screen.spots, "spot", "Press a spot of the screenshot that no control in the list stands for, such as a place on a map, a point on a chart or a part of a picture"),
                (focused_field, "enter", "Press Enter in the focused field, for example to submit it"),
                (true, "escape", "Press Escape, for example to close an open menu, list or dialog"),
                (true, "scroll", "Scroll down to reveal more of the screen; a press or a point at a control already on the screen reaches it without scrolling"),
                (true, "wait", "Wait, because the screen is still loading or changing"),
                (true, "done", "The goal is already reached on this screen"),
                (true, "stuck", "The goal cannot be reached from here: an error, a dead end or missing information"),
            ]
            .into_iter()
            .filter(|(offered, ..)| *offered)
            .map(|(_, name, description)| Operation { name, description, targets: Vec::new() }),
        )
        .collect();
        Self {
            goal,
            screen,
            inputs,
            operations,
        }
    }

    fn values(&self, operation: &str, element: &Element) -> Vec<String> {
        match (operation, &element.kind) {
            ("type", _) => self.inputs.iter().map(Input::shown).collect(),
            ("choose", Kind::Choose(options)) => {
                options.iter().map(|option| format!("{option:?}")).collect()
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn state(&self, history: &[String], files: &[Files]) -> String {
        let mut state = format!("Goal: {}\n", self.goal);
        if !files.is_empty() {
            state.push_str("Files the goal names, for a file chooser or picker:\n");
            for files in files {
                let given: Vec<String> = files
                    .paths
                    .iter()
                    .map(|path| {
                        let path = Path::new(path);
                        let name = path
                            .file_name()
                            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
                        if path.is_dir() {
                            format!("the folder {name}")
                        } else {
                            format!("the file {name}")
                        }
                    })
                    .collect();
                writeln!(state, "- {}: {}", files.key, given.join(", ")).ok();
            }
        }
        if !history.is_empty() {
            state.push_str("Done so far for this goal:\n");
            for done in history {
                writeln!(state, "- {done}").ok();
            }
        }
        state.push_str(&screen_state(self.screen));
        if self.screen.omitted > 0 {
            write!(
                state,
                "\n({} more controls are not listed)",
                self.screen.omitted
            )
            .ok();
        }
        state
    }

    pub(super) async fn decide(
        &self,
        acted: bool,
        mut ask: impl AsyncFnMut(Questions, Option<Grid>) -> Result<Answers>,
    ) -> Result<Decision> {
        let goal = self.goal;
        let mut operations: Vec<&Operation> = self.operations.iter().collect();
        let (operation, index) = loop {
            let answers = ask(
                Questions::from([(
                    "operation".into(),
                    Question::Pick {
                        text: format!(
                            "Which operation brings the screen closest to this goal: {goal}"
                        ),
                        options: operations
                            .iter()
                            .map(|operation| (operation.name.into(), operation.description.into()))
                            .collect(),
                    },
                )]),
                None,
            )
            .await?;
            let chosen = match ranked(answers.get("operation")) {
                Some((pick, top, margin))
                    if top >= OPERATION_AT
                        && margin >= MARGIN
                        && (pick != "stuck" || top >= HOLDS_AT) =>
                {
                    pick
                }
                ranking => {
                    return Ok(Decision::Unsure(format!(
                        "which operation to use ({})",
                        shown(ranking)
                    )));
                }
            };
            let Some(operation) = operations
                .iter()
                .copied()
                .find(|operation| operation.name == chosen)
            else {
                let names: Vec<&str> = operations.iter().map(|operation| operation.name).collect();
                return Ok(Decision::Unsure(format!(
                    "which operation to use: {chosen:?} is not one of {names:?}"
                )));
            };
            let name = operation.name;
            match name {
                "done" if acted => return Ok(Decision::Done),
                "done" => {
                    let reached = ask(
                        Questions::from([(
                            "holds".into(),
                            Question::yesno(&format!(
                                "Is this goal already reached on this screen, with nothing left to do: {goal}"
                            )),
                        )]),
                        None,
                    )
                    .await?;
                    match verdict(&reached)? {
                        (Verdict::Holds, _) => return Ok(Decision::Done),
                        (Verdict::Inconclusive, holds) => {
                            return Ok(Decision::Unsure(format!(
                                "whether the goal is reached (at {holds:.2})"
                            )));
                        }
                        (Verdict::Contradicted, _) => {
                            operations.retain(|other| other.name != name);
                            continue;
                        }
                    }
                }
                "spot" => {
                    match spot(goal, async |questions, grid| {
                        ask(questions, Some(grid)).await
                    })
                    .await?
                    {
                        Ok((x, y)) => return Ok(Decision::Act(Box::new(Action::Spot(x, y)))),
                        Err(reason) if reason == NONE_FITS => {
                            operations.retain(|other| other.name != name);
                            continue;
                        }
                        Err(reason) => {
                            return Ok(Decision::Unsure(format!("where to press: {reason}")));
                        }
                    }
                }
                "stuck" => return Ok(Decision::Stuck),
                "enter" => return Ok(Decision::Act(Box::new(Action::Enter))),
                "escape" => return Ok(Decision::Act(Box::new(Action::Escape))),
                "scroll" => return Ok(Decision::Act(Box::new(Action::Scroll))),
                "wait" => return Ok(Decision::Act(Box::new(Action::Wait))),
                _ => {}
            }
            let descriptions: Vec<String> = operation
                .targets
                .iter()
                .map(|element| element.describe())
                .collect();
            let text =
                format!("Which of these is the one to {name} to get closest to this goal: {goal}");
            let mut plain = async |questions| ask(questions, None).await;
            match agree(&mut plain, name, &text, &descriptions, TARGET_AT).await? {
                Ok(index) => break (operation, index),
                Err(reason) if reason == NONE_FITS => {
                    operations.retain(|other| other.name != name);
                }
                Err(reason) => return Ok(Decision::Unsure(format!("what to {name}: {reason}"))),
            }
        };
        let name = operation.name;
        let element = operation.targets[index];
        match name {
            "press" => return Ok(Decision::Act(Box::new(Action::Press(element.clone())))),
            "hover" => return Ok(Decision::Act(Box::new(Action::Hover(element.clone())))),
            _ => {}
        }
        let values = self.values(name, element);
        let mut text = format!(
            "Which of these to {name} in {} to get closest to this goal: {goal}",
            element.describe()
        );
        if values.len() > MAX_ELEMENTS {
            write!(
                text,
                " ({} more are not listed)",
                values.len() - MAX_ELEMENTS
            )
            .ok();
        }
        let id = format!("{name}_{}", forward(index));
        let offered = &values[..values.len().min(MAX_ELEMENTS)];
        let mut plain = async |questions| ask(questions, None).await;
        let value = match agree(&mut plain, &id, &text, offered, TARGET_AT).await? {
            Ok(value) => value,
            Err(reason) => {
                return Ok(Decision::Unsure(format!(
                    "what to {name} in {}: {reason}",
                    element.describe()
                )));
            }
        };
        Ok(Decision::Act(Box::new(match &element.kind {
            Kind::Choose(options) => Action::Choose(element.clone(), options[value].clone()),
            _ => Action::Type(element.clone(), self.inputs[value].clone()),
        })))
    }
}

pub(super) fn screen_state(screen: &Screen) -> String {
    if screen.tail && cut(screen) {
        let skip = screen.text.chars().count() - SCREEN_CHARS;
        return format!(
            "Screen, of which only the last {SCREEN_CHARS} characters are shown here:\n{}",
            screen.text.chars().skip(skip).collect::<String>()
        );
    }
    let mut state = format!("Screen:\n{}", clip(&screen.text, SCREEN_CHARS));
    if cut(screen) {
        write!(
            state,
            "\n(The screen goes on: only its first {SCREEN_CHARS} characters are shown here. Scrolling does not show the rest here, but its controls can still be used by name.)"
        )
        .ok();
    }
    state
}

pub(super) fn cut(screen: &Screen) -> bool {
    screen.text.chars().nth(SCREEN_CHARS).is_some()
}

pub(super) fn cut_note(tail: bool) -> String {
    let part = if tail { "last" } else { "first" };
    format!("the model saw only the {part} {SCREEN_CHARS} characters of the screen")
}

pub(super) async fn recognize(
    recorded: &Element,
    screen: &Screen,
    mut ask: impl AsyncFnMut(Questions) -> Result<Answers>,
) -> Result<Option<Element>> {
    let candidates: Vec<&Element> = screen
        .elements
        .iter()
        .filter(|element| discriminant(&element.kind) == discriminant(&recorded.kind))
        .filter(|_| recorded.among == 1)
        .collect();
    let descriptions: Vec<String> = candidates
        .iter()
        .map(|element| element.describe())
        .collect();
    let text = format!(
        "Which of these is the same control as the one previously seen as: {}",
        recorded.describe()
    );
    let index = agree(&mut ask, "recorded", &text, &descriptions, RECOGNIZED_AT).await?;
    Ok(index.ok().map(|index| candidates[index].clone()))
}

pub(super) async fn pick(
    what: &str,
    candidates: &[String],
    mut ask: impl AsyncFnMut(Questions) -> Result<Answers>,
) -> Result<Result<usize, String>> {
    let text = format!("Which of these is {what}");
    agree(&mut ask, "extract", &text, candidates, TARGET_AT).await
}

pub(super) fn claim(statement: &str) -> Questions {
    Questions::from([
        (
            "holds".into(),
            Question::yesno(&format!("The screen shows that this is true: {statement}")),
        ),
        (
            "sight".into(),
            Question::yesno(&format!(
                "Does this claim depend on the look of the screen, such as a colour, a position, a size or a picture, rather than only on its text? Claim: {statement}"
            )),
        ),
    ])
}

pub(super) fn needs_sight(answers: &Answers) -> bool {
    answers
        .get("sight")
        .and_then(Answer::yes)
        .is_some_and(|sight| sight >= SIGHT_AT)
}

pub(super) fn verdict(answers: &Answers) -> Result<(Verdict, f64)> {
    let holds = answers
        .get("holds")
        .and_then(Answer::yes)
        .context("the decision model did not answer the claim with yes or no")?;
    let verdict = if holds < 1.0 - HOLDS_AT {
        Verdict::Contradicted
    } else if holds >= HOLDS_AT {
        Verdict::Holds
    } else {
        Verdict::Inconclusive
    };
    Ok((verdict, holds))
}

async fn agree(
    ask: &mut impl AsyncFnMut(Questions) -> Result<Answers>,
    id: &str,
    text: &str,
    descriptions: &[String],
    at: f64,
) -> Result<Result<usize, String>> {
    Ok(zone(ask, id, text, descriptions, at, |a, b| a == b)
        .await?
        .map(|cells| cells[0]))
}

fn position(order: usize, label: &str, count: usize) -> Option<usize> {
    label
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .filter(|&n| n < count)
        .map(|n| if order == 0 { n } else { count - 1 - n })
}

async fn zone(
    ask: &mut impl AsyncFnMut(Questions) -> Result<Answers>,
    id: &str,
    text: &str,
    descriptions: &[String],
    at: f64,
    near: impl Fn(usize, usize) -> bool,
) -> Result<Result<Vec<usize>, String>> {
    if descriptions.is_empty() {
        return Ok(Err("there is nothing to pick from".into()));
    }
    let count = descriptions.len();
    let orders = count.min(2);
    let (mut cells, mut named) = (Vec::new(), Vec::new());
    for (order, (id, question)) in both_orders(id, text, descriptions).take(orders).enumerate() {
        let reply = ask(Questions::from([(id.clone(), question)])).await?;
        let Some(chances) = chances_in(reply.get(&id)) else {
            return Ok(Err(format!("unsure ({})", shown(None))));
        };
        let cell = |slot: usize| position(order, chances[slot].0, count);
        let strong: Vec<(usize, usize)> = (0..chances.len())
            .filter(|&slot| chances[slot].1 >= MARGIN)
            .filter_map(|slot| Some((slot, cell(slot)?)))
            .collect();
        let mut zones: Vec<Vec<usize>> = (0..chances.len()).map(|slot| vec![slot]).collect();
        for (first, &(a, x)) in strong.iter().enumerate() {
            zones.extend(
                strong[first + 1..]
                    .iter()
                    .filter(|&&(_, y)| near(x, y))
                    .map(|&(b, _)| vec![a, b]),
            );
        }
        let mass = |zone: &[usize]| zone.iter().map(|&slot| chances[slot].1).sum::<f64>();
        let lead = |zone: &[usize]| {
            mass(zone)
                - zones
                    .iter()
                    .filter(|other| other.iter().all(|slot| !zone.contains(slot)))
                    .map(|other| mass(other))
                    .fold(0.0, f64::max)
        };
        let Some(chosen) = zones.iter().find(|zone| {
            zone.iter().all(|&slot| cell(slot).is_some())
                && mass(zone) >= at
                && lead(zone) >= MARGIN
        }) else {
            let ((label, top), ahead) = (chances[0], lead(&[0]));
            let sure = top >= at && ahead >= MARGIN;
            if order == 0 && label == NONE && sure {
                return Ok(Err(NONE_FITS.into()));
            }
            let label =
                cell(0).map_or_else(|| label.to_owned(), |index| descriptions[index].clone());
            if order > 0 && sure {
                return Ok(Err(format!(
                    "the two orderings disagree ({} and {label})",
                    named[0]
                )));
            }
            return Ok(Err(format!(
                "unsure ({})",
                shown(Some((label, top, ahead)))
            )));
        };
        let chosen: Vec<usize> = chosen.iter().filter_map(|&slot| cell(slot)).collect();
        named.push(
            chosen
                .iter()
                .map(|&index| descriptions[index].as_str())
                .collect::<Vec<_>>()
                .join(" or "),
        );
        cells.extend(chosen);
    }
    if cells.iter().any(|&a| cells.iter().any(|&b| !near(a, b))) {
        return Ok(Err(format!(
            "the two orderings disagree ({} and {})",
            named[0],
            named[orders - 1]
        )));
    }
    Ok(Ok(cells))
}

fn both_orders(
    id: &str,
    text: &str,
    descriptions: &[String],
) -> impl Iterator<Item = (String, Question)> {
    let count = descriptions.len();
    let options = |label: &dyn Fn(usize) -> String| -> BTreeMap<String, String> {
        descriptions
            .iter()
            .enumerate()
            .map(|(index, description)| (label(index), description.clone()))
            .chain([(NONE.into(), "None of these fits".into())])
            .collect()
    };
    [
        (id.to_string(), options(&|index| forward(index))),
        (
            format!("{id}{REVERSED}"),
            options(&|index| forward(count - 1 - index)),
        ),
    ]
    .into_iter()
    .map(move |(id, options)| {
        (
            id,
            Question::Pick {
                text: text.into(),
                options,
            },
        )
    })
}

fn chances_in(answer: Option<&Answer>) -> Option<Vec<(&str, f64)>> {
    let mut chances: Vec<(&str, f64)> = match answer? {
        Answer::Pick { probabilities, .. } if !probabilities.is_empty() => probabilities
            .iter()
            .map(|(label, probability)| (label.as_str(), *probability))
            .collect(),
        Answer::Pick {
            pick,
            confidence: Some(confidence),
            ..
        } => vec![(pick.as_str(), *confidence)],
        _ => return None,
    };
    chances.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(chances)
}

fn ranked(answer: Option<&Answer>) -> Option<(String, f64, f64)> {
    let chances = chances_in(answer)?;
    let (pick, top) = chances[0];
    let next = chances.get(1).map_or(0.0, |(_, chance)| *chance);
    Some((pick.to_owned(), top, top - next))
}

fn shown(ranking: Option<(String, f64, f64)>) -> String {
    ranking.map_or("no answer".into(), |(pick, top, margin)| {
        format!("{pick} at {top:.2}, ahead by {margin:.2}")
    })
}

pub(super) fn forward(index: usize) -> String {
    format!("{:03}", index + 1)
}

pub(super) async fn spot(
    goal: &str,
    mut ask: impl AsyncFnMut(Questions, Grid) -> Result<Answers>,
) -> Result<Result<(f64, f64), String>> {
    let mut area = Area {
        left: 0.0,
        top: 0.0,
        width: 1.0,
        height: 1.0,
    };
    for (id, (columns, rows), shows) in [
        ("spot", COARSE, "the screen in view"),
        (
            "spot_closer",
            FINE,
            "an enlarged part of the screen around the cell chosen before",
        ),
    ] {
        let descriptions: Vec<String> = (1..=rows)
            .flat_map(|row| {
                (1..=columns)
                    .map(move |column| format!("the cell in row {row} and column {column}"))
            })
            .collect();
        let text = format!(
            "The screenshot shows {shows}, under a grid of cells numbered at their top left corner. Which cell holds the spot to press to get closest to this goal: {goal}"
        );
        let mut asked = async |questions: Questions| {
            let reversed = questions.keys().any(|id| id.ends_with(REVERSED));
            ask(
                questions,
                Grid {
                    area,
                    columns,
                    rows,
                    reversed,
                },
            )
            .await
        };
        let near = |a: usize, b: usize| {
            (a % columns).abs_diff(b % columns) <= 1 && (a / columns).abs_diff(b / columns) <= 1
        };
        let cells = match zone(&mut asked, id, &text, &descriptions, TARGET_AT, near).await? {
            Ok(cells) => cells,
            Err(reason) => return Ok(Err(reason)),
        };
        let span = |of: fn(usize, usize) -> usize| {
            let values = cells.iter().map(|&cell| of(cell, columns));
            (values.clone().min().unwrap_or(0), values.max().unwrap_or(0))
        };
        let ((first, last), (top_row, bottom_row)) = (
            span(|cell, columns| cell % columns),
            span(|cell, columns| cell / columns),
        );
        let (width, height) = (area.width / columns as f64, area.height / rows as f64);
        let (left, top) = (
            area.left + first as f64 * width,
            area.top + top_row as f64 * height,
        );
        let (width, height) = (
            (last - first + 1) as f64 * width,
            (bottom_row - top_row + 1) as f64 * height,
        );
        area = if id == "spot" {
            let grow = |start: f64, size: f64| {
                let grown = size * CLOSER;
                (
                    (start - (grown - size) / 2.0).clamp(0.0, 1.0 - grown),
                    grown,
                )
            };
            let ((left, width), (top, height)) = (grow(left, width), grow(top, height));
            Area {
                left,
                top,
                width,
                height,
            }
        } else {
            Area {
                left,
                top,
                width,
                height,
            }
        };
    }
    Ok(Ok((
        area.left + area.width / 2.0,
        area.top + area.height / 2.0,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(pick: &str, probabilities: &[(&str, f64)]) -> Answer {
        Answer::Pick {
            pick: pick.into(),
            confidence: None,
            probabilities: probabilities
                .iter()
                .map(|(label, p)| ((*label).into(), *p))
                .collect(),
        }
    }

    fn screen() -> Screen {
        Screen {
            text: "- button \"Help\"\n- button \"Pay\"\n".into(),
            elements: vec![
                Element::new("button", "Help", Kind::Press),
                Element::new("button", "Pay", Kind::Press),
            ],
            ..Screen::default()
        }
    }

    async fn decide(step: &Step<'_>, answers: &Answers) -> (Decision, Vec<Questions>) {
        let mut asked = Vec::new();
        let decision = step
            .decide(false, async |questions, _| {
                asked.push(questions);
                Ok(answers.clone())
            })
            .await
            .unwrap();
        (decision, asked)
    }

    async fn recognized(recorded: &Element, screen: &Screen) -> (Option<Element>, usize) {
        let mut asked = 0;
        let element = recognize(recorded, screen, async |_| {
            asked += 1;
            Ok(Answers::new())
        })
        .await
        .unwrap();
        (element, asked)
    }

    #[test]
    fn the_state_says_which_files_and_folders_the_goal_names() {
        let folder = std::env::temp_dir().join(format!("iu2u-state-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let screen = screen();
        let files = [
            Files {
                key: "panel".into(),
                paths: vec![folder.join("panel.zip").display().to_string()],
                zip: false,
            },
            Files {
                key: "project".into(),
                paths: vec![folder.display().to_string()],
                zip: false,
            },
        ];
        let state = Step::new("attach the panel", &screen, &[]).state(&[], &files);
        std::fs::remove_dir_all(&folder).ok();
        let folder = folder.file_name().unwrap().to_string_lossy();
        assert!(
            state.starts_with(&format!(
                "Goal: attach the panel\nFiles the goal names, for a file chooser or picker:\n- panel: the file panel.zip\n- project: the folder {folder}\n"
            )),
            "{state}"
        );
    }

    #[tokio::test]
    async fn the_same_target_in_both_orders_is_acted_on() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        let answers = Answers::from([
            (
                "operation".into(),
                pick("press", &[("press", 0.9), ("done", 0.1)]),
            ),
            (
                "press".into(),
                pick("002", &[("001", 0.1), ("002", 0.85), ("none", 0.05)]),
            ),
            (
                "press_reversed".into(),
                pick("001", &[("001", 0.8), ("002", 0.15), ("none", 0.05)]),
            ),
        ]);
        assert_eq!(
            decide(&step, &answers).await.0,
            Decision::Act(Box::new(Action::Press(screen.elements[1].clone())))
        );
    }

    async fn spotted(answers: &[(&str, Answer)]) -> (Decision, Vec<Grid>) {
        let seeing = Screen {
            spots: true,
            ..screen()
        };
        let step = Step::new("press Madrid on the map", &seeing, &[]);
        let mut grids = Vec::new();
        let decision = step
            .decide(false, async |questions: Questions, grid| {
                grids.extend(grid);
                let id = questions.keys().next().cloned().unwrap_or_default();
                let answer = answers.iter().find(|(asked, _)| *asked == id).unwrap();
                Ok(Answers::from([(id, answer.1.clone())]))
            })
            .await
            .unwrap();
        (decision, grids)
    }

    fn spotted_at(decision: Decision) -> (f64, f64) {
        let Decision::Act(action) = decision else {
            panic!("{decision:?}");
        };
        let Action::Spot(x, y) = *action else {
            panic!("{action:?}");
        };
        (x, y)
    }

    #[tokio::test]
    async fn a_spot_is_found_on_a_grid_and_then_on_a_closer_one() {
        assert!(
            !Step::new("pay", &screen(), &[])
                .operations
                .iter()
                .any(|operation| operation.name == "spot")
        );
        let (decision, grids) = spotted(&[
            ("operation", pick("spot", &[("spot", 0.9)])),
            ("spot", pick("019", &[("019", 0.9)])),
            ("spot_reversed", pick("030", &[("030", 0.9)])),
            ("spot_closer", pick("001", &[("001", 0.9)])),
            ("spot_closer_reversed", pick("064", &[("064", 0.9)])),
        ])
        .await;
        let (x, y) = spotted_at(decision);
        assert!(
            (x - 0.230_468_75).abs() < 1e-9 && (y - 0.307_291_666).abs() < 1e-6,
            "{x} {y}"
        );
        assert_eq!(
            grids
                .iter()
                .map(|grid| (grid.columns, grid.rows, grid.reversed))
                .collect::<Vec<_>>(),
            [(8, 6, false), (8, 6, true), (8, 8, false), (8, 8, true)]
        );
        assert_eq!(grids[0].area.width, 1.0);
        assert!(
            (grids[2].area.left - 0.218_75).abs() < 1e-9
                && (grids[2].area.width - 0.187_5).abs() < 1e-9
        );
    }

    #[tokio::test]
    async fn a_spot_on_the_line_between_cells_is_pressed_on_both() {
        let (decision, grids) = spotted(&[
            ("operation", pick("spot", &[("spot", 0.9)])),
            (
                "spot",
                pick(
                    "001",
                    &[("001", 0.27), ("019", 0.26), ("020", 0.25), ("none", 0.22)],
                ),
            ),
            ("spot_reversed", pick("030", &[("030", 0.9)])),
            ("spot_closer", pick("001", &[("001", 0.9)])),
            (
                "spot_closer_reversed",
                pick("063", &[("063", 0.5), ("064", 0.5)]),
            ),
        ])
        .await;
        assert!(
            (grids[2].area.left - 0.187_5).abs() < 1e-9
                && (grids[2].area.width - 0.375).abs() < 1e-9,
            "{:?}",
            grids[2].area
        );
        let (x, y) = spotted_at(decision);
        assert!(
            (x - 0.234_375).abs() < 1e-9 && (y - 0.307_291_666).abs() < 1e-6,
            "{x} {y}"
        );
    }

    #[tokio::test]
    async fn a_spot_the_model_is_not_sure_of_is_not_pressed() {
        let operation = ("operation", pick("spot", &[("spot", 0.9)]));
        for (answers, reason) in [
            (
                vec![
                    ("spot", pick("019", &[("019", 0.9)])),
                    ("spot_reversed", pick("001", &[("001", 0.9)])),
                ],
                "the two orderings disagree (the cell in row 3 and column 3 and the cell in row 6 and column 8)",
            ),
            (
                vec![
                    ("spot", pick("019", &[("019", 0.9)])),
                    ("spot_reversed", pick("none", &[("none", 0.9)])),
                ],
                "the two orderings disagree (the cell in row 3 and column 3 and none)",
            ),
            (
                vec![(
                    "spot",
                    pick(
                        "none",
                        &[("none", 0.4), ("019", 0.25), ("020", 0.2), ("001", 0.15)],
                    ),
                )],
                "unsure (none at 0.40, ahead by -0.05)",
            ),
            (
                vec![(
                    "spot",
                    pick("019", &[("019", 0.44), ("none", 0.35), ("020", 0.02)]),
                )],
                "unsure (the cell in row 3 and column 3 at 0.44, ahead by 0.09)",
            ),
            (
                vec![(
                    "spot",
                    pick(
                        "019",
                        &[("019", 0.26), ("020", 0.26), ("001", 0.24), ("002", 0.24)],
                    ),
                )],
                "unsure (the cell in row 3 and column 3 at 0.26, ahead by -0.22)",
            ),
        ] {
            let (decision, _) = spotted(&[[operation.clone()].as_slice(), &answers].concat()).await;
            assert!(
                matches!(&decision, Decision::Unsure(found) if *found == format!("where to press: {reason}")),
                "{decision:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_spot_no_cell_fits_is_asked_about_again_without_spots() {
        let seeing = Screen {
            spots: true,
            ..screen()
        };
        let step = Step::new("press the pin below the map", &seeing, &[]);
        let mut asked = Vec::new();
        let decision = step
            .decide(false, async |questions: Questions, _| {
                let (id, question) = questions.into_iter().next().unwrap();
                let label = match &question {
                    Question::Pick { options, .. } if id == "operation" => {
                        if options.contains_key("spot") {
                            "spot"
                        } else {
                            "scroll"
                        }
                    }
                    _ => "none",
                };
                asked.push(id.clone());
                Ok(Answers::from([(id, pick(label, &[(label, 0.9)]))]))
            })
            .await
            .unwrap();
        assert_eq!(decision, Decision::Act(Box::new(Action::Scroll)));
        assert_eq!(asked, ["operation", "spot", "operation"]);
    }

    #[tokio::test]
    async fn orderings_that_disagree_or_a_close_call_leave_the_step_unsure() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        let operation = ("operation".to_string(), pick("press", &[("press", 0.9)]));
        let disagree = Answers::from([
            operation.clone(),
            ("press".into(), pick("002", &[("002", 0.9)])),
            ("press_reversed".into(), pick("002", &[("002", 0.9)])),
        ]);
        let close = Answers::from([(
            "operation".into(),
            pick("press", &[("press", 0.5), ("type", 0.45)]),
        )]);
        assert!(
            matches!(decide(&step, &disagree).await.0, Decision::Unsure(reason) if reason.contains(r#"disagree (button "Pay" and button "Help")"#))
        );
        assert!(matches!(decide(&step, &close).await.0, Decision::Unsure(_)));
    }

    #[tokio::test]
    async fn a_goal_is_out_of_reach_only_when_the_model_is_as_sure_as_a_claim_needs() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        let stuck = |sure: f64| {
            Answers::from([(
                "operation".into(),
                pick("stuck", &[("stuck", sure), ("done", 1.0 - sure)]),
            )])
        };
        assert!(
            matches!(decide(&step, &stuck(0.6)).await.0, Decision::Unsure(reason) if reason.contains("stuck at 0.60"))
        );
        assert_eq!(decide(&step, &stuck(HOLDS_AT)).await.0, Decision::Stuck);
    }

    #[tokio::test]
    async fn a_goal_done_without_acting_is_done_only_when_the_screen_shows_it_reached() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        let reached = async |acted: bool, holds: f64| {
            let mut asked = Vec::new();
            let decision = step
                .decide(acted, async |questions: Questions, _| {
                    let (id, question) = questions.into_iter().next().unwrap();
                    let answer = match &question {
                        Question::Pick { options, .. } if options.contains_key("done") => {
                            pick("done", &[("done", 0.9)])
                        }
                        Question::Pick { .. } => pick("stuck", &[("stuck", 0.9)]),
                        _ => Answer::Yesno { yes: holds },
                    };
                    asked.push(id.clone());
                    Ok(Answers::from([(id, answer)]))
                })
                .await
                .unwrap();
            (decision, asked)
        };
        assert_eq!(
            reached(true, 0.1).await,
            (Decision::Done, vec!["operation".into()])
        );
        assert_eq!(
            reached(false, HOLDS_AT).await,
            (Decision::Done, vec!["operation".into(), "holds".into()])
        );
        assert!(
            matches!(reached(false, 0.5).await.0, Decision::Unsure(reason) if reason == "whether the goal is reached (at 0.50)")
        );
        assert_eq!(
            reached(false, 0.1).await,
            (
                Decision::Stuck,
                vec!["operation".into(), "holds".into(), "operation".into()]
            )
        );
    }

    #[tokio::test]
    async fn a_target_the_first_order_does_not_settle_is_not_asked_about_again() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        let answers = Answers::from([
            ("operation".into(), pick("press", &[("press", 0.9)])),
            ("press".into(), pick("002", &[("002", 0.2)])),
        ]);
        let (decision, asked) = decide(&step, &answers).await;
        assert!(
            matches!(decision, Decision::Unsure(said) if said.contains(r#"unsure (button "Pay" at 0.20"#))
        );
        assert_eq!(asked.len(), 2);
    }

    #[tokio::test]
    async fn an_operation_none_of_whose_targets_fits_is_asked_about_again_without_it() {
        let screen = Screen {
            elements: vec![
                Element::new("combobox", "Rows", Kind::Choose(vec!["20".into()])),
                Element::new("combobox", "Project", Kind::Press),
            ],
            ..Screen::default()
        };
        let step = Step::new("choose Data Warehouse in the project filter", &screen, &[]);
        let mut asked = Vec::new();
        let decision = step
            .decide(false, async |questions: Questions, _| {
                let answer = match questions.iter().next() {
                    Some((id, Question::Pick { options, .. })) if id == "operation" => {
                        if options.contains_key("choose") {
                            pick("choose", &[("choose", 0.9)])
                        } else {
                            pick("press", &[("press", 0.9)])
                        }
                    }
                    Some((id, _)) if id == "choose" => pick("none", &[("none", 0.9)]),
                    _ => pick("001", &[("001", 0.9)]),
                };
                asked.push(questions.keys().next().cloned().unwrap_or_default());
                Ok(Answers::from([(asked.last().unwrap().clone(), answer)]))
            })
            .await
            .unwrap();
        assert_eq!(
            decision,
            Decision::Act(Box::new(Action::Press(screen.elements[1].clone())))
        );
        assert_eq!(asked, ["operation", "choose", "operation", "press"]);
    }

    #[tokio::test]
    async fn each_request_asks_one_question_and_secrets_stay_hidden() {
        let screen = Screen {
            elements: vec![
                Element::new("textbox", "Password", Kind::Type),
                Element::new("textbox", "Email", Kind::Type),
            ],
            ..Screen::default()
        };
        let inputs = [
            ("password", "hunter2", true),
            ("email", "qa@example.com", false),
        ]
        .map(|(key, text, secret)| Input {
            key: key.into(),
            text: text.into(),
            secret,
        });
        let step = Step::new("sign in", &screen, &inputs);
        let picked = |id: &str, label: &str| (id.to_string(), pick(label, &[(label, 0.9)]));
        let answers = Answers::from([
            picked("operation", "type"),
            picked("type", "001"),
            picked("type_reversed", "002"),
            picked("type_001", "001"),
            picked("type_001_reversed", "002"),
        ]);
        let (decision, asked) = decide(&step, &answers).await;
        let ids: Vec<Vec<&String>> = asked
            .iter()
            .map(|questions| questions.keys().collect())
            .collect();
        assert_eq!(
            ids,
            [
                ["operation"],
                ["type"],
                ["type_reversed"],
                ["type_001"],
                ["type_001_reversed"]
            ]
        );
        let questions = format!("{asked:?}");
        assert!(questions.contains("(secret \\\"password\\\")") && !questions.contains("hunter2"));
        assert_eq!(
            decision,
            Decision::Act(Box::new(Action::Type(
                screen.elements[0].clone(),
                inputs[0].clone()
            )))
        );
    }

    #[tokio::test]
    async fn a_lone_option_is_asked_about_once() {
        let screen = Screen {
            elements: vec![Element::new("button", "Pay", Kind::Press)],
            ..Screen::default()
        };
        let step = Step::new("pay", &screen, &[]);
        let answers = Answers::from([
            ("operation".into(), pick("press", &[("press", 0.9)])),
            ("press".into(), pick("001", &[("001", 0.9)])),
        ]);
        let (decision, asked) = decide(&step, &answers).await;
        assert_eq!(
            decision,
            Decision::Act(Box::new(Action::Press(screen.elements[0].clone())))
        );
        assert_eq!(asked.len(), 2);
    }

    #[tokio::test]
    async fn every_option_of_every_drop_down_can_be_chosen() {
        let countries = (0..MAX_ELEMENTS).map(|n| format!("Country {n}")).collect();
        let sizes = vec!["S".to_string(), "M".into(), "XXL".into()];
        let screen = Screen {
            elements: vec![
                Element::new("combobox", "Size", Kind::Choose(sizes)),
                Element::new("combobox", "Country", Kind::Choose(countries)),
            ],
            ..Screen::default()
        };
        let step = Step::new("pick the last country", &screen, &[]);
        let last = forward(MAX_ELEMENTS - 1);
        let answers = Answers::from([
            ("operation".into(), pick("choose", &[("choose", 0.9)])),
            ("choose".into(), pick("002", &[("002", 0.9)])),
            ("choose_reversed".into(), pick("001", &[("001", 0.9)])),
            ("choose_002".into(), pick(&last, &[(&last, 0.9)])),
            ("choose_002_reversed".into(), pick("001", &[("001", 0.9)])),
        ]);
        let (decision, asked) = decide(&step, &answers).await;
        let offered = match &asked[3]["choose_002"] {
            Question::Pick { options, .. } => options.len(),
            _ => 0,
        };
        assert_eq!(offered, MAX_ELEMENTS + 1);
        assert_eq!(
            decision,
            Decision::Act(Box::new(Action::Choose(
                screen.elements[1].clone(),
                format!("Country {}", MAX_ELEMENTS - 1)
            )))
        );
    }

    #[test]
    fn claims_fall_into_three_bands() {
        let band = |holds: f64| {
            verdict(&Answers::from([(
                "holds".into(),
                Answer::Yesno { yes: holds },
            )]))
            .unwrap()
            .0
        };
        assert_eq!(band(0.75), Verdict::Holds);
        assert_eq!(band(0.74), Verdict::Inconclusive);
        assert_eq!(band(0.25), Verdict::Inconclusive);
        assert_eq!(band(0.24), Verdict::Contradicted);
    }

    #[test]
    fn a_claim_answered_with_anything_but_yes_or_no_is_an_error() {
        let answers = Answers::from([("holds".into(), pick("001", &[("001", 0.9)]))]);
        assert!(verdict(&answers).is_err());
    }

    #[test]
    fn the_model_sees_at_most_a_clipped_screen() {
        let long = Screen {
            text: "x".repeat(SCREEN_CHARS * 2),
            ..Screen::default()
        };
        let state = screen_state(&long);
        assert!(state.contains(&format!("{}…", "x".repeat(SCREEN_CHARS))));
        assert!(
            !state.contains(&"x".repeat(SCREEN_CHARS + 1)) && state.contains("The screen goes on")
        );
    }

    #[tokio::test]
    async fn a_control_with_no_candidates_is_not_asked_about() {
        let field = Element::new("textbox", "Email", Kind::Type);
        assert_eq!(recognized(&field, &screen()).await, (None, 0));
        let twin = Element {
            among: 3,
            ..Element::new("button", "Delete", Kind::Press)
        };
        assert_eq!(recognized(&twin, &screen()).await, (None, 0));
        let drop_down = Screen {
            elements: vec![Element::new(
                "combobox",
                "Pay",
                Kind::Choose(vec!["Card".into()]),
            )],
            ..Screen::default()
        };
        let button = Element::new("button", "Pay", Kind::Press);
        assert_eq!(recognized(&button, &drop_down).await, (None, 0));
    }
}
