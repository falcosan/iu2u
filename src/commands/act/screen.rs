use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::clip;

pub(super) const MAX_ELEMENTS: usize = 254;
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');
pub(super) const HEADER: &str = "response header";
pub(super) const VALUE_CHARS: usize = 200;
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
    pub(super) url: String,
    pub(super) description: String,
    pub(super) states: Vec<&'static str>,
    pub(super) kind: Option<Kind>,
    pub(super) handle: usize,
    pub(super) frame: usize,
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
    pub(super) frame: usize,
    pub(super) position: usize,
    pub(super) among: usize,
}

#[derive(Debug, Default)]
pub(super) struct Screen {
    pub(super) text: String,
    pub(super) visible: String,
    pub(super) pages: String,
    pub(super) addresses: String,
    pub(super) elements: Vec<Element>,
    pub(super) omitted: usize,
    pub(super) candidates: Vec<Candidate>,
    pub(super) kept: Vec<String>,
    pub(super) named: Vec<(String, String)>,
    pub(super) shows_secret: bool,
    pub(super) hovers: bool,
    pub(super) spots: bool,
    pub(super) tail: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Candidate {
    pub(super) text: String,
    pub(super) shown: String,
    pub(super) origin: Origin,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub(super) enum Origin {
    Screen {
        role: String,
        name: String,
        field: String,
    },
    Json {
        path: String,
    },
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
            frame: 0,
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
        let hider = Hider::new(secrets);
        let hide = |text: &str| hider.hide(text);
        let mut screen = Self::default();
        let mut offered = HashSet::new();
        let mut heading = String::new();
        let mut visited = vec![false; nodes.len()];
        let mut stack = vec![(0, 0, String::new(), (String::new(), String::new()))];
        while let Some((index, depth, section, parent)) = stack.pop() {
            let Some(node) = nodes.get(index).filter(|_| !visited[index]) else {
                continue;
            };
            visited[index] = true;
            let name = hide(&node.name);
            let whole_value = hide(&node.value);
            let value = clip(&whole_value, VALUE_CHARS);
            let repeats_parent = node.role == "text" && (name == parent.0 || name == parent.1);
            let shown = !node.role.is_empty()
                && !(name.is_empty()
                    && value.is_empty()
                    && node.url.is_empty()
                    && node.kind.is_none()
                    && !node.states.contains(&"busy"))
                && !repeats_parent;
            if shown {
                let description = hide(&node.description);
                let decoded = percent_decode_str(&node.url).decode_utf8_lossy();
                let url = match hide(&decoded) {
                    hidden if hidden != decoded => hidden,
                    _ => hide(&node.url),
                };
                screen.shows_secret |= name != node.name
                    || whole_value != node.value
                    || description != node.description;
                screen.line(node, [&name, &value, &description, &url], depth);
                for (field, text, raw) in [
                    ("name", &name, &node.name),
                    ("value", &whole_value, &node.value),
                    ("url", &url, &node.url),
                    ("description", &description, &node.description),
                ] {
                    if !text.is_empty() && text == raw && offered.insert(text.clone()) {
                        screen.candidates.push(Candidate {
                            text: text.clone(),
                            shown: if field == "name" {
                                format!("{} {:?}", node.role, clip(&name, VALUE_CHARS))
                            } else {
                                let (named, quoted) =
                                    (clip(&name, VALUE_CHARS), clip(text, VALUE_CHARS));
                                format!("the {field} of {} {named:?}: {quoted:?}", node.role)
                            },
                            origin: Origin::Screen {
                                role: node.role.clone(),
                                name: name.clone(),
                                field: field.into(),
                            },
                        });
                    }
                }
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
                    frame: node.frame,
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

    fn line(&mut self, node: &Node, [name, value, description, url]: [&str; 4], depth: usize) {
        let mut line = summary(&node.role, name, value, &node.states);
        if !description.is_empty() {
            write!(line, " · description {description:?}").ok();
        }
        if !url.is_empty() {
            write!(line, " · url {:?}", clip(url, VALUE_CHARS)).ok();
        }
        writeln!(self.text, "{:indent$}- {line}", "", indent = depth * 2).ok();
        write!(
            self.visible,
            "{} ",
            words(&format!("{name} {value} {description}"))
        )
        .ok();
        if !url.is_empty() {
            write!(self.addresses, "{url} ").ok();
        }
        if node.role == "page" {
            write!(self.pages, "{url} ").ok();
        }
    }

    fn push(&mut self, element: Element) {
        if self.elements.len() < MAX_ELEMENTS {
            self.elements.push(element);
        } else {
            self.omitted += 1;
        }
    }

    pub(super) fn shows(&self, text: &str) -> bool {
        shows(&self.visible, text)
            || (text.contains(['/', '?', '&', '=', '#']) && shows(&self.pages, text))
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
            && [self.visible.clone(), self.addresses.clone()]
                .into_iter()
                .chain(options)
                .any(|text| squeeze(&text).contains(&secret))
    }

    pub(super) fn fingerprint(&self) -> String {
        hash(&self.identity())
    }

    pub(super) fn signature(&self) -> String {
        hash(&masked(&self.addressed(), &self.kept))
    }

    pub(super) fn exact(&self) -> String {
        hash(&self.standing(self.text.clone()))
    }

    fn identity(&self) -> String {
        let mut identity = String::with_capacity(self.text.len());
        for line in self.text.split_inclusive('\n') {
            let header = line
                .trim_start()
                .strip_prefix("- ")
                .is_some_and(|line| line.starts_with(HEADER));
            match line.split_once(':') {
                Some((name, _)) if header => {
                    identity.push_str(name);
                    identity.push('\n');
                }
                _ => identity.push_str(line),
            }
        }
        self.standing(identity)
    }

    fn standing(&self, text: String) -> String {
        self.named
            .iter()
            .fold(text, |text, (value, name)| standing_for(&text, value, name))
    }

    fn addressed(&self) -> String {
        let identity = self.identity();
        let mut addressed = String::with_capacity(identity.len());
        let mut rest = identity.as_str();
        while let Some(at) = rest.find(" · url \"") {
            let start = at + " · url \"".len();
            addressed.push_str(&rest[..start]);
            let mut escaped = false;
            let end = rest[start..]
                .find(|c: char| {
                    let closes = c == '"' && !escaped;
                    escaped = c == '\\' && !escaped;
                    closes
                })
                .map_or(rest.len(), |end| start + end);
            let address = &rest[start..end];
            addressed.push_str(&address[..address.find(['?', '#']).unwrap_or(address.len())]);
            rest = &rest[end..];
        }
        addressed.push_str(rest);
        addressed
    }
}

pub(super) struct Hider<'a> {
    forms: Vec<(String, &'a str)>,
}

impl<'a> Hider<'a> {
    pub(super) fn new(secrets: &'a [(&str, String)]) -> Self {
        let mut forms: Vec<(String, &str)> = secrets
            .iter()
            .flat_map(|(secret, label)| {
                let quoted = serde_json::to_string(secret).unwrap_or_default();
                let escaped = quoted
                    .get(1..quoted.len().saturating_sub(1))
                    .unwrap_or_default()
                    .to_owned();
                [
                    (secret.to_string(), label.as_str()),
                    (words(secret), label),
                    (escaped, label),
                    (
                        form_urlencoded::byte_serialize(secret.as_bytes()).collect(),
                        label,
                    ),
                    (
                        utf8_percent_encode(secret, URI_COMPONENT).to_string(),
                        label,
                    ),
                ]
            })
            .filter(|(form, _)| !form.is_empty())
            .collect();
        forms.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        forms.dedup_by(|(a, _), (b, _)| a == b);
        Self { forms }
    }

    pub(super) fn hide(&self, text: &str) -> String {
        self.forms
            .iter()
            .fold(text.to_owned(), |text, (form, label)| {
                text.replace(form, label)
            })
    }
}

fn standing_for(text: &str, value: &str, name: &str) -> String {
    if value.is_empty() {
        return text.to_owned();
    }
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let (first, last) = (value.chars().next(), value.chars().next_back());
    let mut stood = String::with_capacity(text.len());
    let mut done = 0;
    for (at, _) in text.match_indices(value) {
        let end = at + value.len();
        if !(word(first) && word(text[..at].chars().next_back()))
            && !(word(last) && word(text[end..].chars().next()))
        {
            stood.push_str(&text[done..at]);
            stood.push_str(name);
            done = end;
        }
    }
    stood.push_str(&text[done..]);
    stood
}

pub(super) fn masked(text: &str, kept: &[String]) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(|c: char| c.is_ascii_digit()) {
        masked.push_str(&rest[..at]);
        let length = rest[at..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len() - at);
        let number = &rest[at..at + length];
        masked.push_str(if kept.iter().any(|kept| kept == number) {
            number
        } else {
            "#"
        });
        rest = &rest[at + length..];
    }
    masked.push_str(rest);
    masked
}

pub(super) fn shows(visible: &str, text: &str) -> bool {
    let (needle, shown) = (tight(text), tight(visible));
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

pub(super) fn numbers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|number| !number.is_empty())
        .map(str::to_owned)
        .collect()
}

fn hash(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
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
    fn a_secret_in_an_address_the_browser_encodes_only_in_part_is_hidden() {
        let label = "(secret \"pw\")".to_string();
        let mut link = node("link", "Share", Some(Kind::Press), &[]);
        link.url = "https://hub.test/share?pw=p@ss%20word".into();
        let screen = Screen::new(
            &[node("page", "", None, &[1]), link],
            &[("p@ss word", label.clone())],
        );
        assert!(
            !screen.text.contains("p@ss") && screen.text.contains("pw=(secret"),
            "{}",
            screen.text
        );
        assert!(
            screen
                .candidates
                .iter()
                .all(|candidate| !candidate.text.contains("p@ss"))
        );
    }

    #[test]
    fn a_long_text_is_offered_whole_but_shown_short() {
        let long = "x".repeat(5 * VALUE_CHARS);
        let screen = Screen::new(
            &[
                node("page", "", None, &[1]),
                node("paragraph", &long, None, &[]),
            ],
            &[],
        );
        let offered = screen
            .candidates
            .iter()
            .find(|candidate| candidate.text == long)
            .unwrap();
        assert!(
            offered.shown.len() < 2 * VALUE_CHARS,
            "{}",
            offered.shown.len()
        );
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
    fn a_secret_in_an_address_or_a_description_is_hidden() {
        let link = Node {
            url: "https://hub/reset?token=tok-31337".into(),
            description: "Sent with tok-31337".into(),
            ..node("link", "Reset", Some(Kind::Press), &[])
        };
        let secrets = [("tok-31337", "(secret \"token\")".to_string())];
        let screen = Screen::new(&[node("", "", None, &[1]), link], &secrets);
        assert!(!screen.text.contains("tok-31337"), "{}", screen.text);
        for url in [
            "https://hub/verify?email=qa%2Be2e%40example.com",
            "https://hub/verify?name=Q+A+%28e2e%29",
            "https://hub/verify?name=Q%20A%20(e2e)",
        ] {
            let link = Node {
                url: url.into(),
                ..node("link", "Resend", Some(Kind::Press), &[])
            };
            let secrets = [
                ("qa+e2e@example.com", "(secret \"email\")".to_string()),
                ("Q A (e2e)", "(secret \"name\")".to_string()),
            ];
            let screen = Screen::new(&[node("", "", None, &[1]), link], &secrets);
            assert!(!screen.text.contains("e2e"), "{}", screen.text);
        }
        assert!(!screen.reveals("tok-31337"), "{}", screen.visible);
        assert!(
            screen
                .candidates
                .iter()
                .all(|candidate| !candidate.text.contains("tok-31337")),
            "{:?}",
            screen.candidates
        );
    }

    #[test]
    fn a_secret_is_searched_once_for_each_distinct_form() {
        let secrets = [
            ("abc123XYZ", "(secret \"token\")".to_string()),
            ("a b", "(secret \"pair\")".to_string()),
        ];
        assert_eq!(Hider::new(&secrets).forms.len(), 4);
    }

    #[test]
    fn a_secret_escaped_in_json_is_hidden() {
        let line = node(
            "text",
            "password: \"Sup3r\\\"Secret-Value-42\\\"\"",
            None,
            &[],
        );
        let secrets = [(
            "Sup3r\"Secret-Value-42\"",
            "(secret \"password\")".to_string(),
        )];
        let screen = Screen::new(&[node("", "", None, &[1]), line], &secrets);
        assert!(!screen.text.contains("Secret-Value-42"), "{}", screen.text);
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
    fn a_control_without_a_name_still_shows_on_the_screen() {
        let screen = Screen::new(
            &[
                node("page", "Hub", None, &[1, 2]),
                node("button", "", Some(Kind::Press), &[]),
                node("generic", "", None, &[]),
            ],
            &[],
        );
        assert_eq!(screen.text, "- page \"Hub\"\n  - button \"\"\n");
        assert_eq!(screen.elements.len(), 1);
    }

    #[test]
    fn a_signature_ignores_the_numbers_the_step_does_not_mention() {
        let shown = |text: &str, kept: &[&str]| Screen {
            text: text.into(),
            kept: kept.iter().map(|kept| (*kept).to_owned()).collect(),
            ..Screen::default()
        };
        assert_eq!(
            shown("- text \"Updated 22 days ago at 10:41\"", &[]).signature(),
            shown("- text \"Updated 23 days ago at 09:05\"", &[]).signature()
        );
        assert_ne!(
            shown("- text \"3 prototypes\"", &["3"]).signature(),
            shown("- text \"4 prototypes\"", &["3"]).signature()
        );
        assert_ne!(
            shown("- text \"Updated\"", &[]).signature(),
            shown("- text \"Uploaded\"", &[]).signature()
        );
        assert_eq!(
            shown(
                "- page \"Hub\" · url \"http://hub/#code=ab3f&state=eyJ\"\n- text \"Hi\"",
                &[]
            )
            .signature(),
            shown(
                "- page \"Hub\" · url \"http://hub/?q=x\"\n- text \"Hi\"",
                &[]
            )
            .signature()
        );
        assert_ne!(
            shown("- page \"Hub\" · url \"http://hub/a\"", &[]).signature(),
            shown("- page \"Hub\" · url \"http://hub/b\"", &[]).signature()
        );
        assert_eq!(
            shown(
                r#"- link "Share" · url "http://hub/share?pw=(secret \"pw\")&state=eyJ""#,
                &[]
            )
            .signature(),
            shown(
                r#"- link "Share" · url "http://hub/share?pw=(secret \"pw\")&state=xQz""#,
                &[]
            )
            .signature()
        );
        assert_eq!(
            numbers("expect the cart shows 3 items for 12.50"),
            ["3", "12", "50"]
        );
    }

    #[test]
    fn a_value_stands_for_its_name_only_where_it_stands_alone() {
        assert_eq!(
            standing_for("Prototype 4711 made, 47110 left, #4711", "4711", "$id"),
            "Prototype $id made, 47110 left, #$id"
        );
        assert_eq!(standing_for("go x/p/42/", "/p/42", "$p"), "go x$p/");
        assert_eq!(standing_for("same", "", "$p"), "same");
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
