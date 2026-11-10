mod common;

use std::io::{BufRead, BufReader, Write};
use std::pin::pin;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{answer_always, answer_when, client, fail_always, yes};
use futures_util::{StreamExt, stream};
use iu2u::commands::{self, NextRequest};
use serde_json::json;
use wiremock::MockServer;

#[tokio::test]
async fn filter_keeps_the_lines_for_which_the_statement_holds() {
    let model = MockServer::start().await;
    answer_when(&model, "ERROR", json!({ "holds": yes(0.95) })).await;
    answer_always(&model, json!({ "holds": yes(0.05) })).await;
    let lines =
        ["INFO started", "", "ERROR refused", "  ", "INFO ok"].map(|line| Ok(line.to_string()));
    let kept: Vec<String> = commands::filter(
        &client(&model),
        "This line reports a problem",
        0.5,
        stream::iter(lines),
    )
    .map(Result::unwrap)
    .collect()
    .await;
    assert_eq!(kept, ["ERROR refused"]);
    assert_eq!(model.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn filter_passes_each_match_on_before_the_input_ends() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
    let client = client(&model);
    let endless = stream::iter([Ok("ERROR refused".to_string())]).chain(stream::pending());
    let mut kept = pin!(commands::filter(
        &client,
        "This line reports a problem",
        0.5,
        endless
    ));
    let first = tokio::time::timeout(Duration::from_secs(5), kept.next())
        .await
        .expect("filter waited for the end of its input");
    assert_eq!(first.unwrap().unwrap(), "ERROR refused");
}

#[test]
fn filter_exits_like_grep() {
    let exit_code = |timeout_ms: &str| {
        Command::new(env!("CARGO_BIN_EXE_iu2u"))
            .args(["filter", "This line reports a problem"])
            .env("IU2U_MODEL", "model-x")
            .env("IU2U_TIMEOUT_MS", timeout_ms)
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(exit_code("1000"), Some(1));
    assert_eq!(exit_code("soon"), Some(2));
}

#[test]
fn filter_rejects_a_threshold_that_is_not_a_probability() {
    for at in ["80", "-1", "NaN"] {
        let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
            .args([
                "filter",
                "This line reports a problem",
                &format!("--at={at}"),
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("between 0 and 1"));
    }
}

#[tokio::test]
async fn filter_stops_on_an_error_while_its_input_stays_open() {
    let model = MockServer::start().await;
    fail_always(&model, 503).await;
    let mut filter = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["filter", "This line reports a problem"])
        .env("IU2U_BASE_URL", model.uri())
        .env("IU2U_MODEL", "model-x")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = filter.stdin.take().unwrap();
    input.write_all(b"ERROR refused\n").unwrap();
    let exited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = filter.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("filter waited for its input to end");
    assert_eq!(exited.code(), Some(2));
    drop(input);
}

#[tokio::test]
async fn next_picks_an_action_and_reports_progress() {
    let model = MockServer::start().await;
    answer_always(&model, json!({
        "action": { "type": "choice", "choice": "login", "confidence": 0.7, "probabilities": { "login": 0.8, "help": 0.2 } },
        "done": yes(0.1),
        "stuck": yes(0.05)
    }))
    .await;
    let request: NextRequest = serde_json::from_value(json!({
        "goal": "sign in",
        "screen": "Welcome. [login] [help]",
        "actions": { "login": "Click Log in", "help": "Click Help" }
    }))
    .unwrap();
    let result = commands::next(&client(&model), request).await.unwrap();
    assert_eq!(
        result,
        json!({ "action": "login", "confidence": 0.7, "done": 0.1, "stuck": 0.05 })
    );
}

#[tokio::test]
async fn next_needs_a_real_choice() {
    let model = MockServer::start().await;
    let request: NextRequest =
        serde_json::from_value(json!({ "goal": "g", "screen": "s", "actions": { "only": "One" } }))
            .unwrap();
    assert!(commands::next(&client(&model), request).await.is_err());
}

#[tokio::test]
async fn filter_stops_quietly_when_its_reader_goes_away() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
    let mut filter = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["filter", "This line reports a problem"])
        .env("IU2U_BASE_URL", model.uri())
        .env("IU2U_MODEL", "model-x")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = filter.stdin.take().unwrap();
    let mut output = BufReader::new(filter.stdout.take().unwrap());
    input.write_all(b"ERROR one\n").unwrap();
    let mut first = String::new();
    output.read_line(&mut first).unwrap();
    assert_eq!(first, "ERROR one\n");
    drop(output);
    input.write_all(b"ERROR two\n").unwrap();
    drop(input);
    let finished = filter.wait_with_output().unwrap();
    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&finished.stderr), "");
}

#[tokio::test]
async fn filter_reads_past_lines_that_are_not_utf8() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
    let mut filter = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["filter", "This line reports a problem"])
        .env("IU2U_BASE_URL", model.uri())
        .env("IU2U_MODEL", "model-x")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    filter
        .stdin
        .take()
        .unwrap()
        .write_all(b"caf\xe9 latin1\r\nthird ok\n")
        .unwrap();
    let finished = filter.wait_with_output().unwrap();
    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(finished.stdout).unwrap(),
        "caf\u{fffd} latin1\nthird ok\n"
    );
}
