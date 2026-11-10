use anyhow::Result;
use serde_json::Value;

use super::{permission, within_hook_timeout};
use crate::model::{Answers, Client, Question, Questions};

const MAX_COMMAND_CHARS: usize = 4000;
const BLOCK_AT: f64 = 0.85;
const ESCALATE_AT: f64 = 0.5;
const UNATTENDED_MODES: [&str; 3] = ["auto", "dontAsk", "bypassPermissions"];

#[derive(Debug, PartialEq)]
enum Verdict {
    Block(&'static str),
    Escalate(&'static str),
    Pass,
}

pub(super) async fn run(input: &Value, client: Result<Client>) -> String {
    let command = input
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let permission_mode = input["permission_mode"].as_str().unwrap_or_default();
    let host_can_ask = input.get("turn_id").is_none();
    let judged = if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
        None
    } else {
        within_hook_timeout(async { client?.ask(command, &questions()).await })
            .await
            .inspect_err(|error| eprintln!("iu2u Guard: {error:#}"))
            .ok()
            .and_then(|answers| verdict(&answers))
    };
    let verdict = judged.unwrap_or_else(|| {
        if host_can_ask {
            unjudged(permission_mode)
        } else {
            Verdict::Pass
        }
    });
    hook_output(&verdict, host_can_ask)
}

fn questions() -> Questions {
    Questions::from([
        (
            "irreversible".into(),
            Question::yesno(
                "Running this command would delete, overwrite or corrupt files, data, git history or system state in a way that is hard to recover.",
            ),
        ),
        (
            "leaves_machine".into(),
            Question::yesno(
                "Running this command would act outside this computer: publishing or pushing code, deploying, uploading files or secrets, sending messages or payments, or changing remote systems, accounts or permissions. Only reading or downloading does not count.",
            ),
        ),
        (
            "worst_case".into(),
            Question::scale(
                "Worst realistic outcome of running this command.",
                &[
                    "Nothing of value is at risk",
                    "Local work could be lost",
                    "Shared data, other people or production could be harmed",
                ],
            ),
        ),
    ])
}

fn verdict(answers: &Answers) -> Option<Verdict> {
    let irreversible = answers.get("irreversible")?.yes()?;
    let leaves_machine = answers.get("leaves_machine")?.yes()?;
    let severe = answers.get("worst_case")?.probability_of("2")?;
    Some(if irreversible >= BLOCK_AT {
        Verdict::Block("iu2u guard: this command looks irreversible")
    } else if irreversible >= ESCALATE_AT {
        Verdict::Escalate("iu2u guard: this command may remove or overwrite data")
    } else if leaves_machine >= ESCALATE_AT {
        Verdict::Escalate("iu2u guard: this command reaches beyond this computer")
    } else if severe >= ESCALATE_AT {
        Verdict::Escalate("iu2u guard: this command could cause serious harm")
    } else {
        Verdict::Pass
    })
}

fn unjudged(permission_mode: &str) -> Verdict {
    if UNATTENDED_MODES.contains(&permission_mode) {
        Verdict::Escalate("iu2u guard: the decision model could not judge this command")
    } else {
        Verdict::Pass
    }
}

fn hook_output(verdict: &Verdict, host_can_ask: bool) -> String {
    let (decision, reason) = match verdict {
        Verdict::Block(reason) => ("deny", reason),
        Verdict::Escalate(reason) if host_can_ask => ("ask", reason),
        Verdict::Escalate(reason) => ("deny", reason),
        Verdict::Pass => return "{}".into(),
    };
    permission(decision, reason)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::Answer;

    fn answers(irreversible: f64, leaves_machine: f64, severe: f64) -> Answers {
        Answers::from([
            ("irreversible".into(), Answer::Yesno { yes: irreversible }),
            (
                "leaves_machine".into(),
                Answer::Yesno {
                    yes: leaves_machine,
                },
            ),
            (
                "worst_case".into(),
                Answer::Scale {
                    level: 2.0 * severe,
                    confidence: None,
                    probabilities: BTreeMap::from([
                        ("0".into(), 1.0 - severe),
                        ("1".into(), 0.0),
                        ("2".into(), severe),
                    ]),
                },
            ),
        ])
    }

    fn decision(verdict: &Verdict, host_can_ask: bool) -> Value {
        serde_json::from_str::<Value>(&hook_output(verdict, host_can_ask)).unwrap()
            ["hookSpecificOutput"]["permissionDecision"]
            .clone()
    }

    #[test]
    fn blocks_confident_irreversible_commands() {
        assert!(matches!(
            verdict(&answers(0.9, 0.0, 0.0)),
            Some(Verdict::Block(_))
        ));
    }

    #[test]
    fn escalates_on_each_uncertain_signal() {
        for signals in [(0.6, 0.0, 0.0), (0.1, 0.7, 0.0), (0.1, 0.1, 0.6)] {
            assert!(matches!(
                verdict(&answers(signals.0, signals.1, signals.2)),
                Some(Verdict::Escalate(_))
            ));
        }
    }

    #[test]
    fn passes_when_every_signal_is_low() {
        assert_eq!(verdict(&answers(0.1, 0.1, 0.1)), Some(Verdict::Pass));
    }

    #[test]
    fn missing_answers_are_unjudged() {
        assert_eq!(verdict(&Answers::new()), None);
    }

    #[test]
    fn unjudged_escalates_only_when_nobody_is_watching() {
        assert!(matches!(unjudged("auto"), Verdict::Escalate(_)));
        assert!(matches!(
            unjudged("bypassPermissions"),
            Verdict::Escalate(_)
        ));
        assert_eq!(unjudged("default"), Verdict::Pass);
        assert_eq!(unjudged(""), Verdict::Pass);
    }

    #[test]
    fn hook_output_never_allows() {
        assert_eq!(hook_output(&Verdict::Pass, true), "{}");
        assert_eq!(decision(&Verdict::Block("x"), true), "deny");
        assert_eq!(decision(&Verdict::Escalate("x"), true), "ask");
        assert_eq!(decision(&Verdict::Escalate("x"), false), "deny");
    }
}
