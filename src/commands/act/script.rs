use std::collections::HashSet;

use anyhow::{Context, Result, bail};

use super::decide::Input;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Step {
    Open(String),
    App(String),
    Do(String),
    Expect(String),
    See(String),
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Script {
    pub(super) steps: Vec<(usize, Step)>,
    pub(super) inputs: Vec<Input>,
}

impl Step {
    pub(super) fn text(&self) -> String {
        match self {
            Self::Open(url) => format!("open {url}"),
            Self::App(name) => format!("app {name}"),
            Self::Do(goal) => format!("do {goal}"),
            Self::Expect(claim) => format!("expect {claim}"),
            Self::See(text) => format!("see {text:?}"),
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

pub(super) fn parse(text: &str) -> Result<Script> {
    let mut script = Script::default();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let rest = if verb == "see" {
            unquoted(rest).trim()
        } else {
            rest
        }
        .to_string();
        if rest.is_empty() {
            bail!("line {number}: {verb:?} needs some text after it");
        }
        let step = match verb {
            "open" => Step::Open(rest),
            "app" => Step::App(rest),
            "do" => Step::Do(rest),
            "expect" => Step::Expect(rest),
            "see" => Step::See(rest),
            "with" => {
                let input = input(&rest).with_context(|| format!("line {number}"))?;
                anyhow::ensure!(
                    script.inputs.iter().all(|given| given.key != input.key),
                    "line {number}: {:?} already has a value",
                    input.key
                );
                script.inputs.push(input);
                continue;
            }
            _ => bail!(
                "line {number}: unknown step {verb:?}; use open, app, with, do, expect or see"
            ),
        };
        script.steps.push((number, step));
    }
    anyhow::ensure!(!script.steps.is_empty(), "the script has no steps");
    Ok(script)
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
    fn a_script_reads_steps_values_and_secrets() {
        let script = parse(
            "# checkout\nopen http://shop.test\nwith email = qa@example.com\nwith password = $PATH\n\ndo sign in\nexpect the cart is empty\nsee \"Total\"\n",
        )
        .unwrap();
        assert_eq!(
            script.steps,
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
        assert!(parse("# only a comment\nwith key = value").is_err());
        assert!(
            parse("with email = a@x\nwith email = b@x\ndo sign in")
                .unwrap_err()
                .to_string()
                .contains("line 2")
        );
        assert!(parse("with \"city\" = Paris\ndo go").is_err());
        assert!(format!("{:#}", parse("with nothing").unwrap_err()).contains("line 1"));
        assert!(
            format!(
                "{:#}",
                parse("with key = $IU2U_UNSET_FOR_TEST").unwrap_err()
            )
            .contains("IU2U_UNSET_FOR_TEST")
        );
    }
}
