mod common;

use std::time::Duration;

use common::{
    answer_always, answer_when, client, client_with_timeout, fail_always, reply, temp_path, yes,
};
use iu2u::model::{Answers, CONCURRENCY, Client, Question, Questions};
use iu2u::tools::{self, LintParams, RankParams, SeekParams, WeighParams};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method},
};

async fn skipped_after_a_silent_model(client: &Client) -> usize {
    let questions = Questions::from([("refund".into(), Question::yesno("Asks for a refund."))]);
    let results: Vec<anyhow::Result<Answers>> = client
        .ask_each(vec!["charged twice".into(); 40], &questions)
        .await;
    results
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .is_err_and(|error| error.to_string().starts_with("skipped"))
        })
        .count()
}

#[tokio::test]
async fn weigh_reports_answers_and_errors_per_item() {
    let model = MockServer::start().await;
    answer_when(&model, "charged twice", json!({ "refund": yes(0.9) })).await;
    fail_always(&model, 422).await;
    let params: WeighParams = serde_json::from_value(json!({
        "items": ["charged twice", "hello"],
        "questions": { "refund": { "kind": "yesno", "text": "Asks for a refund." } }
    }))
    .unwrap();
    let results = tools::weigh(&client(&model), params).await.unwrap();
    assert_eq!(
        results[0]["answers"]["refund"],
        json!({ "kind": "yesno", "yes": 0.9 })
    );
    assert!(results[1]["error"].as_str().unwrap().contains("422"));
}

#[tokio::test]
async fn errors_quote_only_the_start_of_the_server_response() {
    let model = MockServer::start().await;
    answer_when(&model, "charged twice", json!({ "refund": yes(0.9) })).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>".repeat(1_000)))
        .mount(&model)
        .await;
    let params: WeighParams = serde_json::from_value(json!({
        "items": ["charged twice", "hello"],
        "questions": { "refund": { "kind": "yesno", "text": "Asks for a refund." } }
    }))
    .unwrap();
    let results = tools::weigh(&client(&model), params).await.unwrap();
    let error = results[1]["error"].as_str().unwrap();
    assert!(error.contains("502") && error.chars().count() < 500);
}

#[tokio::test]
async fn weigh_answers_carry_the_pick_and_its_confidence_only() {
    let model = MockServer::start().await;
    answer_always(
        &model,
        json!({ "team": {
            "type": "choice", "choice": "billing", "confidence": 0.8,
            "probabilities": { "billing": 0.8, "support": 0.2 }
        }}),
    )
    .await;
    let params: WeighParams = serde_json::from_value(json!({
        "items": ["charged twice"],
        "questions": { "team": {
            "kind": "pick",
            "text": "Which team handles this?",
            "options": { "billing": "Payments", "support": "Everything else" }
        }}
    }))
    .unwrap();
    let results = tools::weigh(&client(&model), params).await.unwrap();
    assert_eq!(
        results[0]["answers"]["team"],
        json!({ "kind": "pick", "pick": "billing", "confidence": 0.8 })
    );
}

#[tokio::test]
async fn weigh_reports_questions_the_model_left_unanswered() {
    let model = MockServer::start().await;
    answer_always(&model, json!({})).await;
    let params: WeighParams = serde_json::from_value(json!({
        "items": ["charged twice"],
        "questions": { "refund": { "kind": "yesno", "text": "Asks for a refund." } }
    }))
    .unwrap();
    assert!(
        tools::weigh(&client(&model), params)
            .await
            .unwrap_err()
            .contains("left questions unanswered")
    );
}

#[tokio::test]
async fn batches_stop_once_the_model_cannot_be_reached() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let client = Client::new(
        &format!("http://{closed}"),
        None,
        "model-x".into(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(skipped_after_a_silent_model(&client).await >= 40 - CONCURRENCY);
}

#[tokio::test]
async fn batches_stop_once_the_model_rejects_the_client() {
    let model = MockServer::start().await;
    fail_always(&model, 401).await;
    assert!(skipped_after_a_silent_model(&client(&model)).await >= 40 - CONCURRENCY);
    assert!(model.received_requests().await.unwrap().len() <= CONCURRENCY);
}

#[tokio::test]
async fn batches_stop_once_the_model_stops_answering_midway() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(json!({ "refund": yes(0.9) })))
        .up_to_n_times(1)
        .mount(&model)
        .await;
    Mock::given(method("POST"))
        .respond_with(reply(json!({ "refund": yes(0.9) })).set_delay(Duration::from_secs(30)))
        .mount(&model)
        .await;
    let client = client_with_timeout(&model, Duration::from_secs(1));
    let started = std::time::Instant::now();
    assert!(skipped_after_a_silent_model(&client).await >= 40 - 2 * CONCURRENCY);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn slow_items_do_not_hold_up_the_items_after_them() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("slow"))
        .respond_with(reply(json!({ "refund": yes(0.1) })).set_delay(Duration::from_secs(1)))
        .mount(&model)
        .await;
    answer_always(&model, json!({ "refund": yes(0.9) })).await;
    let questions = Questions::from([("refund".into(), Question::yesno("Asks for a refund."))]);
    let states: Vec<String> = (0..24)
        .map(|index| {
            if index % 8 == 0 {
                "slow"
            } else {
                "charged twice"
            }
            .to_string()
        })
        .collect();
    let started = std::time::Instant::now();
    let results = client_with_timeout(&model, Duration::from_secs(5))
        .ask_each(states, &questions)
        .await;
    assert!(started.elapsed() < Duration::from_secs(2));
    let yes = |index: usize| results[index].as_ref().unwrap()["refund"].yes();
    assert_eq!((yes(0), yes(1), yes(8)), (Some(0.1), Some(0.9), Some(0.1)));
}

#[tokio::test]
async fn one_slow_item_does_not_stop_a_batch_the_model_answers() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("slow"))
        .respond_with(reply(json!({ "refund": yes(0.9) })).set_delay(Duration::from_secs(3)))
        .mount(&model)
        .await;
    answer_always(&model, json!({ "refund": yes(0.9) })).await;
    let client = client_with_timeout(&model, Duration::from_secs(1));
    let questions = Questions::from([("refund".into(), Question::yesno("Asks for a refund."))]);
    let mut states = vec!["slow".to_string()];
    states.extend(vec!["charged twice".to_string(); 20]);
    let results = client.ask_each(states, &questions).await;
    assert!(results[0].is_err());
    assert!(results[1..].iter().all(Result::is_ok));
}

#[tokio::test]
async fn weigh_rejects_empty_batches() {
    let model = MockServer::start().await;
    let params: WeighParams =
        serde_json::from_value(json!({ "items": [], "questions": {} })).unwrap();
    assert!(tools::weigh(&client(&model), params).await.is_err());
}

#[tokio::test]
async fn rank_orders_items_by_level() {
    let model = MockServer::start().await;
    let scale =
        |level: f64| json!({ "rank": { "type": "score", "score": level, "probabilities": {} } });
    answer_when(&model, "minor typo", scale(0.1)).await;
    answer_when(&model, "data loss", scale(1.9)).await;
    answer_always(&model, scale(1.0)).await;
    let params = RankParams {
        items: vec!["minor typo".into(), "data loss".into(), "slow query".into()],
        criterion: "How severe is this finding?".into(),
        levels: None,
    };
    let ranked = tools::rank(&client(&model), params).await.unwrap();
    let order: Vec<u64> = ranked
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["item"].as_u64().unwrap())
        .collect();
    assert_eq!(order, [1, 2, 0]);
}

#[tokio::test]
async fn seek_returns_relevant_excerpts_and_skips_unreadable_files() {
    let model = MockServer::start().await;
    answer_when(&model, "retry", json!({ "relevant": yes(0.92) })).await;
    answer_always(&model, json!({ "relevant": yes(0.04) })).await;
    let file = temp_path("seek.rs");
    let mut lines: Vec<String> = (1..=10)
        .map(|i| format!("fn unrelated_{i}() {{ {} }}", "x".repeat(80)))
        .collect();
    lines.push("fn with_retry() { loop { attempt() } }".into());
    std::fs::write(&file, lines.join("\n")).unwrap();
    let params = SeekParams {
        query: "where are retries handled".into(),
        files: vec![file.display().to_string(), "/nonexistent/iu2u".into()],
        max_excerpt_chars: Some(200),
    };
    let found = tools::seek(&client(&model), params).await.unwrap();
    assert_eq!(found["excerpts"], 6);
    assert_eq!(found["matches"].as_array().unwrap().len(), 1);
    assert_eq!(found["total"], 1);
    assert_eq!(found["matches"][0]["lines"], "11-11");
    assert!(
        found["skipped"]["/nonexistent/iu2u"]
            .as_str()
            .unwrap()
            .contains("cannot read")
    );
}

#[tokio::test]
async fn seek_rejects_excerpt_sizes_outside_the_allowed_range() {
    let model = MockServer::start().await;
    for size in [1, 1_000_000] {
        let params = SeekParams {
            query: "anything".into(),
            files: vec!["/nonexistent/iu2u".into()],
            max_excerpt_chars: Some(size),
        };
        assert!(
            tools::seek(&client(&model), params)
                .await
                .unwrap_err()
                .contains("from 200 to 8000")
        );
    }
}

#[tokio::test]
async fn seek_reports_why_the_model_failed_on_every_excerpt() {
    let model = MockServer::start().await;
    fail_always(&model, 503).await;
    let file = temp_path("seek-down.rs");
    std::fs::write(&file, "fn with_retry() {}").unwrap();
    let params = SeekParams {
        query: "where are retries handled".into(),
        files: vec![file.display().to_string()],
        max_excerpt_chars: None,
    };
    assert!(
        tools::seek(&client(&model), params)
            .await
            .unwrap_err()
            .contains("503")
    );
}

#[tokio::test]
async fn seek_finds_nothing_in_empty_files() {
    let model = MockServer::start().await;
    let file = temp_path("empty.py");
    std::fs::write(&file, "").unwrap();
    let params = SeekParams {
        query: "anything".into(),
        files: vec![file.display().to_string(), "/nonexistent/iu2u".into()],
        max_excerpt_chars: None,
    };
    let found = tools::seek(&client(&model), params).await.unwrap();
    assert_eq!(found["excerpts"], 0);
    assert_eq!(found["matches"], json!([]));
    assert!(found["skipped"]["/nonexistent/iu2u"].is_string());
}

#[tokio::test]
async fn seek_fails_when_nothing_is_readable() {
    let model = MockServer::start().await;
    let params = SeekParams {
        query: "anything".into(),
        files: vec!["/nonexistent/iu2u".into()],
        max_excerpt_chars: None,
    };
    assert!(
        tools::seek(&client(&model), params)
            .await
            .unwrap_err()
            .contains("cannot read")
    );
}

#[tokio::test]
async fn files_over_the_size_limit_are_not_read() {
    let model = MockServer::start().await;
    let file = temp_path("huge.log");
    std::fs::write(&file, "x".repeat((1 << 20) + 1)).unwrap();
    let params = SeekParams {
        query: "anything".into(),
        files: vec![file.display().to_string()],
        max_excerpt_chars: None,
    };
    assert!(
        tools::seek(&client(&model), params)
            .await
            .unwrap_err()
            .contains("larger than 1048576 bytes")
    );
    std::fs::remove_file(&file).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn special_files_are_not_read() {
    let model = MockServer::start().await;
    let params = SeekParams {
        query: "anything".into(),
        files: vec!["/dev/null".into()],
        max_excerpt_chars: None,
    };
    assert!(
        tools::seek(&client(&model), params)
            .await
            .unwrap_err()
            .contains("not a regular file")
    );
}

#[tokio::test]
async fn lint_fails_when_it_could_judge_no_file() {
    let model = MockServer::start().await;
    fail_always(&model, 503).await;
    let file = temp_path("lint-down.rs");
    std::fs::write(&file, "let x = parse().unwrap();").unwrap();
    let client = client(&model);
    let lint = |files: Vec<String>| {
        tools::lint(
            &client,
            LintParams {
                rules: vec!["Errors are never ignored".into()],
                files,
            },
        )
    };
    assert!(
        lint(vec![file.display().to_string()])
            .await
            .unwrap_err()
            .contains("503")
    );
    assert!(
        lint(vec!["/nonexistent/iu2u".into()])
            .await
            .unwrap_err()
            .contains("nothing readable")
    );
}

#[tokio::test]
async fn lint_reports_likely_violations_per_file() {
    let model = MockServer::start().await;
    answer_when(
        &model,
        "unwrap()",
        json!({ "rule_0": yes(0.88), "rule_1": yes(0.1) }),
    )
    .await;
    answer_always(&model, json!({ "rule_0": yes(0.02), "rule_1": yes(0.03) })).await;
    let dir = temp_path("lint");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.rs"), "let x = parse().unwrap();").unwrap();
    std::fs::write(dir.join("good.rs"), "let x = parse()?;").unwrap();
    let file = |name: &str| dir.join(name).display().to_string();
    let params = LintParams {
        rules: vec![
            "Errors are never ignored or turned into panics".into(),
            "Names are in English".into(),
        ],
        files: vec![file("bad.rs"), file("good.rs"), file("missing.rs")],
    };
    let report = tools::lint(&client(&model), params).await.unwrap();
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0]["file"], file("bad.rs"));
    assert_eq!(violations[0]["lines"], "1-1");
    assert_eq!(
        violations[0]["rule"],
        "Errors are never ignored or turned into panics"
    );
    assert!(
        report["failed"][file("missing.rs")]
            .as_str()
            .unwrap()
            .contains("cannot read")
    );
}

#[tokio::test]
async fn lint_finds_violations_deep_in_long_files() {
    let model = MockServer::start().await;
    answer_when(&model, "unwrap()", json!({ "rule_0": yes(0.88) })).await;
    answer_always(&model, json!({ "rule_0": yes(0.02) })).await;
    let mut text: String = (0..300)
        .map(|index| format!("fn checked_{index}() {{\n    let value = parse({index})?;\n}}\n\n"))
        .collect();
    text.push_str("fn last() {\n    let value = parse().unwrap();\n}\n");
    let file = temp_path("long.rs");
    std::fs::write(&file, &text).unwrap();
    let params = LintParams {
        rules: vec!["Errors are never ignored or turned into panics".into()],
        files: vec![file.display().to_string()],
    };
    let report = tools::lint(&client(&model), params).await.unwrap();
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 1);
    let lines = violations[0]["lines"].as_str().unwrap();
    let (first, last) = lines.split_once('-').unwrap();
    assert!(first.parse::<usize>().unwrap() > 1);
    assert_eq!(last.parse::<usize>().unwrap(), text.lines().count());
    assert!(model.received_requests().await.unwrap().len() > 1);
}

#[tokio::test]
async fn lint_returns_the_most_likely_violations_and_their_total() {
    let model = MockServer::start().await;
    let rules: Vec<String> = (0..25)
        .map(|index| format!("Rule number {index}"))
        .collect();
    let answers: serde_json::Map<String, serde_json::Value> = (0..25)
        .map(|index| (format!("rule_{index}"), yes(0.5 + f64::from(index) / 100.0)))
        .collect();
    answer_always(&model, answers.into()).await;
    let file = temp_path("many-rules.rs");
    std::fs::write(&file, "let x = parse().unwrap();").unwrap();
    let params = LintParams {
        rules,
        files: vec![file.display().to_string()],
    };
    let report = tools::lint(&client(&model), params).await.unwrap();
    assert_eq!(report["total"], 25);
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 20);
    assert_eq!(violations[0]["rule"], "Rule number 24");
}

#[tokio::test]
async fn lint_lists_every_line_range_it_could_not_judge() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("broken"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&model)
        .await;
    answer_always(&model, json!({ "rule_0": yes(0.02) })).await;
    let blocks: Vec<String> = ["ok", "broken", "broken", "ok", "broken"]
        .iter()
        .map(|kind| format!("{kind}{}", "x".repeat(1500)))
        .collect();
    let file = temp_path("partly-judged.rs");
    std::fs::write(&file, blocks.join("\n\n")).unwrap();
    let file = file.display().to_string();
    let params = LintParams {
        rules: vec!["Names are in English".into()],
        files: vec![file.clone()],
    };
    let report = tools::lint(&client(&model), params).await.unwrap();
    assert!(
        report["failed"][&file]
            .as_str()
            .unwrap()
            .starts_with("lines 3-6, 9-9: ")
    );
}
