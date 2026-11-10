mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use common::{
    answer_always, answer_when, client, client_with_timeout, fail_always, reply, temp_path, yes,
};
use iu2u::{
    hooks::Power,
    model::{Client, Questions},
};
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, matchers::method};

async fn run(power: Power, input: &Value, client: &Client) -> String {
    power.run(input, Ok(client.clone())).await.unwrap()
}

fn permission(output: &str) -> Value {
    serde_json::from_str::<Value>(output).unwrap()["hookSpecificOutput"]["permissionDecision"]
        .clone()
}

fn irreversible() -> Value {
    json!({
        "irreversible": yes(0.95),
        "leaves_machine": yes(0.1),
        "worst_case": { "type": "score", "score": 2.0, "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0 } }
    })
}

fn hook(power: &str, env: &[(&str, &str)], input: &Value) -> String {
    let mut hook = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["hook", power])
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    String::from_utf8(hook.wait_with_output().unwrap().stdout).unwrap()
}

fn guard_hook(env: &[(&str, &str)]) -> String {
    hook(
        "guard",
        env,
        &json!({ "permission_mode": "auto", "tool_input": { "command": "rm -rf ~" } }),
    )
}

#[tokio::test]
async fn guard_falls_back_by_host_and_mode_when_the_model_is_down() {
    let model = MockServer::start().await;
    fail_always(&model, 503).await;
    let client = client(&model);
    let claude = |mode: &str| json!({ "permission_mode": mode, "tool_input": { "command": "ls" } });
    assert_eq!(run(Power::Guard, &claude("default"), &client).await, "{}");
    assert_eq!(
        permission(&run(Power::Guard, &claude("auto"), &client).await),
        "ask"
    );
    let codex = json!({ "turn_id": "t1", "tool_input": { "command": "ls" } });
    assert_eq!(run(Power::Guard, &codex, &client).await, "{}");
}

#[tokio::test]
async fn guard_blocks_what_the_model_judges_irreversible() {
    let model = MockServer::start().await;
    answer_always(&model, irreversible()).await;
    let input = json!({ "permission_mode": "default", "tool_input": { "command": "rm -rf ~" } });
    assert_eq!(
        permission(&run(Power::Guard, &input, &client(&model)).await),
        "deny"
    );
}

#[tokio::test]
async fn guard_hook_reads_the_plugin_options() {
    let model = MockServer::start().await;
    answer_always(&model, irreversible()).await;
    let base_url = model.uri();
    let options = [
        ("CLAUDE_PLUGIN_OPTION_BASE_URL", base_url.as_str()),
        ("CLAUDE_PLUGIN_OPTION_MODEL", "model-x"),
    ];
    assert_eq!(permission(&guard_hook(&options)), "deny");
    let switched_off = [
        options.as_slice(),
        &[("CLAUDE_PLUGIN_OPTION_GUARD", "false")],
    ]
    .concat();
    assert_eq!(guard_hook(&switched_off), "");
}

#[tokio::test]
async fn unknown_switch_values_keep_the_default() {
    let model = MockServer::start().await;
    answer_always(&model, irreversible()).await;
    let base_url = model.uri();
    let options = [
        ("CLAUDE_PLUGIN_OPTION_BASE_URL", base_url.as_str()),
        ("CLAUDE_PLUGIN_OPTION_MODEL", "model-x"),
        ("CLAUDE_PLUGIN_OPTION_GUARD", "1"),
    ];
    assert_eq!(permission(&guard_hook(&options)), "deny");
}

#[tokio::test]
async fn plugin_options_win_over_environment_variables() {
    let model = MockServer::start().await;
    answer_always(&model, irreversible()).await;
    let base_url = model.uri();
    let env = [
        ("CLAUDE_PLUGIN_OPTION_BASE_URL", base_url.as_str()),
        ("CLAUDE_PLUGIN_OPTION_MODEL", "model-x"),
        ("IU2U_BASE_URL", "http://127.0.0.1:9"),
    ];
    assert_eq!(permission(&guard_hook(&env)), "deny");
}

#[tokio::test]
async fn an_empty_plugin_api_key_is_not_replaced_by_the_shell_one() {
    let model = MockServer::start().await;
    answer_always(&model, irreversible()).await;
    let base_url = model.uri();
    let env = [
        ("CLAUDE_PLUGIN_OPTION_BASE_URL", base_url.as_str()),
        ("CLAUDE_PLUGIN_OPTION_MODEL", "model-x"),
        ("CLAUDE_PLUGIN_OPTION_API_KEY", ""),
        ("IU2U_API_KEY", "shell-secret"),
    ];
    assert_eq!(permission(&guard_hook(&env)), "deny");
    let requests = model.received_requests().await.unwrap();
    assert!(requests[0].headers.get("authorization").is_none());
}

#[test]
fn switched_off_powers_still_read_their_whole_input() {
    let mut hook = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["hook", "trim"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({ "tool_response": { "stdout": "x".repeat(200_000) } });
    hook.stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let finished = hook.wait_with_output().unwrap();
    assert!(finished.status.success() && finished.stdout.is_empty());
}

#[test]
fn hook_usage_errors_never_block() {
    for args in [
        ["hook", "renamed"].as_slice(),
        &["hook", "guard", "--new-flag"],
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_iu2u"))
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(1), "{args:?}");
    }
}

async fn connected_client_to_a_model_that_stalls(answers: Value) -> (MockServer, Client) {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(answers.clone()))
        .up_to_n_times(1)
        .mount(&model)
        .await;
    Mock::given(method("POST"))
        .respond_with(reply(answers).set_delay(Duration::from_secs(30)))
        .mount(&model)
        .await;
    let client = client_with_timeout(&model, Duration::from_secs(60));
    client
        .ask("open the connection", &Questions::new())
        .await
        .unwrap();
    tokio::time::pause();
    (model, client)
}

#[tokio::test]
async fn guard_gives_up_before_its_hook_times_out() {
    let (_model, slow) = connected_client_to_a_model_that_stalls(irreversible()).await;
    let input = json!({ "permission_mode": "auto", "tool_input": { "command": "rm -rf ~" } });
    let started = tokio::time::Instant::now();
    assert_eq!(permission(&run(Power::Guard, &input, &slow).await), "ask");
    assert!((12..15).contains(&started.elapsed().as_secs()));
}

#[tokio::test]
async fn proof_gives_up_before_its_hook_times_out() {
    let (_model, slow) = connected_client_to_a_model_that_stalls(
        json!({ "finished": yes(0.95), "verified": yes(0.05) }),
    )
    .await;
    let stop = json!({ "hook_event_name": "Stop", "last_assistant_message": "All done!" });
    let started = tokio::time::Instant::now();
    assert_eq!(run(Power::Proof, &stop, &slow).await, "");
    assert!((12..15).contains(&started.elapsed().as_secs()));
}

#[test]
fn unconfigured_guard_asks_when_nobody_is_watching() {
    assert_eq!(permission(&guard_hook(&[])), "ask");
}

#[tokio::test]
async fn guard_does_not_judge_oversized_commands() {
    let model = MockServer::start().await;
    answer_always(&model, json!({})).await;
    let input = json!({ "permission_mode": "auto", "tool_input": { "command": "x".repeat(4001) } });
    assert_eq!(
        permission(&run(Power::Guard, &input, &client(&model)).await),
        "ask"
    );
    assert!(model.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn trim_hides_irrelevant_blocks_and_keeps_the_full_output() {
    let model = MockServer::start().await;
    answer_when(&model, "noise", json!({ "matters": yes(0.05) })).await;
    answer_always(&model, json!({ "matters": yes(0.9) })).await;
    let mut lines = vec!["first line".to_string()];
    lines.extend((0..400).map(|i| format!("noise {i} {}", "x".repeat(40))));
    lines.extend((0..39).map(|i| format!("result {i}")));
    lines.push("error: something broke".into());
    lines.extend((0..80).map(|i| format!("noise tail {i}")));
    lines.push("last line".into());
    let stdout = lines.join("\n");
    let input = json!({
        "tool_use_id": "toolu_trim",
        "tool_input": { "command": "cargo build", "description": "Build the project" },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false }
    });
    let data = temp_path("plugin-data");
    let base_url = model.uri();
    let env = [
        ("CLAUDE_PLUGIN_DATA", data.to_str().unwrap()),
        ("IU2U_BASE_URL", base_url.as_str()),
        ("IU2U_MODEL", "model-x"),
        ("IU2U_TRIM", "true"),
    ];
    let output: Value = serde_json::from_str(&hook("trim", &env, &input)).unwrap();
    let updated = &output["hookSpecificOutput"]["updatedToolOutput"];
    let trimmed = updated["stdout"].as_str().unwrap();
    assert!(trimmed.starts_with("first line") && trimmed.ends_with("last line"));
    assert!(trimmed.contains("error: something broke"));
    assert!(trimmed.len() < stdout.len() / 2);
    assert_eq!(updated["interrupted"], false);
    let saved = trimmed
        .split("the full output is in ")
        .nth(1)
        .and_then(|rest| rest.split(" for 24 hours]").next())
        .unwrap();
    assert!(Path::new(saved).starts_with(data.join("trimmed")));
    assert_eq!(std::fs::read_to_string(saved).unwrap(), stdout);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(saved).unwrap().permissions())
            & 0o777,
        0o600
    );
    let expired = Path::new(saved).with_file_name("expired.txt");
    std::fs::File::create(&expired)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60))
        .unwrap();
    let mut next = input.clone();
    next["tool_use_id"] = json!("toolu_trim_next");
    hook("trim", &env, &next);
    assert!(!expired.exists());
    assert!(Path::new(saved).exists());
    std::fs::remove_dir_all(&data).unwrap();
}

#[tokio::test]
async fn trim_needs_the_plugin_data_folder() {
    let model = MockServer::start().await;
    let input = json!({ "tool_response": { "stdout": "x\n".repeat(20_000) } });
    assert_eq!(run(Power::Trim, &input, &client(&model)).await, "");
    assert!(model.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn trim_leaves_short_output_alone() {
    let model = MockServer::start().await;
    let input = json!({ "tool_response": { "stdout": "short" } });
    assert_eq!(run(Power::Trim, &input, &client(&model)).await, "");
}

#[tokio::test]
async fn trim_leaves_output_that_claude_code_already_saved_alone() {
    let model = MockServer::start().await;
    let input = json!({ "tool_response": {
        "stdout": "x".repeat(30_000),
        "persistedOutputPath": "/tmp/full-output.txt"
    } });
    assert_eq!(run(Power::Trim, &input, &client(&model)).await, "");
    assert!(model.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn proof_sends_unverified_claims_of_being_done_back_once() {
    let model = MockServer::start().await;
    answer_when(
        &model,
        "tests pass",
        json!({ "finished": yes(0.95), "verified": yes(0.9) }),
    )
    .await;
    answer_always(
        &model,
        json!({ "finished": yes(0.95), "verified": yes(0.05) }),
    )
    .await;
    let client = client(&model);
    let stop = |message: &str, active: bool| json!({ "hook_event_name": "Stop", "last_assistant_message": message, "stop_hook_active": active });
    let sent_back: Value =
        serde_json::from_str(&run(Power::Proof, &stop("All done!", false), &client).await).unwrap();
    assert_eq!(sent_back["hookSpecificOutput"]["hookEventName"], "Stop");
    assert!(
        sent_back["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .starts_with("iu2u proof:")
    );
    assert_eq!(
        run(Power::Proof, &stop("All done!", true), &client).await,
        ""
    );
    assert_eq!(
        run(
            Power::Proof,
            &stop("Done: cargo test, 12 tests pass", false),
            &client
        )
        .await,
        ""
    );
}

async fn model_picking_deploy() -> MockServer {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "skill": {
        "type": "choice", "choice": "deploy", "confidence": 0.8, "probabilities": { "deploy": 0.9, "none": 0.1 }
    }}))
    .await;
    model
}

fn write_deploy_skill(claude_dir: &Path) {
    let skill = claude_dir.join("skills/deploy");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: deploy\ndescription: Ship the app\n---\nbody",
    )
    .unwrap();
}

fn suggests_deploy(output: &str) -> bool {
    serde_json::from_str::<Value>(output).unwrap()["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("\"deploy\"")
}

fn compass_hook(model: &MockServer, env: &[(&str, &str)], cwd: &Path) -> String {
    let base_url = model.uri();
    let settings = [
        ("IU2U_BASE_URL", base_url.as_str()),
        ("IU2U_MODEL", "model-x"),
        ("IU2U_COMPASS", "true"),
    ];
    hook(
        "compass",
        &[env, &settings].concat(),
        &json!({ "prompt": "ship it", "cwd": cwd }),
    )
}

#[tokio::test]
async fn proof_sends_an_unverified_subagent_handback_back_once() {
    let model = MockServer::start().await;
    answer_always(
        &model,
        json!({ "finished": yes(0.95), "verified": yes(0.05) }),
    )
    .await;
    let data = temp_path("proof-data");
    std::fs::remove_dir_all(&data).ok();
    let base_url = model.uri();
    let env = [
        ("CLAUDE_PLUGIN_DATA", data.to_str().unwrap()),
        ("IU2U_BASE_URL", base_url.as_str()),
        ("IU2U_MODEL", "model-x"),
    ];
    let handback = json!({
        "hook_event_name": "PreToolUse",
        "agent_id": "agent-1",
        "tool_name": "SubagentHandback",
        "tool_input": { "message": "All done!" }
    });
    assert_eq!(permission(&hook("proof", &env, &handback)), "deny");
    assert_eq!(hook("proof", &env, &handback), "");
    let stop = json!({
        "hook_event_name": "SubagentStop",
        "agent_id": "agent-1",
        "stop_hook_active": false,
        "last_assistant_message": "Done."
    });
    assert_eq!(hook("proof", &env, &stop), "");
    assert_eq!(model.received_requests().await.unwrap().len(), 1);
    assert_eq!(permission(&hook("proof", &env, &handback)), "deny");
    std::fs::remove_dir_all(&data).unwrap();
}

#[tokio::test]
async fn compass_suggests_a_confident_skill_pick() {
    let model = model_picking_deploy().await;
    let home = temp_path("compass-pick-home");
    let project = temp_path("compass");
    std::fs::create_dir_all(&home).unwrap();
    write_deploy_skill(&project.join(".claude"));
    let env = [("HOME", home.to_str().unwrap())];
    assert!(suggests_deploy(&compass_hook(&model, &env, &project)));
}

#[tokio::test]
async fn compass_finds_project_skills_after_a_cd() {
    let model = model_picking_deploy().await;
    let home = temp_path("compass-home");
    let project = temp_path("compass-project");
    std::fs::create_dir_all(&home).unwrap();
    write_deploy_skill(&project.join(".claude"));
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("CLAUDE_PROJECT_DIR", project.to_str().unwrap()),
    ];
    assert!(suggests_deploy(&compass_hook(
        &model,
        &env,
        &project.join("backend")
    )));
}

#[tokio::test]
async fn compass_reads_personal_skills_from_the_claude_config_dir() {
    let model = model_picking_deploy().await;
    let home = temp_path("compass-empty-home");
    let config = temp_path("compass-config");
    std::fs::create_dir_all(&home).unwrap();
    write_deploy_skill(&config);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("CLAUDE_CONFIG_DIR", config.to_str().unwrap()),
    ];
    assert!(suggests_deploy(&compass_hook(&model, &env, &home)));
}

#[tokio::test]
async fn prune_lists_steps_to_keep_and_to_drop() {
    let model = MockServer::start().await;
    answer_when(&model, "old-notes", json!({ "needed": yes(0.05) })).await;
    answer_always(&model, json!({ "needed": yes(0.9) })).await;
    let transcript = [
        json!({ "type": "user", "turnOrigin": "human", "origin": { "kind": "human" }, "message": { "content": "look at the old-detour" } }),
        json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Read", "input": { "file_path": "old-detour.md" } }] } }),
        json!({ "type": "system", "subtype": "compact_boundary" }),
        json!({ "type": "user", "turnOrigin": "human", "isCompactSummary": true, "message": { "content": "This session is being continued" } }),
        json!({ "type": "user", "turnOrigin": "human", "origin": { "kind": "human" }, "message": { "content": [{ "type": "text", "text": "fix the login bug" }, { "type": "image" }] } }),
        json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "ignored" }] } }),
        json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Read", "input": { "file_path": "old-notes.md" } }] } }),
        json!({ "type": "user", "message": { "content": "Stop hook feedback: run the tests" } }),
        json!({ "type": "user", "message": { "content": "/compact" } }),
        json!({ "type": "user", "turnOrigin": "human", "message": { "content": "<local-command-stdout>Set model</local-command-stdout>" } }),
        json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Edit", "input": { "file_path": "src/login.rs" } }] } }),
        json!({ "type": "user", "turnOrigin": "human", "origin": { "kind": "human" }, "message": { "content": "yes" } }),
    ]
    .map(|entry| entry.to_string())
    .join("\n");
    let path = temp_path("transcript.jsonl");
    std::fs::write(&path, transcript).unwrap();
    let output = run(
        Power::Prune,
        &json!({ "transcript_path": path }),
        &client(&model),
    )
    .await;
    let (keep, drop) = output.split_once("Reduce").unwrap();
    assert!(keep.contains("src/login.rs") && keep.contains("fix the login bug"));
    assert!(drop.contains("old-notes"));
    for left_out in [
        "old-detour",
        "continued",
        "ignored",
        "feedback",
        "/compact",
        "local-command",
    ] {
        assert!(!output.contains(left_out), "{left_out}");
    }
    for request in model.received_requests().await.unwrap() {
        assert!(
            String::from_utf8_lossy(&request.body)
                .contains("latest requests: look at the old-detour | fix the login bug | yes.")
        );
    }
}

#[tokio::test]
async fn prune_follows_what_the_user_typed_after_compact() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "needed": yes(0.9) })).await;
    let path = temp_path("compact-transcript.jsonl");
    std::fs::write(
        &path,
        json!({ "type": "user", "origin": { "kind": "human" }, "message": { "content": "fix the login bug" } })
            .to_string(),
    )
    .unwrap();
    let input = json!({
        "transcript_path": path,
        "custom_instructions": "keep every detail of the migration"
    });
    run(Power::Prune, &input, &client(&model)).await;
    for request in model.received_requests().await.unwrap() {
        assert!(
            String::from_utf8_lossy(&request.body)
                .contains("fix the login bug | keep every detail of the migration.")
        );
    }
}

#[tokio::test]
async fn prune_fits_both_lists_in_the_hook_output_cap() {
    let model = MockServer::start().await;
    answer_when(&model, "old-notes", json!({ "needed": yes(0.05) })).await;
    answer_always(&model, json!({ "needed": yes(0.9) })).await;
    let mut transcript = vec![
        json!({ "type": "user", "turnOrigin": "human", "origin": { "kind": "human" }, "message": { "content": "fix the login bug" } }),
    ];
    transcript.extend((0..100).map(|i| {
        json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": format!("step {i} {}", "x".repeat(300)) }] } })
    }));
    transcript.push(json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Read", "input": { "file_path": "old-notes.md" } }] } }));
    let path = temp_path("long-transcript.jsonl");
    std::fs::write(
        &path,
        transcript
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let output = run(
        Power::Prune,
        &json!({ "transcript_path": path }),
        &client(&model),
    )
    .await;
    assert!(output.chars().count() < 10_000);
    let (keep, drop) = output.split_once("Reduce").unwrap();
    assert!(keep.contains("step 99 ") && !keep.contains("step 0 "));
    assert!(drop.contains("old-notes"));
}
