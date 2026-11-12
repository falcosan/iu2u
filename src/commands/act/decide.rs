use std::collections::BTreeMap;
use std::fmt::Write;
use std::mem::discriminant;

use anyhow::{Context, Result};

use super::screen::{Element, Kind, MAX_ELEMENTS, Screen};
use super::surface::Action;
use crate::clip;
use crate::model::{Answer, Answers, Question, Questions};

pub(super) const SCREEN_CHARS: usize = 6000;
pub(super) const HOLDS_AT: f64 = 0.75;
const OPERATION_AT: f64 = 0.5;
const TARGET_AT: f64 = 0.3;
const RECOGNIZED_AT: f64 = 0.6;
const MARGIN: f64 = 0.1;
const NONE: &str = "none";
const REVERSED: &str = "_reversed";

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
        let press = elements
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
            ("press", "Press, click or toggle one control", press),
            ("type", "Replace the whole text of a field with one of the given values", type_),
            ("choose", "Choose an option in a drop-down list", choose),
        ]
        .into_iter()
        .map(|(name, description, targets)| Operation { name, description, targets })
        .filter(|operation| !operation.targets.is_empty())
        .chain(
            [
                (focused_field, "enter", "Press Enter in the focused field, for example to submit it"),
                (true, "scroll", "Scroll down to reveal more of the screen"),
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

    pub(super) fn state(&self, history: &[String]) -> String {
        let mut state = format!("Goal: {}\n", self.goal);
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
        mut ask: impl AsyncFnMut(Questions) -> Result<Answers>,
    ) -> Result<Decision> {
        let goal = self.goal;
        let answers = ask(Questions::from([(
            "operation".into(),
            Question::Pick {
                text: format!("Which operation brings the screen closest to this goal: {goal}"),
                options: self
                    .operations
                    .iter()
                    .map(|operation| (operation.name.into(), operation.description.into()))
                    .collect(),
            },
        )]))
        .await?;
        let chosen = match ranked(answers.get("operation")) {
            Some((pick, top, margin)) if top >= OPERATION_AT && margin >= MARGIN => pick,
            ranking => {
                return Ok(Decision::Unsure(format!(
                    "which operation to use ({})",
                    shown(ranking)
                )));
            }
        };
        let Some(operation) = self
            .operations
            .iter()
            .find(|operation| operation.name == chosen)
        else {
            let names: Vec<&str> = self
                .operations
                .iter()
                .map(|operation| operation.name)
                .collect();
            return Ok(Decision::Unsure(format!(
                "which operation to use: {chosen:?} is not one of {names:?}"
            )));
        };
        let name = operation.name;
        match name {
            "done" => return Ok(Decision::Done),
            "stuck" => return Ok(Decision::Stuck),
            "enter" => return Ok(Decision::Act(Box::new(Action::Enter))),
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
        let index = match agree(&mut ask, name, &text, &descriptions, TARGET_AT).await? {
            Ok(index) => index,
            Err(reason) => return Ok(Decision::Unsure(format!("what to {name}: {reason}"))),
        };
        let element = operation.targets[index];
        if name == "press" {
            return Ok(Decision::Act(Box::new(Action::Press(element.clone()))));
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
        let value = match agree(&mut ask, &id, &text, offered, TARGET_AT).await? {
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
    let mut state = format!("Screen:\n{}", clip(&screen.text, SCREEN_CHARS));
    if cut(screen) {
        write!(
            state,
            "\n(The screen goes on: only its first {SCREEN_CHARS} characters are shown.)"
        )
        .ok();
    }
    state
}

pub(super) fn cut(screen: &Screen) -> bool {
    screen.text.chars().nth(SCREEN_CHARS).is_some()
}

pub(super) fn cut_note() -> String {
    format!("the model saw only the first {SCREEN_CHARS} characters of the screen")
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

pub(super) fn claim(statement: &str) -> Questions {
    Questions::from([(
        "holds".into(),
        Question::yesno(&format!("The screen shows that this is true: {statement}")),
    )])
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
    if descriptions.is_empty() {
        return Ok(Err("there is nothing to pick from".into()));
    }
    let count = descriptions.len();
    let orders = count.min(2);
    let mut picks = Vec::new();
    for (id, question) in both_orders(id, text, descriptions).take(orders) {
        let reply = ask(Questions::from([(id.clone(), question)])).await?;
        match ranked(reply.get(&id)) {
            Some((pick, top, margin)) if top >= at && margin >= MARGIN => {
                if pick == NONE && picks.is_empty() {
                    return Ok(Err("none of the options fits".into()));
                }
                picks.push(pick);
            }
            ranking => return Ok(Err(format!("unsure ({})", shown(ranking)))),
        }
    }
    let index = |label: &str| {
        label
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .filter(|&n| n < count)
    };
    let (first, last) = (&picks[0], &picks[orders - 1]);
    Ok(match (index(first), index(last).map(|n| count - 1 - n)) {
        (Some(a), Some(b)) if a == b => Ok(a),
        _ => Err(format!("the two orderings disagree ({first} and {last})")),
    })
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

fn ranked(answer: Option<&Answer>) -> Option<(String, f64, f64)> {
    let Some(Answer::Pick {
        pick,
        confidence,
        probabilities,
    }) = answer
    else {
        return None;
    };
    let top = probabilities.get(pick).copied().or(*confidence)?;
    let second = probabilities
        .iter()
        .filter(|(label, _)| *label != pick)
        .map(|(_, probability)| *probability)
        .fold(0.0, f64::max);
    Some((pick.clone(), top, top - second))
}

fn shown(ranking: Option<(String, f64, f64)>) -> String {
    ranking.map_or("no answer".into(), |(pick, top, margin)| {
        format!("{pick} at {top:.2}, ahead by {margin:.2}")
    })
}

fn forward(index: usize) -> String {
    format!("{:03}", index + 1)
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
            .decide(async |questions| {
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
            matches!(decide(&step, &disagree).await.0, Decision::Unsure(reason) if reason.contains("disagree"))
        );
        assert!(matches!(decide(&step, &close).await.0, Decision::Unsure(_)));
    }

    #[tokio::test]
    async fn a_target_the_first_order_does_not_settle_is_not_asked_about_again() {
        let screen = screen();
        let step = Step::new("pay", &screen, &[]);
        for (first, reason) in [
            (pick("none", &[("none", 0.9)]), "none of the options fits"),
            (pick("002", &[("002", 0.2)]), "unsure"),
        ] {
            let answers = Answers::from([
                ("operation".into(), pick("press", &[("press", 0.9)])),
                ("press".into(), first),
            ]);
            let (decision, asked) = decide(&step, &answers).await;
            assert!(matches!(decision, Decision::Unsure(said) if said.contains(reason)));
            assert_eq!(asked.len(), 2);
        }
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
