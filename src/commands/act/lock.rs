use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::decide::Input;
use super::screen::{Element, Kind, Screen};
use super::surface::Action;

pub(super) type Lock = BTreeMap<String, Recorded>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Recorded {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) moves: Vec<Move>,
    pub(super) screen: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Control {
    role: String,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    section: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    among: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub(super) enum Move {
    Press {
        #[serde(flatten)]
        control: Control,
    },
    Type {
        #[serde(flatten)]
        control: Control,
        value: String,
    },
    Choose {
        #[serde(flatten)]
        control: Control,
        option: String,
    },
    Enter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
    },
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
    },
    Wait {
        #[serde(default)]
        ms: u64,
    },
}

pub(super) enum Replay {
    Exact(Action),
    Missing(Element),
}

impl Recorded {
    pub(super) fn missing(&self, inputs: &[Input]) -> Option<&str> {
        self.moves.iter().find_map(|recorded| match recorded {
            Move::Type { value, .. } if inputs.iter().all(|input| &input.key != value) => {
                Some(value.as_str())
            }
            _ => None,
        })
    }
}

impl Control {
    fn of(element: &Element, screen: &Screen) -> Self {
        let twins = element.among > 1;
        Self {
            role: element.role.clone(),
            name: element.name.clone(),
            section: element.section.clone(),
            position: twins.then_some(element.position),
            among: twins.then_some(element.among),
            screen: twins.then(|| screen.fingerprint()),
        }
    }

    fn as_element(&self, kind: Kind) -> Element {
        Element {
            section: self.section.clone(),
            position: self.position.unwrap_or(1),
            among: self.among.unwrap_or(1),
            ..Element::new(&self.role, &self.name, kind)
        }
    }
}

impl Move {
    pub(super) fn of(action: &Action, screen: &Screen) -> Self {
        match action {
            Action::Press(element) => Self::Press {
                control: Control::of(element, screen),
            },
            Action::Type(element, input) => Self::Type {
                control: Control::of(element, screen),
                value: input.key.clone(),
            },
            Action::Choose(element, option) => Self::Choose {
                control: Control::of(element, screen),
                option: option.clone(),
            },
            Action::Enter => Self::Enter {
                screen: Some(screen.fingerprint()),
            },
            Action::Scroll => Self::Scroll {
                screen: Some(screen.fingerprint()),
            },
            Action::Wait => Self::Wait { ms: 0 },
        }
    }

    pub(super) fn replay(&self, screen: &Screen, inputs: &[Input]) -> Replay {
        let (control, kind) = match self {
            Self::Enter { .. } => return Replay::Exact(Action::Enter),
            Self::Scroll { .. } => return Replay::Exact(Action::Scroll),
            Self::Wait { .. } => return Replay::Exact(Action::Wait),
            Self::Press { control } => (control, Kind::Press),
            Self::Type { control, .. } => (control, Kind::Type),
            Self::Choose { control, option } => (control, Kind::Choose(vec![option.clone()])),
        };
        let recorded = control.as_element(kind);
        if control
            .screen
            .as_ref()
            .is_some_and(|recorded_screen| *recorded_screen != screen.fingerprint())
        {
            return Replay::Missing(recorded);
        }
        let mut matches = screen
            .elements
            .iter()
            .filter(|element| element.same_control(&recorded));
        let found = match (matches.next(), matches.next()) {
            (Some(element), None) => self.with(element.clone(), inputs),
            _ => None,
        };
        found.map_or(Replay::Missing(recorded), Replay::Exact)
    }

    pub(super) fn with(&self, element: Element, inputs: &[Input]) -> Option<Action> {
        match self {
            Self::Press { .. } => Some(Action::Press(element)),
            Self::Type { value, .. } => {
                let input = inputs.iter().find(|input| &input.key == value)?;
                Some(Action::Type(element, input.clone()))
            }
            Self::Choose { option, .. } => match &element.kind {
                Kind::Choose(options) if options.contains(option) => {
                    Some(Action::Choose(element, option.clone()))
                }
                _ => None,
            },
            Self::Enter { .. } | Self::Scroll { .. } | Self::Wait { .. } => None,
        }
    }
}

pub(super) fn key(step: &str, occurrence: usize) -> String {
    format!("{step} ({occurrence})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(role: &str, name: &str, kind: Kind) -> Element {
        Element {
            section: "Login".into(),
            handle: 7,
            ..Element::new(role, name, kind)
        }
    }

    #[test]
    fn a_recorded_move_replays_on_the_one_matching_control() {
        let email = element("textbox", "Email", Kind::Type);
        let input = Input {
            key: "email".into(),
            text: "qa@example.com".into(),
            secret: false,
        };
        let screen = Screen {
            elements: vec![element("button", "Email", Kind::Press), email.clone()],
            ..Screen::default()
        };
        let recorded = Move::of(&Action::Type(email.clone(), input.clone()), &screen);
        assert!(matches!(
            recorded.replay(&screen, &[input]),
            Replay::Exact(Action::Type(found, _)) if found.handle == 7 && found.role == "textbox"
        ));
        assert_eq!(
            serde_json::to_value(&recorded).unwrap(),
            serde_json::json!({ "move": "type", "role": "textbox", "name": "Email", "section": "Login", "value": "email" })
        );
    }

    #[test]
    fn a_missing_or_ambiguous_control_is_not_guessed() {
        let pay = element("button", "Pay", Kind::Press);
        let recorded = Move::of(&Action::Press(pay.clone()), &Screen::default());
        let twice = Screen {
            elements: vec![pay.clone(), pay],
            ..Screen::default()
        };
        assert!(matches!(recorded.replay(&twice, &[]), Replay::Missing(_)));
        assert!(matches!(
            recorded.replay(&Screen::default(), &[]),
            Replay::Missing(_)
        ));
    }

    #[test]
    fn a_control_among_identical_ones_replays_by_its_position() {
        let rows = |among: usize, text: &str| Screen {
            text: text.into(),
            elements: (0..among)
                .map(|handle| Element {
                    handle,
                    position: handle + 1,
                    among,
                    ..element("button", "Delete", Kind::Press)
                })
                .collect(),
            ..Screen::default()
        };
        let todos = rows(3, "Milk Eggs Bread");
        let recorded = Move::of(&Action::Press(todos.elements[1].clone()), &todos);
        assert!(matches!(
            recorded.replay(&todos, &[]),
            Replay::Exact(Action::Press(found)) if found.handle == 1
        ));
        assert!(matches!(
            recorded.replay(&rows(3, "Eggs Milk Bread"), &[]),
            Replay::Missing(_)
        ));
        assert!(matches!(
            recorded.replay(&rows(2, "Milk Eggs Bread"), &[]),
            Replay::Missing(_)
        ));
        assert_eq!(
            serde_json::to_value(&recorded).unwrap()["position"],
            serde_json::json!(2)
        );
    }

    #[test]
    fn a_recorded_value_the_script_no_longer_has_is_named() {
        let email = Input {
            key: "email".into(),
            text: "qa@example.com".into(),
            secret: false,
        };
        let recorded = Recorded {
            moves: vec![Move::of(
                &Action::Type(element("textbox", "Email", Kind::Type), email.clone()),
                &Screen::default(),
            )],
            screen: String::new(),
        };
        assert_eq!(recorded.missing(&[email]), None);
        assert_eq!(recorded.missing(&[]), Some("email"));
    }

    #[test]
    fn repeated_steps_get_their_own_keys() {
        assert_eq!(key("do pay", 1), "do pay (1)");
        assert_eq!(key("do pay", 2), "do pay (2)");
        assert_ne!(key("do pay", 2), key("do pay (2)", 1));
    }
}
