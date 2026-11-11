use std::collections::BTreeMap;
use std::fmt::Write;

use super::screen::{Element, Kind, MAX_ELEMENTS, Screen};
use super::surface::Action;
use crate::clip;
use crate::model::{Answer, Answers, Question, Questions};

pub(super) const SCREEN_CHARS: usize = 6000;
const OPERATION_AT: f64 = 0.5;
const TARGET_AT: f64 = 0.3;
const RECOGNIZED_AT: f64 = 0.6;
const MARGIN: f64 = 0.1;
const HOLDS_AT: f64 = 0.75;
const CONTRADICTED_AT: f64 = 0.5;
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

    pub(super) fn questions(&self) -> Questions {
        let goal = self.goal;
        let mut questions = Questions::from([(
            "operation".into(),
            Question::Pick {
                text: format!("Which operation brings the screen closest to this goal: {goal}"),
                options: self
                    .operations
                    .iter()
                    .map(|operation| (operation.name.into(), operation.description.into()))
                    .collect(),
            },
        )]);
        for operation in self
            .operations
            .iter()
            .filter(|operation| !operation.targets.is_empty())
        {
            let text = format!(
                "Which of these is the one to {} to get closest to this goal: {goal}",
                operation.name
            );
            let descriptions: Vec<String> = operation
                .targets
                .iter()
                .map(|element| element.describe())
                .collect();
            questions.extend(both_orders(operation.name, &text, &descriptions));
            for (index, element) in operation.targets.iter().enumerate() {
                let values = self.values(operation.name, element);
                if values.is_empty() {
                    continue;
                }
                let mut text = format!(
                    "Which of these to {} in {} to get closest to this goal: {goal}",
                    operation.name,
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
                let id = format!("{}_{}", operation.name, forward(index));
                let shown = &values[..values.len().min(MAX_ELEMENTS)];
                questions.extend(both_orders(&id, &text, shown));
            }
        }
        questions
    }

    pub(super) fn decide(&self, answers: &Answers) -> Decision {
        let names: Vec<&str> = self
            .operations
            .iter()
            .map(|operation| operation.name)
            .collect();
        let chosen = match ranked(answers.get("operation")) {
            Some((pick, top, margin)) if top >= OPERATION_AT && margin >= MARGIN => pick,
            ranking => {
                return Decision::Unsure(format!("which operation to use ({})", shown(ranking)));
            }
        };
        let Some(operation) = self
            .operations
            .iter()
            .find(|operation| operation.name == chosen)
        else {
            return Decision::Unsure(format!(
                "which operation to use: {chosen:?} is not one of {names:?}"
            ));
        };
        match operation.name {
            "done" => return Decision::Done,
            "stuck" => return Decision::Stuck,
            "enter" => return Decision::Act(Box::new(Action::Enter)),
            "scroll" => return Decision::Act(Box::new(Action::Scroll)),
            "wait" => return Decision::Act(Box::new(Action::Wait)),
            _ => {}
        }
        let index = match agreed(answers, operation.name, operation.targets.len(), TARGET_AT) {
            Ok(index) => index,
            Err(reason) => {
                return Decision::Unsure(format!("what to {}: {reason}", operation.name));
            }
        };
        let element = operation.targets[index];
        if operation.name == "press" {
            return Decision::Act(Box::new(Action::Press(element.clone())));
        }
        let id = format!("{}_{}", operation.name, forward(index));
        let count = self.values(operation.name, element).len().min(MAX_ELEMENTS);
        let value = match agreed(answers, &id, count, TARGET_AT) {
            Ok(value) => value,
            Err(reason) => {
                return Decision::Unsure(format!(
                    "what to {} in {}: {reason}",
                    operation.name,
                    element.describe()
                ));
            }
        };
        Decision::Act(Box::new(match &element.kind {
            Kind::Choose(options) => Action::Choose(element.clone(), options[value].clone()),
            _ => Action::Type(element.clone(), self.inputs[value].clone()),
        }))
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

pub(super) fn recognize(
    recorded: &Element,
    screen: &Screen,
) -> (Questions, impl Fn(&Answers) -> Option<Element> + use<>) {
    let candidates: Vec<&Element> = screen
        .elements
        .iter()
        .filter(|element| {
            element.kind == recorded.kind
                || (recorded.kind != Kind::Type && element.kind != Kind::Type)
        })
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
    let questions = if descriptions.is_empty() {
        Questions::new()
    } else {
        both_orders("recorded", &text, &descriptions).collect()
    };
    let matched: Vec<Element> = candidates.into_iter().cloned().collect();
    let found = move |answers: &Answers| {
        agreed(answers, "recorded", matched.len(), RECOGNIZED_AT)
            .ok()
            .map(|index| matched[index].clone())
    };
    (questions, found)
}

pub(super) fn claim(statement: &str) -> Questions {
    Questions::from([
        (
            "holds".into(),
            Question::yesno(&format!("The screen shows that this is true: {statement}")),
        ),
        (
            "contradicted".into(),
            Question::yesno(&format!("The screen shows that this is false: {statement}")),
        ),
    ])
}

pub(super) fn verdict(answers: &Answers) -> (Verdict, f64, f64) {
    let probability = |id: &str| answers.get(id).and_then(Answer::yes).unwrap_or(0.0);
    let (holds, contradicted) = (probability("holds"), probability("contradicted"));
    let verdict = if contradicted >= CONTRADICTED_AT || holds < 1.0 - HOLDS_AT {
        Verdict::Contradicted
    } else if holds >= HOLDS_AT {
        Verdict::Holds
    } else {
        Verdict::Inconclusive
    };
    (verdict, holds, contradicted)
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

fn agreed(answers: &Answers, id: &str, count: usize, at: f64) -> Result<usize, String> {
    let index = |label: &str| {
        label
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .filter(|&n| n < count)
    };
    let first = ranked(answers.get(id));
    let second = ranked(answers.get(&format!("{id}{REVERSED}")));
    match (&first, &second) {
        (Some((a, top_a, margin_a)), Some((b, top_b, margin_b)))
            if top_a.min(*top_b) >= at && margin_a.min(*margin_b) >= MARGIN =>
        {
            match (index(a), index(b).map(|n| count - 1 - n)) {
                (Some(a), Some(b)) if a == b => Ok(a),
                _ if a == NONE && b == NONE => Err("none of the options fits".into()),
                _ => Err(format!("the two orderings disagree ({a} and {b})")),
            }
        }
        _ => Err(format!("unsure ({}; {})", shown(first), shown(second))),
    }
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

    #[test]
    fn the_same_target_in_both_orders_is_acted_on() {
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
            step.decide(&answers),
            Decision::Act(Box::new(Action::Press(screen.elements[1].clone())))
        );
    }

    #[test]
    fn orderings_that_disagree_or_a_close_call_leave_the_step_unsure() {
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
            matches!(step.decide(&disagree), Decision::Unsure(reason) if reason.contains("disagree"))
        );
        assert!(matches!(step.decide(&close), Decision::Unsure(_)));
    }

    #[test]
    fn values_are_offered_per_field_and_secrets_stay_hidden() {
        let screen = Screen {
            elements: vec![Element::new("textbox", "Password", Kind::Type)],
            ..Screen::default()
        };
        let inputs = [Input {
            key: "password".into(),
            text: "hunter2".into(),
            secret: true,
        }];
        let step = Step::new("sign in", &screen, &inputs);
        let questions =
            serde_json::to_string(&step.questions().keys().collect::<Vec<_>>()).unwrap();
        let state = format!("{:?}", step.questions());
        assert!(
            questions.contains("\"type_reversed\"") && questions.contains("\"type_001_reversed\"")
        );
        assert!(state.contains("(secret \\\"password\\\")") && !state.contains("hunter2"));
    }

    #[test]
    fn every_option_of_every_drop_down_can_be_chosen() {
        let countries = (0..MAX_ELEMENTS).map(|n| format!("Country {n}")).collect();
        let sizes = vec!["S".to_string(), "M".into(), "XXL".into()];
        let screen = Screen {
            elements: vec![
                Element::new("combobox", "Country", Kind::Choose(countries)),
                Element::new("combobox", "Size", Kind::Choose(sizes)),
            ],
            ..Screen::default()
        };
        let step = Step::new("pick the biggest size", &screen, &[]);
        let answers = Answers::from([
            ("operation".into(), pick("choose", &[("choose", 0.9)])),
            ("choose".into(), pick("002", &[("002", 0.9)])),
            ("choose_reversed".into(), pick("001", &[("001", 0.9)])),
            ("choose_002".into(), pick("003", &[("003", 0.9)])),
            ("choose_002_reversed".into(), pick("001", &[("001", 0.9)])),
        ]);
        let questions = step.questions();
        let offered = |id: &str| match &questions[id] {
            Question::Pick { options, .. } => options.len(),
            _ => 0,
        };
        assert_eq!(
            (offered("choose_001"), offered("choose_002")),
            (MAX_ELEMENTS + 1, 4)
        );
        assert_eq!(
            step.decide(&answers),
            Decision::Act(Box::new(Action::Choose(
                screen.elements[1].clone(),
                "XXL".into()
            )))
        );
    }

    #[test]
    fn claims_fall_into_three_bands() {
        let answers = |holds: f64, contradicted: f64| {
            Answers::from([
                ("holds".into(), Answer::Yesno { yes: holds }),
                ("contradicted".into(), Answer::Yesno { yes: contradicted }),
            ])
        };
        assert_eq!(verdict(&answers(0.9, 0.05)).0, Verdict::Holds);
        assert_eq!(verdict(&answers(0.6, 0.1)).0, Verdict::Inconclusive);
        assert_eq!(verdict(&answers(0.9, 0.6)).0, Verdict::Contradicted);
        assert_eq!(verdict(&answers(0.1, 0.1)).0, Verdict::Contradicted);
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

    #[test]
    fn a_control_with_no_candidates_is_not_asked_about() {
        let field = Element::new("textbox", "Email", Kind::Type);
        let (questions, recognized) = recognize(&field, &screen());
        assert!(questions.is_empty());
        assert_eq!(recognized(&Answers::new()), None);
        let twin = Element {
            among: 3,
            ..Element::new("button", "Delete", Kind::Press)
        };
        assert!(recognize(&twin, &screen()).0.is_empty());
    }
}
