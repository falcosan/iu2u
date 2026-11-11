use std::cmp::Reverse;
use std::collections::HashMap;
use std::fmt::Write;

use crate::clip;

pub(super) const MAX_ELEMENTS: usize = 254;
const VALUE_CHARS: usize = 200;
const SECTIONS: [&str; 14] = [
    "alertdialog",
    "banner",
    "complementary",
    "dialog",
    "form",
    "group",
    "main",
    "menu",
    "navigation",
    "region",
    "search",
    "tabpanel",
    "toolbar",
    "window",
];

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Kind {
    Press,
    Type,
    Choose(Vec<String>),
}

#[derive(Debug, Default)]
pub(super) struct Node {
    pub(super) role: String,
    pub(super) name: String,
    pub(super) value: String,
    pub(super) states: Vec<&'static str>,
    pub(super) kind: Option<Kind>,
    pub(super) handle: usize,
    pub(super) children: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Element {
    pub(super) role: String,
    pub(super) name: String,
    pub(super) raw_name: String,
    pub(super) raw_options: Vec<String>,
    pub(super) value: String,
    pub(super) states: Vec<&'static str>,
    pub(super) section: String,
    pub(super) kind: Kind,
    pub(super) handle: usize,
    pub(super) position: usize,
    pub(super) among: usize,
}

#[derive(Debug, Default)]
pub(super) struct Screen {
    pub(super) text: String,
    pub(super) visible: String,
    pub(super) elements: Vec<Element>,
    pub(super) omitted: usize,
}

impl Element {
    pub(super) fn new(role: &str, name: &str, kind: Kind) -> Self {
        Self {
            role: role.into(),
            name: name.into(),
            raw_name: name.into(),
            raw_options: Vec::new(),
            value: String::new(),
            states: Vec::new(),
            section: String::new(),
            kind,
            handle: 0,
            position: 1,
            among: 1,
        }
    }

    pub(super) fn describe(&self) -> String {
        let mut text = summary(&self.role, &self.name, &self.value, &self.states);
        if !self.section.is_empty() {
            write!(text, " · in {:?}", self.section).ok();
        }
        if self.among > 1 {
            write!(text, " · {} of {}", self.position, self.among).ok();
        }
        text
    }

    pub(super) fn same_control(&self, other: &Self) -> bool {
        self.role == other.role
            && self.name == other.name
            && self.section == other.section
            && self.position == other.position
            && self.among == other.among
    }
}

impl Screen {
    pub(super) fn new(nodes: &[Node], secrets: &[(&str, String)]) -> Self {
        let mut forms: Vec<(String, &str)> = secrets
            .iter()
            .flat_map(|(secret, label)| {
                [(secret.to_string(), label.as_str()), (words(secret), label)]
            })
            .filter(|(form, _)| !form.is_empty())
            .collect();
        forms.sort_by_key(|(form, _)| Reverse(form.len()));
        let hide = |text: &str| {
            forms.iter().fold(text.to_owned(), |text, (form, label)| {
                text.replace(form, label)
            })
        };
        let mut screen = Self::default();
        let mut heading = String::new();
        let mut visited = vec![false; nodes.len()];
        let mut stack = vec![(0, 0, String::new(), (String::new(), String::new()))];
        while let Some((index, depth, section, parent)) = stack.pop() {
            let Some(node) = nodes.get(index).filter(|_| !visited[index]) else {
                continue;
            };
            visited[index] = true;
            let name = hide(&node.name);
            let value = clip(&hide(&node.value), VALUE_CHARS);
            let repeats_parent = node.role == "text" && (name == parent.0 || name == parent.1);
            let shown =
                !node.role.is_empty() && !(name.is_empty() && value.is_empty()) && !repeats_parent;
            if shown {
                screen.line(node, &name, &value, depth);
            }
            if node.role == "heading" {
                heading.clone_from(&name);
            }
            if let Some(kind) = node
                .kind
                .clone()
                .filter(|_| !node.states.contains(&"disabled"))
            {
                let (kind, raw_options) = match kind {
                    Kind::Choose(options) => (
                        Kind::Choose(options.iter().map(|option| hide(option)).collect()),
                        options,
                    ),
                    kind => (kind, Vec::new()),
                };
                screen.push(Element {
                    role: node.role.clone(),
                    name: name.clone(),
                    raw_name: node.name.clone(),
                    raw_options,
                    value: value.clone(),
                    states: node.states.clone(),
                    section: if section.is_empty() {
                        heading.clone()
                    } else {
                        section.clone()
                    },
                    kind,
                    handle: node.handle,
                    position: 1,
                    among: 1,
                });
            }
            let inner = if SECTIONS.contains(&node.role.as_str()) && !name.is_empty() {
                name.clone()
            } else {
                section
            };
            let depth = depth + usize::from(shown);
            let around = if node.role.is_empty() {
                parent
            } else {
                (name.clone(), value.clone())
            };
            stack.extend(
                node.children
                    .iter()
                    .rev()
                    .map(|&child| (child, depth, inner.clone(), around.clone())),
            );
        }
        screen.number_twins();
        screen
    }

    fn number_twins(&mut self) {
        let keys: Vec<(String, String, String)> = self
            .elements
            .iter()
            .map(|element| {
                (
                    element.role.clone(),
                    element.name.clone(),
                    element.section.clone(),
                )
            })
            .collect();
        let mut among = HashMap::new();
        for key in &keys {
            *among.entry(key).or_insert(0) += 1;
        }
        let mut seen = HashMap::new();
        for (element, key) in self.elements.iter_mut().zip(&keys) {
            let position = seen.entry(key).or_insert(0);
            *position += 1;
            element.position = *position;
            element.among = among[key];
        }
    }

    fn line(&mut self, node: &Node, name: &str, value: &str, depth: usize) {
        writeln!(
            self.text,
            "{:indent$}- {}",
            "",
            summary(&node.role, name, value, &node.states),
            indent = depth * 2
        )
        .ok();
        write!(self.visible, "{} ", words(&format!("{name} {value}"))).ok();
    }

    fn push(&mut self, element: Element) {
        if self.elements.len() < MAX_ELEMENTS {
            self.elements.push(element);
        } else {
            self.omitted += 1;
        }
    }

    pub(super) fn shows(&self, text: &str) -> bool {
        let (needle, shown) = (tight(text), tight(&self.visible));
        let word = |character: Option<&char>| character.is_some_and(|c| c.is_alphanumeric());
        needle.is_empty()
            || shown
                .windows(needle.len())
                .enumerate()
                .any(|(start, window)| {
                    window == needle.as_slice()
                        && !(word(needle.first())
                            && word(start.checked_sub(1).and_then(|i| shown.get(i))))
                        && !(word(needle.last()) && word(shown.get(start + needle.len())))
                })
    }

    pub(super) fn reveals(&self, secret: &str) -> bool {
        let squeeze = |text: &str| {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        let secret = squeeze(secret);
        let options = self
            .elements
            .iter()
            .filter_map(|element| match &element.kind {
                Kind::Choose(options) => Some(options.join(" ")),
                _ => None,
            });
        !secret.is_empty()
            && std::iter::once(self.visible.clone())
                .chain(options)
                .any(|text| squeeze(&text).contains(&secret))
    }

    pub(super) fn fingerprint(&self) -> String {
        let hash = self
            .text
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        format!("{hash:016x}")
    }
}

fn summary(role: &str, name: &str, value: &str, states: &[&str]) -> String {
    let mut text = format!("{role} {name:?}");
    if !value.is_empty() {
        write!(text, " = {value:?}").ok();
    }
    for state in states {
        write!(text, " · {state}").ok();
    }
    text
}

fn tight(text: &str) -> Vec<char> {
    let mut tight: Vec<char> = Vec::new();
    let mut spaced = false;
    for character in text.chars() {
        if character.is_whitespace() {
            spaced = true;
            continue;
        }
        if spaced
            && character.is_alphanumeric()
            && tight.last().is_some_and(|c| c.is_alphanumeric())
        {
            tight.push(' ');
        }
        spaced = false;
        tight.push(character);
    }
    tight
}

pub(super) fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, name: &str, kind: Option<Kind>, children: &[usize]) -> Node {
        Node {
            role: role.into(),
            name: name.into(),
            kind,
            children: children.to_vec(),
            ..Node::default()
        }
    }

    #[test]
    fn elements_take_the_nearest_named_section_or_the_last_heading() {
        let nodes = [
            node("", "", None, &[1, 2, 3]),
            node("heading", "Cart", None, &[]),
            node("button", "Pay", Some(Kind::Press), &[]),
            node("dialog", "Coupon", None, &[4]),
            node("textbox", "Code", Some(Kind::Type), &[]),
        ];
        let screen = Screen::new(&nodes, &[]);
        let sections: Vec<_> = screen.elements.iter().map(|e| e.section.as_str()).collect();
        assert_eq!(sections, ["Cart", "Coupon"]);
        assert_eq!(
            screen.text,
            "- heading \"Cart\"\n- button \"Pay\"\n- dialog \"Coupon\"\n  - textbox \"Code\"\n"
        );
    }

    #[test]
    fn text_that_repeats_its_control_is_shown_once() {
        let nodes = [
            node("", "", None, &[1]),
            node("button", "Pay", Some(Kind::Press), &[2]),
            node("text", "Pay", None, &[]),
        ];
        assert_eq!(Screen::new(&nodes, &[]).text, "- button \"Pay\"\n");
    }

    #[test]
    fn identical_controls_are_told_apart_by_their_position() {
        let nodes = [
            node("", "", None, &[1, 2, 3]),
            node("heading", "Todos", None, &[]),
            node("button", "Delete", Some(Kind::Press), &[]),
            node("button", "Delete", Some(Kind::Press), &[]),
        ];
        let screen = Screen::new(&nodes, &[]);
        let described: Vec<String> = screen.elements.iter().map(Element::describe).collect();
        assert_eq!(
            described,
            [
                "button \"Delete\" · in \"Todos\" · 1 of 2",
                "button \"Delete\" · in \"Todos\" · 2 of 2"
            ]
        );
        assert!(!screen.elements[0].same_control(&screen.elements[1]));
    }

    #[test]
    fn text_that_repeats_a_field_value_is_shown_once() {
        let field = Node {
            value: "old@x.io".into(),
            ..node("textbox", "Email", Some(Kind::Type), &[2])
        };
        let nodes = [
            node("", "", None, &[1]),
            field,
            node("", "", None, &[3]),
            node("text", "old@x.io", None, &[]),
        ];
        assert_eq!(
            Screen::new(&nodes, &[]).text,
            "- textbox \"Email\" = \"old@x.io\"\n"
        );
    }

    #[test]
    fn disabled_controls_are_shown_but_not_offered() {
        let mut button = node("button", "Pay", Some(Kind::Press), &[]);
        button.states = vec!["disabled"];
        let screen = Screen::new(&[node("", "", None, &[1]), button], &[]);
        assert!(screen.elements.is_empty());
        assert!(screen.text.contains("disabled"));
    }

    #[test]
    fn elements_beyond_the_choice_limit_are_counted_and_cycles_are_walked_once() {
        let mut nodes = vec![node("", "", None, &[0])];
        nodes[0].children.extend(1..=MAX_ELEMENTS + 3);
        nodes.extend(
            (0..MAX_ELEMENTS + 3).map(|i| node("link", &i.to_string(), Some(Kind::Press), &[0])),
        );
        let screen = Screen::new(&nodes, &[]);
        assert_eq!((screen.elements.len(), screen.omitted), (MAX_ELEMENTS, 3));
    }

    #[test]
    fn visible_text_reads_across_nodes_without_roles_or_states() {
        let mut field = node("textbox", "Email", Some(Kind::Type), &[]);
        field.value = "qa".into();
        field.states = vec!["required"];
        let nodes = [
            node("", "", None, &[1, 2, 3]),
            node("text", "Total:", None, &[]),
            node("text", "$5", None, &[]),
            field,
        ];
        let screen = Screen::new(&nodes, &[]);
        assert!(screen.visible.contains(&words("Total:  $5")));
        assert!(screen.visible.contains("Email qa"));
        assert!(!screen.visible.contains("textbox") && !screen.visible.contains("required"));
    }

    #[test]
    fn shown_text_must_start_and_end_on_word_edges() {
        let nodes = [
            node("", "", None, &[1, 2, 3, 4]),
            node("text", "Total $50", None, &[]),
            node("text", "Unpaid", None, &[]),
            node("text", "Total:", None, &[]),
            node("text", "$5", None, &[]),
        ];
        let screen = Screen::new(&nodes, &[]);
        assert!(!screen.shows("Total $5") && !screen.shows("paid"));
        let split = |parts: &[&str]| {
            let mut nodes = vec![node("", "", None, &[])];
            nodes[0].children.extend(1..=parts.len());
            nodes.extend(parts.iter().map(|part| node("text", part, None, &[])));
            Screen::new(&nodes, &[])
        };
        assert!(split(&["Welcome,", "Ada", "!"]).shows("Welcome, Ada!"));
        assert!(split(&["$", "19", ".", "99"]).shows("$19.99"));
        assert!(!split(&["$", "19", ".", "99"]).shows("$19.9"));
        assert!(!split(&["1", "2", "3"]).shows("12") && split(&["1", "2", "3"]).shows("1 2"));
        assert!(split(&["sk-live", "7f3a"]).reveals("sk-live  7f3a"));
        assert!(split(&["x", "abc", "123", "y"]).reveals("abc123"));
        assert!(screen.shows("Total:  $5") && screen.shows("$50") && screen.shows("Unpaid"));
        let overlapping = Screen::new(
            &[node("", "", None, &[1]), node("text", "ba a a", None, &[])],
            &[],
        );
        assert!(overlapping.shows("a a"));
    }

    #[test]
    fn a_hidden_secret_leaves_no_trace_even_collapsed_or_clipped() {
        let label = "(secret \"token\")".to_string();
        let secret = format!("s\"k  {}", "x".repeat(VALUE_CHARS));
        let mut field = node("textbox", "Token", Some(Kind::Type), &[]);
        field.value = secret.clone();
        let mut choice = node("combobox", "Account", None, &[]);
        choice.kind = Some(Kind::Choose(vec![format!("is {secret}")]));
        let nodes = [
            node("dialog", &words(&secret), None, &[1, 2]),
            field,
            choice,
        ];
        let screen = Screen::new(&nodes, &[(&secret, label.clone())]);
        let described: Vec<String> = screen.elements.iter().map(Element::describe).collect();
        let shown = format!("{}{}{described:?}", screen.text, screen.visible);
        assert!(
            !shown.contains("s\\\"k") && !shown.contains("s\"k"),
            "{shown}"
        );
        assert_eq!(screen.elements[0].value, label);
        assert_eq!(screen.elements[0].section, label);
        assert_eq!(screen.elements[1].raw_options, [format!("is {secret}")]);
    }

    #[test]
    fn a_secret_inside_a_longer_one_does_not_uncover_it() {
        let field = Node {
            value: "admin2024secret".into(),
            ..node("textbox", "Password", Some(Kind::Type), &[])
        };
        let secrets = [
            ("admin", "(secret \"user\")".to_string()),
            ("admin2024secret", "(secret \"password\")".to_string()),
        ];
        let screen = Screen::new(&[node("", "", None, &[1]), field], &secrets);
        assert_eq!(screen.elements[0].value, "(secret \"password\")");
    }

    #[test]
    fn a_secret_inside_a_drop_down_option_is_caught() {
        let cards = node(
            "combobox",
            "Card",
            Some(Kind::Choose(vec!["Visa 4242424242424242".into()])),
            &[],
        );
        let screen = Screen::new(&[node("", "", None, &[1]), cards], &[]);
        assert!(screen.reveals("4242 4242 4242 4242"));
    }

    #[test]
    fn fingerprint_is_stable_and_follows_the_text() {
        let screen = |text: &str| Screen {
            text: text.into(),
            ..Screen::default()
        };
        assert_eq!(screen("a").fingerprint(), "af63dc4c8601ec8c");
        assert_ne!(screen("a").fingerprint(), screen("b").fingerprint());
    }
}
