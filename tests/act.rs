#![cfg(unix)]

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use common::{answer_always, reply, temp_path, yes};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHOP: &str = r#"<!doctype html><title>Shop</title>
<h1>Sign in</h1>
<label>Email <input id="email"></label>
<button id="go" type="button">BUTTON</button>
<p id="out"></p>
<script>go.onclick = () => { out.textContent = 'Welcome ' + email.value; };</script>"#;

const SECRET: &str = "sk-live  7f3a";

const PIXEL: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 252, 207, 192, 240, 31, 0,
    5, 5, 2, 0, 95, 200, 241, 210, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const SCRIPT: &str = "open PAGE
with email = qa@example.com
do sign in with the email
expect the page welcomes qa@example.com
see \"Welcome qa@example.com\"
";

fn choice(pick: &str) -> Value {
    json!({ "type": "choice", "choice": pick, "confidence": 0.9, "probabilities": { pick: 0.9, "none": 0.1 } })
}

fn decision(operation: &str, pick: &str, pick_reversed: &str) -> Value {
    json!({
        "operation": choice(operation),
        "holds": yes(0.95),
        "press": choice(pick),
        "press_reversed": choice(pick_reversed),
        "hover": choice(pick),
        "hover_reversed": choice(pick_reversed),
        "choose": choice(pick),
        "choose_reversed": choice(pick_reversed),
        "type": choice(pick),
        "type_reversed": choice(pick_reversed),
        "type_001": choice("001"),
        "type_001_reversed": choice("001"),
        "type_002": choice("001"),
        "type_002_reversed": choice("001"),
        "type_003": choice("001"),
        "type_003_reversed": choice("001"),
        "type_004": choice("001"),
        "type_004_reversed": choice("001"),
        "type_005": choice("001"),
        "type_005_reversed": choice("001"),
        "choose_001": choice("001"),
        "choose_001_reversed": choice("001"),
        "recorded": choice("001"),
        "recorded_reversed": choice("001"),
    })
}

async fn mock(replies: impl IntoIterator<Item = (&str, Value)>) -> MockServer {
    mock_after(Duration::ZERO, replies).await
}

async fn mock_after(
    delay: Duration,
    replies: impl IntoIterator<Item = (&str, Value)>,
) -> MockServer {
    let model = MockServer::start().await;
    for (state, answers) in replies {
        Mock::given(method("POST"))
            .and(body_string_contains(state))
            .respond_with(reply(answers).set_delay(delay))
            .mount(&model)
            .await;
    }
    model
}

async fn model() -> MockServer {
    mock([
        (
            "\"holds\"",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("Welcome", decision("done", "001", "001")),
        ("typed", decision("press", "002", "001")),
        ("", decision("type", "001", "001")),
    ])
    .await
}

fn script(name: &str, page: &str, steps: &str) -> std::path::PathBuf {
    let html = temp_path(&format!("{name}.html"));
    std::fs::write(&html, page).unwrap();
    let script = temp_path(&format!("{name}.e2e"));
    std::fs::write(
        &script,
        steps.replace("PAGE", &format!("file://{}", html.display())),
    )
    .unwrap();
    script
}

fn shop(name: &str) -> std::path::PathBuf {
    script(name, &SHOP.replace("BUTTON", "Sign in"), SCRIPT)
}

fn edit_page(script: &Path, from: &str, to: &str) {
    let page = script.with_extension("html");
    std::fs::write(
        &page,
        std::fs::read_to_string(&page).unwrap().replace(from, to),
    )
    .unwrap();
}

fn command(model: Option<&MockServer>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iu2u"));
    command
        .arg("act")
        .env_remove("IU2U_MODEL")
        .env_remove("CLAUDE_PLUGIN_OPTION_MODEL");
    if let Some(model) = model {
        command
            .env("IU2U_BASE_URL", model.uri())
            .env("IU2U_MODEL", "model-x");
    }
    command
}

fn act(script: &Path, model: Option<&MockServer>, locked: bool) -> (Output, Value) {
    let output = command(model)
        .arg("--json")
        .arg(script)
        .args(locked.then_some("--locked"))
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "not a report: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output, report)
}

fn act_with_browser(name: &str, browser: &str, flag: Option<&str>) -> (Output, Value) {
    use std::os::unix::fs::PermissionsExt;
    let path = temp_path(&format!("{name}-browser"));
    std::fs::write(&path, browser).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = script(name, "<!doctype html><p>Hi</p>", "open PAGE\nsee \"Hi\"\n");
    let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["act", "--json"])
        .args(flag)
        .arg(&script)
        .env("IU2U_BROWSER", &path)
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output, report)
}

fn vias(report: &Value) -> Vec<&str> {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["via"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn act_reaches_a_goal_records_it_and_replays_it_without_the_model() {
    let model = model().await;
    let script = shop("record");
    let (first, report) = act(&script, Some(&model), false);
    assert_eq!(first.status.code(), Some(0), "{report}");
    assert_eq!(vias(&report), ["browser", "model", "model", "check"]);
    assert_eq!(
        report["steps"][1]["actions"],
        json!([
            "typed \"qa@example.com\" into textbox \"Email\" · empty · in \"Sign in\"",
            "pressed button \"Sign in\" · in \"Sign in\""
        ])
    );
    assert_eq!(report["model_calls"], 8);
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["seconds"].is_f64()),
        "{report}"
    );
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(lock.contains("\"move\": \"press\""));

    let (second, replayed) = act(&script, None, true);
    assert_eq!(second.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["browser", "replay", "cache", "check"]);
    assert_eq!(replayed["model_calls"], 0);
    assert!(
        replayed["screen"]
            .as_str()
            .unwrap()
            .contains("Welcome qa@example.com")
    );
}

#[tokio::test]
async fn a_renamed_control_fails_a_locked_run_and_is_healed_by_the_model() {
    let model = model().await;
    let page = SHOP.replace("BUTTON", "Sign in").replace(
        "</script>",
        "setTimeout(() => { out.textContent = 'Expired'; }, 14000);</script>",
    );
    let script = script("heal", &page, SCRIPT);
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    edit_page(&script, ">Sign in</button>", ">Log in</button>");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("--locked")
    );

    let (healed, report) = act(&script, Some(&model), false);
    assert_eq!(healed.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "healed");
    assert_eq!(report["model_calls"], 3);
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(lock.contains("Log in") && !lock.contains("\"name\": \"Sign in\""));
}

#[tokio::test]
async fn an_unrecognized_control_is_decided_afresh() {
    let mut unrecognized = decision("press", "002", "001");
    unrecognized["recorded"] = choice("none");
    unrecognized["recorded_reversed"] = choice("none");
    let model = mock([
        (
            "\"holds\"",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("Welcome", decision("done", "001", "001")),
        ("typed", unrecognized),
        ("", decision("type", "001", "001")),
    ])
    .await;
    let script = shop("afresh");
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    edit_page(&script, ">Sign in</button>", ">Log in</button>");
    edit_page(&script, "<label>Email", "<label>E-mail");

    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "model");
    assert_eq!(
        report["steps"][1]["actions"][1],
        "pressed button \"Log in\" · in \"Sign in\""
    );
}

#[tokio::test]
async fn a_script_on_stdin_reports_a_broken_claim_and_exits_one() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.1), "sight": yes(0.0) })).await;
    let page = temp_path("claim.html");
    std::fs::write(&page, SHOP.replace("BUTTON", "Sign in")).unwrap();
    let mut child = command(Some(&model))
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        "open file://{}\nexpect the cart is empty\nsee \"never\"",
        page.display()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["script"], "-");
    assert_eq!(report["steps"].as_array().unwrap().len(), 2);
    assert_eq!(report["steps"][1]["status"], "failed");
    assert_eq!(
        report["steps"][1]["detail"],
        "the model says it does not hold: 10% sure it holds"
    );
}

fn long_claim(name: &str) -> std::path::PathBuf {
    let links: String = (0..300)
        .map(|index| format!("<a href=\"#{index}\">Menu entry {index}</a>"))
        .collect();
    let page = format!("<!doctype html><nav>{links}</nav><p>Cart total 5 dollars</p>");
    script(
        name,
        &page,
        "open PAGE\nexpect the cart total is 5 dollars\n",
    )
}

#[tokio::test]
async fn a_claim_about_a_long_screen_says_how_much_the_model_saw() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.1), "sight": yes(0.0) })).await;
    let (output, report) = act(&long_claim("long"), Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("the model saw only the first 6000 characters"),
        "{report}"
    );
}

#[test]
fn a_closed_output_ends_act_with_its_own_exit_code() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .arg("act")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"click the button\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_report_that_cannot_be_written_exits_two() {
    for (redirect, reported) in [("> \"$1\"", true), ("> \"$1\" 2>&1", false)] {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap '' XFSZ; ulimit -f 0; exec \"$0\" act {redirect}"
            ))
            .arg(env!("CARGO_BIN_EXE_iu2u"))
            .arg(temp_path("unwritten.json"))
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"click the button\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{said}");
        assert_eq!(said.starts_with("Error:"), reported, "{said}");
    }
}

#[test]
fn an_empty_secret_stops_the_script_before_it_runs() {
    let script = script(
        "empty",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nwith password = $IU2U_EMPTY_SECRET\ndo sign in\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["act", "--json"])
        .arg(&script)
        .env("IU2U_EMPTY_SECRET", "")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("set the environment variable IU2U_EMPTY_SECRET")
    );

    let silent = temp_path("empty-run.e2e");
    std::fs::write(&silent, "with token = run true\nrun echo never\n").unwrap();
    let (output, report) = act(&silent, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["error"], "with token: the command printed nothing");

    let failing = temp_path("failing-run.e2e");
    std::fs::write(
        &failing,
        "with password = $IU2U_TEST_SECRET\nwith token = run echo \"$password\" >&2; exit 1\nrun echo never\n",
    )
    .unwrap();
    let output = command(None)
        .arg("--json")
        .arg(&failing)
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(
        report["error"],
        "with token: the command ended with exit 1: (secret \"password\")"
    );

    let printed = temp_path("printed-run.e2e");
    std::fs::write(
        &printed,
        "with token = run printf 'tok-printed-31337\\n'; exit 1\nrun echo never\n",
    )
    .unwrap();
    let (output, report) = act(&printed, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["error"], "with token: the command ended with exit 1");
}

#[tokio::test]
async fn an_inconclusive_claim_fails_the_script_and_records_nothing() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.6), "sight": yes(0.0) })).await;
    let script = script(
        "inconclusive",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page greets the visitor\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["status"], "inconclusive");
    assert!(!Path::new(&format!("{}.lock", script.display())).exists());
}

#[tokio::test]
async fn a_claim_takes_the_values_it_names() {
    let model = mock([
        (
            "points to http://api.test/files/a.css",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        (
            "\"holds\"",
            json!({ "holds": yes(0.05), "sight": yes(0.0) }),
        ),
    ])
    .await;
    let script = script(
        "claim-values",
        "<!doctype html><a href=\"http://api.test/files/a.css\">a.css</a>",
        "with api = http://api.test\nwith key = $IU2U_TEST_SECRET\nopen PAGE\nexpect the link a.css points to $api/files/a.css and not to $key\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    for request in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&request.body).contains("7f3a"));
    }
}

#[tokio::test]
async fn a_long_output_shows_the_model_its_end() {
    let model = mock([
        (
            "SUMMARY all good",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        (
            "\"holds\"",
            json!({ "holds": yes(0.05), "sight": yes(0.0) }),
        ),
    ])
    .await;
    let script = script(
        "long-output",
        "",
        "run seq -f 'line %g' 1 2000; echo SUMMARY all good\nsee \"line 1\"\nexpect the summary says all is good\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][2]["detail"]
            .as_str()
            .unwrap()
            .ends_with("the model saw only the last 6000 characters of the screen"),
        "{report}"
    );
}

#[tokio::test]
async fn a_claim_about_a_changed_screen_is_asked_again() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "reclaim",
        "<!doctype html><p>Order shipped</p>",
        "open PAGE\nexpect the order is on its way\n",
    );
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    edit_page(&script, "shipped", "cancelled");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["via"], "cache");

    let (asked, report) = act(&script, Some(&model), false);
    assert_eq!(asked.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "model");
}

#[tokio::test]
async fn a_failing_claim_on_a_screen_that_keeps_changing_stops_within_its_time() {
    let holds = MockServer::start().await;
    answer_always(&holds, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "ticking-claim",
        "<!doctype html><p>Order shipped</p><p id=\"c\"></p><script>let n = 0; setTimeout(() => setInterval(() => c.textContent = 'Tick ' + ++n, 100), 800);</script>",
        "open PAGE\nwait 1s\nexpect the order is on its way within 2s\n",
    );
    assert_eq!(act(&script, Some(&holds), false).0.status.code(), Some(0));
    edit_page(&script, "shipped", "cancelled");
    let fails = MockServer::start().await;
    answer_always(&fails, json!({ "holds": yes(0.05), "sight": yes(0.0) })).await;
    let (output, report) = act(&script, Some(&fails), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][2]["seconds"].as_f64().unwrap() < 3.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_claim_about_a_header_is_asked_again_when_the_header_changes() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/job"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-state", "pending"))
        .up_to_n_times(3)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/job"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-state", "done"))
        .mount(&api)
        .await;
    let model = mock([
        (
            "x-state: done",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("", json!({ "holds": yes(0.05), "sight": yes(0.0) })),
    ])
    .await;
    let script = script(
        "header-claim",
        "",
        &format!(
            "request GET {}/job\nexpect the x-state header says done within 8s\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_claim_about_a_header_is_not_replayed_once_the_header_changes() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/job"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-state", "done"))
        .up_to_n_times(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/job"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-state", "failed"))
        .mount(&api)
        .await;
    let model = mock([
        (
            "x-state: done",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("", json!({ "holds": yes(0.05), "sight": yes(0.0) })),
    ])
    .await;
    let script = script(
        "header-replay",
        "",
        &format!(
            "request GET {}/job\nexpect the x-state header says done\n",
            api.uri()
        ),
    );
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["via"], "model", "{report}");
}

#[tokio::test]
async fn a_claim_that_holds_while_the_screen_changes_is_recorded_with_the_screen_that_lasts() {
    let model = mock_after(
        Duration::from_millis(1500),
        [(
            "\"holds\"",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        )],
    )
    .await;
    let script = script(
        "lasting",
        "<!doctype html><p id=\"s\">Order shipped, packing</p><script>setTimeout(() => s.textContent = 'Order shipped, packed', 800)</script>",
        "open PAGE\nexpect the order is on its way\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 2, "{report}");
    edit_page(&script, "shipped, packing", "shipped, packed");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_claim_on_a_screen_that_keeps_changing_asks_less_and_less_often() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.05), "sight": yes(0.0) })).await;
    let script = script(
        "ticking",
        "<!doctype html><p id=\"t\">0</p><script>setInterval(() => t.textContent = Date.now(), 300)</script>",
        "open PAGE\nexpect the build finished within 6s\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    let calls = report["model_calls"].as_u64().unwrap();
    assert!((3..=5).contains(&calls), "{report}");
}

#[tokio::test]
async fn a_claim_that_comes_true_while_asks_are_spaced_out_is_still_judged() {
    let model = mock([
        ("Done", json!({ "holds": yes(0.95), "sight": yes(0.0) })),
        ("", json!({ "holds": yes(0.05), "sight": yes(0.0) })),
    ])
    .await;
    let script = script(
        "late-done",
        "<!doctype html><p id=\"t\">Building 0</p><script>let n = 0; const tick = setInterval(() => t.textContent = 'Building ' + ++n, 300); setTimeout(() => { clearInterval(tick); t.textContent = 'Done'; }, 8300)</script>",
        "open PAGE\nexpect the build finished within 9s\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_claim_keeps_its_lock_when_only_its_time_to_wait_changes() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "within-key",
        "<!doctype html><p>Order confirmed</p>",
        "open PAGE\nexpect the order is confirmed within 1m\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let longer = std::fs::read_to_string(&script)
        .unwrap()
        .replace("within 1m", "within 2m");
    std::fs::write(&script, longer).unwrap();

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn the_time_a_claim_may_wait_is_not_a_number_it_checks() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "within-number",
        "<!doctype html><p>Order confirmed</p><p>Updated 5 minutes ago</p>",
        "open PAGE\nexpect the order is confirmed within 5s\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    edit_page(&script, "Updated 5", "Updated 6");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_locked_claim_holds_a_named_value_to_its_new_value() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "named-value",
        "<!doctype html><p>Prototype 4711 created</p>",
        "with id = 4711\nopen PAGE\nexpect the page says prototype $id was created\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let next = std::fs::read_to_string(&script)
        .unwrap()
        .replace("with id = 4711", "with id = 4712");
    std::fs::write(&script, next).unwrap();
    edit_page(&script, "4711", "4710");

    let (wrong, report) = act(&script, None, true);
    assert_eq!(wrong.status.code(), Some(1), "{report}");
    edit_page(&script, "4710", "4712");
    let (right, report) = act(&script, None, true);
    assert_eq!(right.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_claim_about_a_message_that_goes_is_recorded_with_the_screen_that_showed_it() {
    let model = mock_after(
        Duration::from_secs(6),
        [
            ("Saved", json!({ "holds": yes(0.95), "sight": yes(0.0) })),
            (
                "\"holds\"",
                json!({ "holds": yes(0.05), "sight": yes(0.0) }),
            ),
        ],
    )
    .await;
    let script = script(
        "saved-message",
        "<!doctype html><p id=\"t\" role=\"status\">Saved</p><script>setTimeout(() => t.textContent = '', 5000)</script>",
        "open PAGE\nexpect a message says it was saved\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 2, "{report}");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_lock_that_cannot_be_written_keeps_the_report() {
    use std::os::unix::fs::PermissionsExt;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let folder = temp_path("read-only");
    std::fs::create_dir_all(&folder).unwrap();
    let page = folder.join("page.html");
    std::fs::write(&page, "<!doctype html><p>Hello</p>").unwrap();
    let script = folder.join("hello.e2e");
    std::fs::write(
        &script,
        format!(
            "open file://{}\nexpect the page says hello\n",
            page.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (output, report) = act(&script, Some(&model), false);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["steps"][1]["status"], "passed", "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("cannot write"),
        "{report}"
    );
}

#[tokio::test]
async fn a_request_that_never_ends_does_not_hold_up_the_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    let page = format!(
        "<!doctype html><p id=\"out\">Hello</p><script>fetch('{}/poll'); setTimeout(() => {{ out.textContent = 'Too late'; }}, 7000);</script>",
        server.uri()
    );
    let script = script("poll", &page, "open PAGE\nsee \"Hello\"\n");
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn a_browser_that_fails_to_start_says_why() {
    let (output, report) = act_with_browser(
        "nobrowser",
        "#!/bin/sh\necho 'error while loading shared libraries: libnss3.so' >&2\nexit 127\n",
        None,
    );
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("libnss3.so"),
        "{report}"
    );
}

#[test]
fn the_browser_runs_headless_unless_headful_is_passed() {
    for (flag, mode) in [(None, "mode=headless"), (Some("--headful"), "mode=window")] {
        let (_, report) = act_with_browser(
            "mode",
            "#!/bin/sh\ncase \" $* \" in *' --headless '*) echo mode=headless >&2 ;; *) echo mode=window >&2 ;; esac\nexit 1\n",
            flag,
        );
        assert!(report["error"].as_str().unwrap().contains(mode), "{report}");
    }
}

#[test]
fn a_script_error_exits_two() {
    let script = temp_path("broken.e2e");
    std::fs::write(&script, "click the button\n").unwrap();
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2));
    assert!(report["error"].as_str().unwrap().contains("line 1"));

    let (missing, report) = act(&temp_path("missing.e2e"), None, false);
    assert_eq!(missing.status.code(), Some(2));
    assert!(report["error"].as_str().unwrap().contains("cannot read"));

    let vision = command(None)
        .arg(&script)
        .env("IU2U_VISION", "yes")
        .output()
        .unwrap();
    assert_eq!(vision.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&vision.stderr).contains("IU2U_VISION must be true or false"));

    let empty = temp_path("comment-only.e2e");
    std::fs::write(&empty, "# only a comment\nwith key = value\n").unwrap();
    let (output, report) = act(&empty, None, false);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report["error"], "the script has no steps");
}

#[tokio::test]
async fn a_changed_result_fails_a_locked_run_and_a_failed_run_keeps_the_lock() {
    let model = model().await;
    let script = shop("changed");
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    edit_page(&script, "'Welcome '", "'Declined '");
    let lock = format!("{}.lock", script.display());
    let recorded = std::fs::read_to_string(&lock).unwrap();

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("--locked")
    );

    let stuck = mock([("", decision("stuck", "001", "001"))]).await;
    let (failed, report) = act(&script, Some(&stuck), false);
    assert_eq!(failed.status.code(), Some(1), "{report}");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), recorded);
}

#[tokio::test]
async fn a_renamed_value_is_named_instead_of_blaming_the_screen() {
    let model = model().await;
    let script = shop("rename");
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    let text = std::fs::read_to_string(&script).unwrap();
    std::fs::write(&script, text.replace("with email =", "with login =")).unwrap();

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("\"email\", which the script no longer has"),
        "{report}"
    );
}

#[tokio::test]
async fn a_secret_reaches_neither_the_model_nor_the_report() {
    let model = model().await;
    let script = script(
        "secret",
        &SHOP.replace("BUTTON", &format!("Sign in as {SECRET}")),
        "open PAGE\nwith email = $IU2U_TEST_SECRET\ndo sign in with the email\nsee \"Welcome\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let mut seen = String::from_utf8_lossy(&output.stdout).into_owned();
    seen.push_str(&std::fs::read_to_string(format!("{}.lock", script.display())).unwrap());
    for request in model.received_requests().await.unwrap() {
        seen.push_str(&String::from_utf8_lossy(&request.body));
    }
    assert!(
        seen.contains("Welcome (secret") && !seen.contains("7f3a"),
        "{seen}"
    );
}

#[test]
fn a_secret_stays_out_of_failed_steps_and_errors() {
    let failing = script(
        "secret-see",
        "<!doctype html><p>Hello tok-5f3a9c1e7b</p>",
        "with token = run printf 'tok-%s' 5f3a9c1e7b\nopen PAGE\nrun echo ok\nsee \"Signed in as $token\" within 0s\n",
    );
    let (output, seen) = act(&failing, None, false);
    assert_eq!(output.status.code(), Some(1), "{seen}");
    assert!(seen["screenshot"].is_null(), "{seen}");
    let erroring = script(
        "secret-request",
        "",
        "with token = run printf 'tok-%s' 5f3a9c1e7b\nrequest GET http://127.0.0.1:9/items?key=$token\n",
    );
    let (output, sent) = act(&erroring, None, false);
    assert_eq!(output.status.code(), Some(2), "{sent}");
    for report in [seen, sent] {
        assert!(!report.to_string().contains("5f3a9c1e7b"), "{report}");
    }
}

#[tokio::test]
async fn a_secret_read_for_a_request_is_never_offered_for_typing() {
    let site = site(&[
        ("/api", "ok"),
        (
            "/app",
            "<!doctype html><input aria-label=\"Search\"><button>Go</button>",
        ),
    ])
    .await;
    let model = mock([("", decision("type", "001", "001"))]).await;
    let script = script(
        "env-secret",
        "",
        &format!(
            "request GET {uri}/api\n  authorization: Bearer $IU2U_TEST_SECRET\nopen {uri}/app\ndo search for cats\n",
            uri = site.uri()
        ),
    );
    act(&script, Some(&model), false);
    let asked = model.received_requests().await.unwrap();
    assert!(!asked.is_empty());
    for request in asked {
        assert!(!String::from_utf8_lossy(&request.body).contains("IU2U_TEST_SECRET"));
    }
}

#[test]
fn see_finds_a_secret_where_the_screen_hides_it() {
    let script = script(
        "see-secret",
        &format!("<!doctype html><p>Key {SECRET}</p>"),
        "with key = $IU2U_TEST_SECRET\nopen PAGE\nsee \"$key\"\nsee no \"$key\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["status"], "passed", "{report}");
    assert_eq!(report["steps"][2]["status"], "failed", "{report}");
}

#[tokio::test]
async fn typing_into_a_field_that_cannot_take_focus_types_nowhere() {
    let model = mock([("", decision("type", "002", "001"))]).await;
    let page = r#"<!doctype html><label>Email <input id="email"></label><div role="textbox" aria-label="Password"></div><script>email.focus();</script>"#;
    let script = script(
        "unfocused",
        page,
        "open PAGE\nwith password = hunter2\ndo type the password\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .contains("cannot take focus"),
        "{report}"
    );
    assert!(
        !report["screen"].as_str().unwrap().contains("hunter2"),
        "{report}"
    );
}

#[tokio::test]
async fn act_types_into_a_rich_text_editor() {
    let model = mock([
        ("typed", decision("done", "001", "001")),
        ("", decision("type", "001", "002")),
    ])
    .await;
    let page = r#"<!doctype html><div contenteditable aria-label="Note"></div><label>Quantity <input type="number" value="1"></label>"#;
    let script = script(
        "rich",
        page,
        "open PAGE\ndo write \"hi there\" in the note\nsee \"hi there\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .starts_with("typed \"hi there\" into textbox \"Note\""),
        "{report}"
    );
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("spinbutton \"Quantity\" = \"1\""),
        "{report}"
    );
}

#[tokio::test]
async fn a_page_that_opens_a_new_tab_keeps_running_in_front() {
    let model = mock([
        ("Counting", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><button onclick="const other = window.open(''); let ticks = 0; out.textContent = 'Counting'; const timer = setInterval(() => { if (++ticks === 150) { clearInterval(timer); out.textContent = 'Finished while ' + document.visibilityState + ' and the other tab is ' + other.document.visibilityState + ', blurred ' + blurs + ' times'; } }, 10);">Help</button><p id="out"></p><script>let blurs = 0; onblur = () => blurs++;</script>"#;
    let script = script(
        "newtab",
        page,
        "open PAGE\ndo open the help\nsee \"Finished while visible and the other tab is hidden, blurred 0 times\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_press_blocked_for_a_moment_is_tried_again_after_a_wait() {
    let model = MockServer::start().await;
    for (state, answers, times) in [
        ("Saved", decision("done", "001", "001"), None),
        ("could not", decision("wait", "001", "001"), Some(1)),
        ("", decision("press", "001", "001"), None),
    ] {
        let mock = Mock::given(method("POST"))
            .and(body_string_contains(state))
            .respond_with(reply(answers).set_delay(Duration::from_millis(400)));
        match times {
            Some(times) => mock.up_to_n_times(times).mount(&model).await,
            None => mock.mount(&model).await,
        }
    }
    let page = r#"<!doctype html><button onclick="out.textContent = 'Saved'">Save</button><div id="cover" style="position: fixed; inset: 0"></div><p id="out"></p><script>setTimeout(() => cover.remove(), 1500);</script>"#;
    let script = script("covered", page, "open PAGE\ndo save\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["actions"][1], "waited", "{report}");

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert!(
        replayed["screen"].as_str().unwrap().contains("Saved"),
        "{replayed}"
    );
}

#[tokio::test]
async fn a_replay_waits_for_a_control_that_something_covers_for_a_moment() {
    let model = mock([
        ("Saved", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><button onclick="out.textContent = 'Saved'">Save</button><p id="out"></p><!--cover-->"#;
    let script = script("uncovered", page, "open PAGE\ndo save\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    edit_page(
        &script,
        "<!--cover-->",
        r#"<div id="cover" style="position: fixed; inset: 0"></div><script>setTimeout(() => cover.remove(), 1500);</script>"#,
    );

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert!(
        replayed["screen"].as_str().unwrap().contains("Saved"),
        "{replayed}"
    );
}

#[tokio::test]
async fn act_types_into_the_parts_of_a_date_field() {
    let model = mock([("typed", decision("done", "001", "001"))]).await;
    Mock::given(method("POST"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = request.body_json().unwrap();
            let day = |question: &str| {
                body["questions"][question]["criteria"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .find(|(_, target)| {
                        target
                            .as_str()
                            .unwrap()
                            .starts_with("spinbutton \"Day Day\"")
                    })
                    .map_or("001".into(), |(label, _)| label.clone())
            };
            reply(decision("type", &day("type"), &day("type_reversed")))
        })
        .mount(&model)
        .await;
    let script = script(
        "date",
        r#"<!doctype html><label>Birth <input type="date"></label>"#,
        "open PAGE\ndo set the birth day to \"15\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("spinbutton \"Day Day\" = \"15\""),
        "{report}"
    );
}

#[tokio::test]
async fn a_secret_shown_in_pieces_stops_the_step_before_it_is_sent() {
    let model = mock([("", decision("done", "001", "001"))]).await;
    let script = script(
        "pieces",
        "<!doctype html><p><code>sk-live</code> <code>7f3a</code></p>",
        "open PAGE\nwith token = $IU2U_TEST_SECRET\nexpect the token is shown\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("7f3a"),
        "{report}"
    );
    assert!(model.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_field_that_reformats_what_is_typed_takes_it_once() {
    let model = mock([
        ("typed", decision("done", "001", "001")),
        ("", decision("type", "001", "001")),
    ])
    .await;
    let script = script(
        "coupon",
        r#"<!doctype html><label>Coupon <input oninput="this.value = this.value.toUpperCase()"></label>"#,
        "open PAGE\ndo apply the coupon \"save10\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    assert!(
        screen.contains("textbox \"Coupon\" = \"SAVE10\"") && screen.matches("SAVE10").count() == 1,
        "{report}"
    );
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(lock.contains("\"move\": \"type\""), "{lock}");
}

async fn press_and_see(name: &str, page: &str, text: &str) {
    let model = mock([
        ("pressed", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        name,
        page,
        &format!("open PAGE\ndo press the button\nsee {text:?}\n"),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn act_presses_a_button_inside_a_web_component() {
    press_and_see(
        "shadow",
        r#"<!doctype html><x-pay></x-pay><p id="out"></p><script>customElements.define('x-pay', class extends HTMLElement { connectedCallback() { const button = document.createElement('button'); button.textContent = 'Pay'; button.onclick = () => { out.textContent = 'Paid'; }; this.attachShadow({ mode: 'open' }).append(button); } });</script>"#,
        "Paid",
    )
    .await;
}

#[tokio::test]
async fn a_press_does_not_hold_the_mouse_button_while_moving() {
    press_and_see(
        "hover",
        r#"<!doctype html><button onmousemove="if (event.buttons) { document.title = 'dragged'; }" onclick="out.textContent = document.title === 'dragged' ? 'Dragged' : 'Clicked';">Go</button><p id="out"></p>"#,
        "Clicked",
    )
    .await;
}

#[tokio::test]
async fn a_dismissed_alert_stays_on_the_screen_until_the_next_action() {
    let model = mock([
        ("pressed", decision("done", "001", "001")),
        ("Goal: close", decision("press", "002", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "alert",
        r#"<!doctype html><button onclick="alert('Saved')">Save</button><button onclick="out.textContent = 'Closed'">Close</button><p id="out"></p>"#,
        "open PAGE\ndo save\nsee \"Saved (dismissed)\"\ndo close\nsee \"Closed\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        !report["screen"].as_str().unwrap().contains("Saved"),
        "{report}"
    );
}

#[tokio::test]
async fn an_alert_raised_while_the_model_decides_is_still_shown() {
    let model = mock_after(
        Duration::from_millis(2500),
        [
            ("pressed", decision("done", "001", "001")),
            ("", decision("press", "001", "001")),
        ],
    )
    .await;
    let script = script(
        "late-alert",
        r#"<!doctype html><button onclick="out.textContent = 'Pressed'">Go</button><p id="out"></p><script>setTimeout(() => alert('Card expired'), 2000);</script>"#,
        "open PAGE\ndo press the button\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let asked = model.received_requests().await.unwrap();
    assert!(
        asked.iter().any(|request| {
            String::from_utf8_lossy(&request.body).contains("Card expired (dismissed)")
        }),
        "{report}"
    );
}

#[tokio::test]
async fn a_confirm_dialog_is_accepted() {
    press_and_see(
        "confirm",
        r#"<!doctype html><button onclick="if (confirm('Delete?')) { out.textContent = 'Deleted'; }">Delete</button><p id="out"></p>"#,
        "Deleted",
    )
    .await;
}

fn todos(items: &[&str]) -> String {
    let rows: String = items
        .iter()
        .map(|item| {
            format!(r#"<li>{item} <button onclick="this.parentNode.remove()">Delete</button></li>"#)
        })
        .collect();
    format!("<!doctype html><h1>Todos</h1><ul>{rows}</ul>")
}

#[tokio::test]
async fn a_reordered_list_is_not_replayed_by_position() {
    let model = mock([
        ("pressed", decision("done", "001", "001")),
        ("", decision("press", "001", "003")),
    ])
    .await;
    let script = script(
        "reorder",
        &todos(&["Milk", "Eggs", "Bread"]),
        "open PAGE\ndo delete the milk\n",
    );
    assert_eq!(act(&script, Some(&model), false).0.status.code(), Some(0));
    std::fs::write(
        script.with_extension("html"),
        todos(&["Eggs", "Milk", "Bread"]),
    )
    .unwrap();

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{replayed}");
    let screen = replayed["screen"].as_str().unwrap();
    assert!(
        ["Eggs", "Milk", "Bread"]
            .iter()
            .all(|item| screen.contains(item)),
        "{screen}"
    );
}

#[test]
fn pages_are_asked_to_reduce_motion() {
    let script = script(
        "motion",
        "<!doctype html><p id=\"m\"></p><script>m.textContent = matchMedia('(prefers-reduced-motion: reduce)').matches ? 'Motion reduced' : 'Motion on';</script>",
        "open PAGE\nsee \"Motion reduced\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

async fn site(pages: &[(&str, &str)]) -> MockServer {
    let site = MockServer::start().await;
    for (at, body) in pages {
        Mock::given(method("GET"))
            .and(path(*at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(*body, "text/html"))
            .mount(&site)
            .await;
    }
    site
}

#[tokio::test]
async fn a_request_shows_its_response_to_the_steps_after_it_and_hides_secrets() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/session"))
        .and(header("authorization", format!("Bearer {SECRET}").as_str()))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "error": "Sign-in required", "echo": SECRET })),
        )
        .mount(&api)
        .await;
    let script = script(
        "request",
        "",
        &format!(
            "request GET {}/api/session\n  authorization: Bearer $IU2U_TEST_SECRET\nsee \"HTTP 401 Unauthorized\"\nsee error: \"Sign-in required\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][0]["via"], "http");
    assert_eq!(report["steps"][0]["detail"], "HTTP 401 Unauthorized");
    let screen = report["screen"].as_str().unwrap();
    assert!(
        !screen.contains(SECRET) && screen.contains("secret \\\"IU2U_TEST_SECRET"),
        "{report}"
    );
}

#[tokio::test]
async fn a_wait_after_the_checks_of_a_response_looks_at_the_page_again() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/grants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "grants": 2 })))
        .mount(&api)
        .await;
    let script = script(
        "back-to-page",
        "<!doctype html><p>Still on the form</p>",
        &format!(
            "open PAGE\nrequest GET {}/grants\nsee grants: 2\nwait 100ms\nsee \"Still on the form\" within 0s\nsee no \"grants: 2\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_value_with_quotes_keeps_a_json_body_valid() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/notes"))
        .and(body_json(json!({ "title": "a\\b \"c\"" })))
        .respond_with(ResponseTemplate::new(201))
        .mount(&api)
        .await;
    let script = script(
        "json-quotes",
        "",
        &format!(
            "with title = a\\b \"c\"\nrequest POST {}/notes\n  json {{\"title\": \"$title\"}}\nsee \"HTTP 201\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn requests_send_json_and_forms_and_show_redirects_without_following_them() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/merge"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({ "from": "a", "to": "b" })))
        .respond_with(ResponseTemplate::new(409).set_body_string("already merged"))
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .and(body_string_contains("name=\"name\""))
        .and(body_string_contains("Campus 2026"))
        .and(body_string_contains("filename=\"notes.txt\""))
        .and(body_string_contains("text/plain"))
        .and(body_string_contains("hello file"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/done"))
        .mount(&api)
        .await;
    let file = temp_path("notes.txt");
    std::fs::write(&file, "hello file").unwrap();
    let merge = temp_path("merge.json");
    let script = script(
        "forms",
        "",
        &format!(
            "request POST {uri}/merge\n  json {{\"from\": \"a\", \"to\": \"b\"}}\nsee \"HTTP 409 Conflict\"\nsee \"already merged\"\nrun printf '{{\"from\": \"a\",\\n \"to\": \"b\"}}' > {merge}\nrequest POST {uri}/merge\n  json file {merge}\nsee \"HTTP 409 Conflict\"\nrequest POST {uri}/upload\n  form name = \"Campus 2026\"\n  form files = file {file}\nsee \"HTTP 302 Found\"\nsee \"location: /done\"\n",
            uri = api.uri(),
            file = file.display(),
            merge = merge.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let asked: Vec<String> = api
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_string())
        .collect();
    assert_eq!(asked, ["/merge", "/merge", "/upload"]);
}

#[tokio::test]
async fn a_long_text_body_cut_inside_a_character_still_shows_as_text() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/long"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(format!("a{}", "ñ".repeat(600_000))),
        )
        .mount(&api)
        .await;
    let script = script(
        "long-body",
        "",
        &format!(
            "request GET {}/long\nsee no \"binary body\" within 0s\nsee \"the body continues\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_form_without_files_is_sent_url_encoded() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("scope=read+write"))
        .respond_with(ResponseTemplate::new(200).set_body_string("token issued"))
        .mount(&api)
        .await;
    let script = script(
        "urlencoded",
        "",
        &format!(
            "request POST {}/token\n  form grant_type = client_credentials\n  form scope = read write\nsee \"token issued\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn a_form_with_a_file_refuses_a_content_type_of_its_own() {
    let file = temp_path("typed-upload.txt");
    std::fs::write(&file, "hello").unwrap();
    let script = script(
        "typed-upload",
        "",
        &format!(
            "request POST http://127.0.0.1:9/upload\n  content-type: multipart/form-data\n  form files = file {}\n",
            file.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"].as_str().unwrap().ends_with(
            "a form with a file sets its own content-type; leave out the content-type header"
        ),
        "{report}"
    );
}

#[tokio::test]
async fn a_json_file_is_sent_as_it_is() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/schema"))
        .and(body_json(
            json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "name": "$name" }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .mount(&api)
        .await;
    let body = temp_path("schema.json");
    std::fs::write(
        &body,
        r#"{"$schema": "https://json-schema.org/draft/2020-12/schema", "name": "$name"}"#,
    )
    .unwrap();
    let script = script(
        "as-is",
        "",
        &format!(
            "with name = Ada\nrequest POST {}/schema\n  json file {}\nsee \"HTTP 204\"\n",
            api.uri(),
            body.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_relative_request_goes_to_the_open_page_and_needs_one() {
    let site = site(&[
        ("/", "<!doctype html><p>Home</p>"),
        ("/api/items", "[1,2,3]"),
    ])
    .await;
    let script = script(
        "relative",
        "",
        &format!(
            "open {}/\nsee \"Home\"\nrequest GET /api/items\nsee \"[2]: 3\"\nsee no \"[3]\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let alone = self::script("alone", "", "request GET /api/items\n");
    let (output, report) = act(&alone, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("is relative"),
        "{report}"
    );
}

#[tokio::test]
async fn routes_send_a_site_elsewhere_answer_in_its_place_and_answer_once() {
    let site = site(&[
        ("/login", "<!doctype html><h1>Fake login</h1>"),
        ("/api/once", "fine"),
        (
            "/app",
            "<!doctype html><p id=\"a\">Loading</p><p id=\"c\">Checking</p><script>fetch('/api/items').then(r => r.text().then(t => a.textContent = 'Items ' + r.status + ' ' + t)); (async () => { const one = await fetch('/api/once'); const two = await fetch('/api/once'); c.textContent = 'Once ' + one.status + ' then ' + two.status; })();</script>",
        ),
    ])
    .await;
    let script = script(
        "routes",
        "",
        &format!(
            "route https://login.example.test to {uri}\nopen https://login.example.test/login\nsee \"Fake login\"\nroute */api/items answer 500 {{\"error\":\"boom\"}}\nroute */api/once once answer 404\nopen {uri}/app\nsee \"Items 500 {{\"error\":\"boom\"}}\"\nsee \"Once 404 then 200\"\nsee \"*/api/once 1 request\"\nroute */api/items answer 200 fixed\nreload\nsee \"Items 200 fixed\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["screen"]
            .as_str()
            .unwrap()
            .matches("- route \"*/api/items\" = \"1 request\"")
            .count(),
        1,
        "{report}"
    );
}

#[test]
fn a_route_to_another_address_refuses_a_star_even_from_a_value() {
    let script = script(
        "star-route",
        "",
        "with sso = http://127.0.0.1:9/d*/p.html\nroute $sso to http://127.0.0.1:9\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("without *"),
        "{report}"
    );
}

#[test]
fn a_route_answer_that_is_html_shows_as_a_page() {
    let script = script(
        "fake-page",
        "",
        "route http://down.example.test answer 503 <!doctype html><title>Down</title><h1>Back soon</h1>\nopen http://down.example.test/status\nsee \"Back soon\"\nsee no \"<h1>\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("heading \"Back soon\""),
        "{report}"
    );
}

#[tokio::test]
async fn a_route_answers_a_page_from_another_origin() {
    let api = MockServer::start().await;
    let site = site(&[(
        "/app",
        &format!(
            "<!doctype html><p id=\"get\">Loading</p><p id=\"post\">Loading</p><script>fetch('{api}/items').then(r => r.text()).then(t => get.textContent = 'Got ' + t, e => get.textContent = 'Get ' + e.message); fetch('{api}/items', {{ method: 'POST', headers: {{ 'content-type': 'application/json' }}, body: '{{}}' }}).then(r => r.text()).then(t => post.textContent = 'Posted ' + t, e => post.textContent = 'Post ' + e.message);</script>",
            api = api.uri()
        ),
    )])
    .await;
    let script = script(
        "cors",
        "",
        &format!(
            "route {}/items answer 200 faked\nopen {}/app\nsee \"Got faked\"\nsee \"Posted faked\"\n",
            api.uri(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("/items\" = \"2 requests\""),
        "{report}"
    );
}

#[tokio::test]
async fn a_route_sees_requests_the_browser_cache_would_answer() {
    let site = site(&[
        (
            "/first",
            "<!doctype html><img alt=\"Field\" src=\"/field.png\">",
        ),
        (
            "/second",
            "<!doctype html><img alt=\"Field\" src=\"/field.png\">",
        ),
    ])
    .await;
    Mock::given(method("GET"))
        .and(path("/field.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("cache-control", "public, max-age=31536000, immutable")
                .set_body_raw(PIXEL, "image/png"),
        )
        .mount(&site)
        .await;
    let script = script(
        "cached",
        "",
        &format!(
            "open {uri}/first\nroute */field.png answer 404\nopen {uri}/second\nsee \"*/field.png 1 request\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_held_request_the_page_aborts_is_no_longer_held() {
    let site = site(&[(
        "/app",
        "<!doctype html><p id=\"out\">Loading</p><script>const stop = new AbortController(); fetch('/api/slow', { signal: stop.signal }).catch(() => out.textContent = 'Gave up'); setTimeout(() => stop.abort(), 1000);</script>",
    )])
    .await;
    let script = script(
        "held-aborted",
        "",
        &format!(
            "route */api/slow hold\nopen {}/app\nsee \"1 held until release\"\nsee \"Gave up\" within 5s\nsee no \"held until release\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_held_request_goes_with_the_page_that_sent_it() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><p>Page A</p><script>fetch('/api/slow');</script>",
        ),
        ("/other", "<!doctype html><p>Page B</p>"),
    ])
    .await;
    let script = script(
        "held-gone",
        "",
        &format!(
            "route */api/slow hold\nopen {uri}/app\nsee \"1 held until release\"\nopen {uri}/other\nsee \"Page B\"\nsee no \"held until release\" within 0s\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_held_request_waits_for_its_release_without_holding_up_the_steps() {
    let site = site(&[(
        "/app",
        "<!doctype html><p id=\"b\">Loading slow</p><script>fetch('/api/slow').then(() => b.textContent = 'Slow done');</script>",
    )])
    .await;
    Mock::given(method("GET"))
        .and(path("/api/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(1)))
        .mount(&site)
        .await;
    let script = script(
        "hold",
        "",
        &format!(
            "route */api/slow hold\nopen {}/app\nsee \"Loading slow\"\nsee \"1 held until release\"\nrelease */api/slow\nsee \"Slow done\"\nsee no \"held until release\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 1.5,
        "{report}"
    );
    assert_eq!(report["steps"][4]["detail"], "let 1 held request go");
    assert!(
        report["steps"][4]["seconds"].as_f64().unwrap() >= 0.9,
        "{report}"
    );
    let unheld = self::script("unheld", "", "release */api/nothing\n");
    let (output, report) = act(&unheld, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"].as_str().unwrap().contains("no route holds"),
        "{report}"
    );
}

#[tokio::test]
async fn a_held_picture_does_not_hold_up_the_steps() {
    let site = site(&[(
        "/app",
        "<!doctype html><p>Loading picture</p><img src=\"/slow.png\" width=\"50\" height=\"50\">",
    )])
    .await;
    let script = script(
        "held-picture",
        "",
        &format!(
            "route */slow.png hold\nopen {}/app\nsee \"Picture done\" within 1s\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 2.0,
        "{report}"
    );
    assert!(report["seconds"].as_f64().unwrap() < 5.0, "{report}");
    assert!(report["screenshot"].is_string(), "{report}");
}

#[tokio::test]
async fn back_forward_and_reload_walk_the_history() {
    let site = site(&[
        ("/one", "<!doctype html><title>One</title><p>First page</p>"),
        (
            "/two",
            "<!doctype html><title>Two</title><p>Second page</p>",
        ),
    ])
    .await;
    let script = script(
        "history",
        "",
        &format!(
            "open {uri}/one\nopen {uri}/two\nback\nsee \"First page\"\nsee \"{uri}/one\"\nforward\nsee \"Second page\"\nreload\nsee \"Second page\"\nforward\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["steps"].as_array().unwrap().len(), 10, "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("no page to go forward to"),
        "{report}"
    );
}

#[tokio::test]
async fn the_screen_names_the_page_address_link_targets_tooltips_and_busy_parts() {
    let site = site(&[(
        "/app",
        "<!doctype html><title>App</title><a href=\"/next\">Next</a><button title=\"Shows the grid\" aria-pressed=\"true\">Grid</button><ul aria-busy=\"true\" aria-label=\"Results\"><li>One</li></ul><div aria-busy=\"true\"><p>Loading cards</p></div>",
    )])
    .await;
    let script = script(
        "enriched",
        "",
        &format!("open {}/app\nsee \"Shows the grid\"\n", site.uri()),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    for expected in [
        format!("page \"App\" · focused · url \"{}/app\"", site.uri()),
        format!("link \"Next\" · url \"{}/next\"", site.uri()),
        "button \"Grid\" · pressed · description \"Shows the grid\"".into(),
        "list \"Results\" · busy".into(),
        "generic \"\" · busy\n    - text \"Loading cards\"".into(),
    ] {
        assert!(screen.contains(&expected), "{expected:?} not in:\n{screen}");
    }
}

#[tokio::test]
async fn every_tab_has_the_same_page_size() {
    let size = "<script>document.write(`<p>Size ${innerWidth}x${innerHeight}</p>`)</script>";
    let site = site(&[
        (
            "/app",
            &format!("<!doctype html><a href=\"/help\" target=\"_blank\">Help</a>{size}"),
        ),
        (
            "/help",
            &format!("<!doctype html><title>Help</title><p>Help center</p>{size}"),
        ),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "page-size",
        "",
        &format!(
            "open {uri}/app\nsee \"Size 1280x900\"\ndo open the help\nsee \"$hub/help\"\nsee \"Size 1280x900\"\n",
            uri = site.uri()
        )
        .replace("$hub", &site.uri()),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_followed_tab_shows_only_its_own_failed_requests() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><img src=\"http://127.0.0.1:9/logo.png\"><a href=\"/help\" target=\"_blank\">Help</a><script>setInterval(() => fetch('http://127.0.0.1:9/poll').catch(() => {}), 200);</script>",
        ),
        (
            "/help",
            "<!doctype html><title>Help</title><p>Help center</p>",
        ),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "own-failures",
        "",
        &format!(
            "open {uri}/app\nsee \"failed: http://127.0.0.1:9/logo.png\"\ndo open the help\nsee \"Help center\"\nwait 1s\nsee no \"failed:\" within 0s\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_link_that_opens_a_new_tab_is_followed() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><a href=\"/help\" target=\"_blank\">Help</a>",
        ),
        (
            "/help",
            "<!doctype html><title>Help</title><p>Help center</p>",
        ),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "follow",
        "",
        &format!(
            "open {uri}/app\ndo open the help\nsee \"Help center\"\nsee \"{uri}/help\"\nback\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["steps"][3]["status"], "passed", "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("no page to go back to"),
        "{report}"
    );
}

#[tokio::test]
async fn a_frame_inside_a_frame_from_another_site_shows_its_own_clickable_parts() {
    let site = MockServer::start().await;
    let port = site.address().port();
    for (at, body) in [
        (
            "/app",
            format!("<!doctype html><iframe style=\"width: 100%; height: 300px; border: 0\" src=\"http://localhost:{port}/outer\"></iframe>"),
        ),
        (
            "/outer",
            "<!doctype html><iframe style=\"width: 100%; height: 200px; border: 0\" src=\"/inner\"></iframe>".into(),
        ),
        (
            "/inner",
            "<!doctype html><div onclick=\"this.textContent = 'Opened'\">Open card</div>".into(),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
            .mount(&site)
            .await;
    }
    let script = script(
        "nested-clickable",
        "",
        &format!("open http://127.0.0.1:{port}/app\nsee \"Open card\"\n"),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("\"Open card\" · clickable"),
        "{report}"
    );
}

#[tokio::test]
async fn a_file_chooser_inside_a_frame_from_another_site_gets_the_named_file() {
    let note = temp_path("cross-note.txt");
    std::fs::write(&note, "hello").unwrap();
    let site = MockServer::start().await;
    let port = site.address().port();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("<!doctype html><iframe style=\"width: 100%; height: 300px; border: 0\" src=\"http://localhost:{port}/frame\"></iframe>"),
            "text/html",
        ))
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/frame"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<!doctype html><button onclick=\"fi.click()\">Upload</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name).join(', ')\"><p id=\"out\">Nothing yet</p>",
            "text/html",
        ))
        .mount(&site)
        .await;
    let model = mock([
        ("Files cross-note.txt", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "cross-chooser",
        "",
        &format!(
            "with note = file {}\nopen http://127.0.0.1:{port}/app\ndo attach the note\n",
            note.display()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_route_set_later_reaches_a_frame_from_another_site() {
    let site = MockServer::start().await;
    let port = site.address().port();
    for (at, body) in [
        (
            "/app",
            format!("<!doctype html><iframe style=\"width: 100%; height: 300px; border: 0\" src=\"http://localhost:{port}/frame\"></iframe>"),
        ),
        (
            "/frame",
            "<!doctype html><button onclick=\"fetch('/api').then(r => r.text()).then(t => out.textContent = t)\">Load</button><p id=\"out\">Nothing yet</p>".into(),
        ),
        ("/api", "real api".into()),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
            .mount(&site)
            .await;
    }
    let model = mock([
        ("routed api", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "cross-route",
        "",
        &format!(
            "open http://127.0.0.1:{port}/app\nsee \"Nothing yet\"\nroute */api answer 200 routed api\ndo load the data\n"
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_file_picker_inside_a_frame_gets_the_named_file() {
    let note = temp_path("framed-note.txt");
    std::fs::write(&note, "hello").unwrap();
    let model = mock([
        ("Opened hello", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><iframe style="width: 100%; height: 300px; border: 0" srcdoc="<button onclick='showOpenFilePicker().then(async ([file]) => out.textContent = &quot;Opened &quot; + (await (await file.getFile()).text()))'>Pick file</button><p id='out'>Nothing yet</p>"></iframe>"#;
    let script = script(
        "framed-picker",
        page,
        &format!(
            "with note = file {}\nopen PAGE\ndo open the note\n",
            note.display()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_followed_tab_gets_the_routes_and_injected_scripts_from_its_first_page() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><a href=\"/help\" target=\"_blank\">Help</a>",
        ),
        (
            "/help",
            "<!doctype html><p>Help center</p><p id=\"flag\"></p><p id=\"api\"></p><script>flag.textContent = window.__flag ? 'flag set' : 'flag missing'; fetch('/api').then(r => r.text()).then(t => api.textContent = t);</script>",
        ),
        ("/api", "real api"),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "follow-prepared",
        "",
        &format!(
            "inject window.__flag = 1\nroute */api answer 200 routed api\nopen {uri}/app\ndo open the help\nsee \"flag set\"\nsee \"routed api\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_followed_tab_that_closes_goes_back_to_the_tab_that_opened_it() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><p>App page</p><a href=\"/help\" target=\"_blank\">Help</a>",
        ),
        (
            "/help",
            "<!doctype html><p>Help center</p><script>setTimeout(() => window.close(), 2000);</script>",
        ),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "closing",
        "",
        &format!(
            "open {uri}/app\ndo open the help\nsee \"Help center\"\ninject document.addEventListener('DOMContentLoaded', () => document.body.append('Injected'))\nwait 3s\nsee \"App page\"\nsee no \"Injected\"\nreload\nsee \"Injected\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn the_step_after_a_followed_tab_closes_goes_to_the_tab_that_opened_it() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><p>App page</p><a href=\"/help\" target=\"_blank\">Help</a>",
        ),
        (
            "/help",
            "<!doctype html><p>Help center</p><script>setTimeout(() => window.close(), 1000);</script>",
        ),
    ])
    .await;
    let model = mock([
        ("Help center", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "closing-reload",
        "",
        &format!(
            "open {uri}/app\ndo open the help\nwait 2s\nreload\nsee \"App page\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_tab_that_closes_before_it_is_followed_leaves_the_page_it_came_from() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><p>App page</p><a href=\"/popup\" target=\"_blank\">Popup</a>",
        ),
        (
            "/popup",
            "<!doctype html><p>Popup</p><script>window.close()</script>",
        ),
    ])
    .await;
    let model = mock([
        ("pressed", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "quick-popup",
        "",
        &format!(
            "open {uri}/app\ndo open the popup\nsee \"App page\"\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_held_request_is_released_from_the_tab_that_sent_it() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><button onclick=\"fetch('/api/pay'); window.open('/secure')\">Pay</button>",
        ),
        ("/secure", "<!doctype html><p>Secure check</p>"),
        ("/api/pay", "paid"),
    ])
    .await;
    let model = mock([
        ("Secure check", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "held-tab",
        "",
        &format!(
            "route */api/pay hold\nopen {uri}/app\ndo pay\nsee \"Secure check\"\nrelease */api/pay\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let paid = site
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|request| request.url.path() == "/api/pay");
    assert!(paid, "{report}");
}

#[tokio::test]
async fn links_that_open_a_new_tab_say_so() {
    let site = site(&[(
        "/app",
        "<!doctype html><a href=\"/help\" target=\"_blank\">Help</a> <a href=\"/home\" target=\"_self\">Home</a><svg><a href=\"/chart\" target=\"_blank\"><text y=\"20\">Chart</text></a></svg>",
    )])
    .await;
    let script = script(
        "tabs",
        "",
        &format!("open {}/app\nsee \"Home\"\n", site.uri()),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    assert!(
        screen.contains(&format!(
            "link \"Help\" · opens a new tab · url \"{}/help\"",
            site.uri()
        )),
        "{report}"
    );
    assert!(
        screen.contains(&format!("link \"Home\" · url \"{}/home\"", site.uri())),
        "{report}"
    );
}

#[tokio::test]
async fn a_table_row_that_opens_on_a_click_can_be_pressed() {
    let site = site(&[(
        "/app",
        "<!doctype html><table><thead><tr><th>Name</th></tr></thead><tbody><tr onclick=\"out.textContent = 'Opened Aviso'\"><td>Aviso de plaga</td></tr></tbody></table><p id=\"out\">Closed</p>",
    )])
    .await;
    let model = mock([
        ("Opened Aviso", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "row",
        "",
        &format!(
            "open {}/app\ndo open the row of Aviso de plaga\nsee \"Opened Aviso\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed row \"Aviso de plaga\" · clickable",
        "{report}"
    );
}

#[tokio::test]
async fn a_row_that_shows_something_else_by_the_time_of_the_press_is_not_pressed() {
    let model = mock_after(
        Duration::from_millis(2500),
        [
            ("Opened", decision("done", "001", "001")),
            ("", decision("press", "001", "001")),
        ],
    )
    .await;
    let script = script(
        "changed-row",
        "<!doctype html><table><tbody><tr onclick=\"out.textContent = 'Opened ' + this.textContent\"><td id=\"cell\">DEV-105 Fix login</td></tr></tbody></table><p id=\"out\">Closed</p><script>setTimeout(() => cell.textContent = 'DEV-099 Other', 5000)</script>",
        "open PAGE\ndo open DEV-105\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let refused = report["steps"][1]["actions"][0].as_str().unwrap();
    assert!(
        refused.starts_with("tried but could not do this")
            && refused
                .ends_with("\"DEV-105 Fix login\" · clickable changed before it could be used)"),
        "{report}"
    );
}

#[tokio::test]
async fn a_long_table_leaves_room_for_the_controls_under_it() {
    let rows: String = (0..300)
        .map(|row| format!("<tr><td>Item {row}</td></tr>"))
        .collect();
    let site = site(&[(
        "/app",
        &format!("<!doctype html><p id=\"out\"></p><table><tbody>{rows}</tbody></table><button onclick=\"out.textContent = 'Page 2'\">Next page</button>"),
    )])
    .await;
    let model = mock([
        ("Page 2", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "long-table",
        "",
        &format!("open {}/app\ndo go to the next page\n", site.uri()),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed button \"Next page\"",
        "{report}"
    );
}

#[tokio::test]
async fn a_root_that_listens_for_the_clicks_of_a_whole_page_is_not_a_control() {
    let script = script(
        "react-root",
        "<!doctype html><div id=\"root\"><div class=\"app\"><h1>Order 4711</h1><p>Your order shipped</p><p>Thanks for buying with us</p></div></div><script>root.addEventListener('click', () => {});</script>",
        "open PAGE\nsee \"Your order shipped\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    assert!(!screen.contains("clickable"), "{screen}");
    assert_eq!(screen.matches("Your order shipped").count(), 1, "{screen}");
}

#[tokio::test]
async fn a_part_the_page_made_clickable_without_a_role_can_be_pressed() {
    let site = site(&[(
        "/app",
        "<!doctype html><button>Issue DEV-101<span id=\"clear\"><svg aria-hidden=\"true\" width=\"10\" height=\"10\"></svg></span></button><div id=\"card\"><span>Open</span> <span>card</span></div><p id=\"out\">Closed</p><script>document.body.addEventListener('click', () => {}); clear.addEventListener('click', event => { event.stopPropagation(); out.textContent = 'Cleared'; }); card.addEventListener('click', () => out.textContent = 'Card opened');</script>",
    )])
    .await;
    let model = mock([
        ("Card opened", decision("done", "001", "001")),
        ("open the card", decision("press", "003", "001")),
        ("Cleared", decision("done", "001", "001")),
        ("", decision("press", "002", "002")),
    ])
    .await;
    let script = script(
        "clickable",
        "",
        &format!(
            "open {}/app\ndo clear the issue\nsee \"Cleared\"\ndo open the card\nsee \"Card opened\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed generic \"\" · clickable",
        "{report}"
    );
    assert_eq!(
        report["steps"][3]["actions"][0], "pressed generic \"Open card\" · clickable",
        "{report}"
    );
    assert_eq!(
        report["screen"]
            .as_str()
            .unwrap()
            .matches("clickable")
            .count(),
        2,
        "{report}"
    );
}

#[tokio::test]
async fn wait_lets_real_time_pass_on_the_same_screen() {
    let site = site(&[(
        "/app",
        "<!doctype html><p id=\"out\">Before</p><script>setTimeout(() => out.textContent = 'After', 700)</script>",
    )])
    .await;
    let script = script(
        "wait",
        "",
        &format!(
            "open {}/app\nsee \"Before\"\nwait 1s\nsee \"After\" within 0s\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][2]["via"], "wait", "{report}");
    assert!(
        report["steps"][2]["seconds"].as_f64().unwrap() >= 1.0,
        "{report}"
    );
}

#[tokio::test]
async fn the_page_clock_runs_its_timers_ahead_without_waiting() {
    let script = script(
        "clock",
        "<!doctype html><p id=\"out\">Waiting</p><p id=\"ticks\"></p><script>const start = new Date(); let count = 0; setTimeout(() => out.textContent = `Done after ${Math.round((Date.now() - start) / 1000)}s`, 60000); setInterval(() => ticks.textContent = `${++count} ticks`, 3000);</script>",
        "clock install\nopen PAGE\nsee \"Waiting\"\nclock run 60s\nsee \"Done after 60s\" within 0s\nsee \"20 ticks\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let seconds: f64 = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["seconds"].as_f64().unwrap())
        .sum();
    assert!(seconds < 10.0, "{report}");
}

#[tokio::test]
async fn a_failed_script_leaves_a_screenshot_until_it_passes() {
    let script = script(
        "failed-shot",
        "<!doctype html><p>Here</p>",
        "open PAGE\nsee \"Gone\" within 0s\n",
    );
    let shot = temp_path("failed-shot.e2e.failed.png");
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["screenshot"], shot.display().to_string());
    assert!(std::fs::read(&shot).unwrap().starts_with(b"\x89PNG"));
    let passing = std::fs::read_to_string(&script)
        .unwrap()
        .replace("\"Gone\" within 0s", "\"Here\"");
    std::fs::write(&script, passing).unwrap();
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(!shot.exists());
}

#[tokio::test]
async fn the_page_clock_runs_ahead_past_a_page_that_polls_without_a_delay() {
    let script = script(
        "polling-clock",
        "<!doctype html><p id=\"out\">Waiting</p><script>const poll = () => setTimeout(poll); poll(); setTimeout(() => out.textContent = 'Ten seconds passed', 10000);</script>",
        "clock install\nopen PAGE\nclock run 10s\nsee \"Ten seconds passed\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][2]["seconds"].as_f64().unwrap() < 5.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_frozen_page_clock_keeps_its_time_while_its_timers_and_events_run() {
    let script = script(
        "frozen-clock",
        "<!doctype html><div id=\"outer\"><button id=\"inner\">Go</button></div><p id=\"out\">Waiting</p><script>const start = Date.now(); setTimeout(() => { const attached = Date.now(); inner.addEventListener('click', event => event.stamped = Date.now()); outer.addEventListener('click', event => out.textContent = event.stamped > attached && new Date().getTime() - start < 1000 ? 'Time stood still' : 'Time moved'); inner.click(); }, 2000);</script>",
        "clock freeze\nopen PAGE\nsee \"Time stood still\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn an_event_after_a_clock_run_is_stamped_with_the_page_clock() {
    let script = script(
        "stamped-clock",
        "<!doctype html><button id=\"go\">Go</button><p id=\"out\">Waiting</p><script>setTimeout(() => { const attached = performance.now(); go.addEventListener('click', event => out.textContent = event.timeStamp >= attached ? 'Stamped on time' : 'Stamped late'); go.click(); }, 30000);</script>",
        "clock install\nopen PAGE\nclock run 30s\nsee \"Stamped on time\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_clock_run_on_a_page_without_the_clock_says_where_to_install_it() {
    let script = script(
        "no-clock",
        "<!doctype html><p>Waiting</p>",
        "open PAGE\nclock run 3s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("put clock install before its open"),
        "{report}"
    );
}

#[tokio::test]
async fn the_model_can_point_at_a_control_to_show_its_tooltip() {
    let site = site(&[(
        "/app",
        "<!doctype html><button onmouseenter=\"tip.textContent = 'Visible to Sandra Kim'\">Reserved</button><p id=\"tip\"></p>",
    )])
    .await;
    let model = mock([
        ("Visible to Sandra Kim", decision("done", "001", "001")),
        ("", decision("hover", "001", "001")),
    ])
    .await;
    let script = script(
        "hover",
        "",
        &format!(
            "open {}/app\ndo show who can see it\nsee \"Visible to Sandra Kim\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"],
        json!(["pointed at button \"Reserved\""])
    );
}

#[tokio::test]
async fn see_and_expect_wait_as_long_as_their_within_says() {
    let model = mock([
        (
            "Build failed",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("", json!({ "holds": yes(0.1), "sight": yes(0.0) })),
    ])
    .await;
    let script = script(
        "within",
        "<!doctype html><p id=\"s\">Building</p><script>setTimeout(() => s.textContent = 'Build failed', 6000);</script>",
        "open PAGE\nexpect the build failed within 9s\nsee \"Build failed\" within 1s\nsee \"Never shown\" within 1s\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps[1]["status"], "passed", "{report}");
    assert!(steps[1]["seconds"].as_f64().unwrap() >= 5.5, "{report}");
    assert_eq!(steps[2]["status"], "passed", "{report}");
    assert_eq!(steps[3]["status"], "failed", "{report}");
    assert!(steps[3]["seconds"].as_f64().unwrap() < 2.0, "{report}");
    assert!(report["model_calls"].as_u64().unwrap() <= 3, "{report}");
}

fn project_folder(name: &str) -> std::path::PathBuf {
    let root = temp_path(name);
    for (file, text) in [
        ("index.html", "<h1>Hi</h1>"),
        ("assets/style.css", "body {}"),
        ("node_modules/x/i.js", "x"),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}

#[tokio::test]
async fn a_goal_gives_the_page_the_files_and_folders_it_names() {
    let notes = [temp_path("first.txt"), temp_path("second.txt")];
    std::fs::write(&notes[0], "one").unwrap();
    std::fs::write(&notes[1], "three").unwrap();
    let folder = project_folder("chosen-project");
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"fi.click()\">Choose files</button><input id=\"fi\" type=\"file\" multiple style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name + ' ' + f.size).join(', ')\"><button onclick=\"di.click()\">Choose folder</button><input id=\"di\" type=\"file\" webkitdirectory style=\"display:none\" onchange=\"out.textContent = 'Folder ' + [...this.files].map(f => f.webkitRelativePath).sort().join(', ')\"><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Folder chosen-project", decision("done", "001", "001")),
        ("Goal: upload the project", decision("press", "002", "001")),
        ("Files first.txt", decision("done", "001", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "upload",
        "",
        &format!(
            "with notes = file {}, {}\nwith project = file {}\nopen {}/app\ndo attach the notes\nsee \"Files first.txt 3, second.txt 5\"\nsee \"for several files first.txt, second.txt\"\ndo upload the project\nsee \"Folder chosen-project/assets/style.css, chosen-project/index.html, chosen-project/node_modules/x/i.js\"\n",
            notes[0].display(),
            notes[1].display(),
            folder.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"],
        json!([
            "pressed button \"Choose files\"",
            "gave the file chooser notes (first.txt, second.txt)"
        ]),
        "{report}"
    );
    let screen = report["screen"].as_str().unwrap();
    assert!(
        screen.contains("- file chooser \"for a folder\" = \"chosen-project\"")
            && !screen.contains("for several files"),
        "{report}"
    );
}

#[tokio::test]
async fn a_file_chooser_gets_only_what_its_input_takes() {
    let notes = [temp_path("only-first.txt"), temp_path("only-second.txt")];
    std::fs::write(&notes[0], "one").unwrap();
    std::fs::write(&notes[1], "two").unwrap();
    let folder = project_folder("only-project");
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"fi.click()\">Choose file</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name).join(', ')\"><button onclick=\"di.click()\">Choose folder</button><input id=\"di\" type=\"file\" webkitdirectory style=\"display:none\" onchange=\"out.textContent = 'Folder of ' + this.files.length\"><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Folder of 3", decision("done", "001", "001")),
        (
            "Goal: upload the notes and the project",
            decision("press", "002", "001"),
        ),
        ("Files only-first.txt", decision("done", "001", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "upload-one",
        "",
        &format!(
            "with notes = file {}, {}\nwith project = file {}\nopen {}/app\ndo attach the notes\nsee no \"only-second.txt\"\ndo upload the notes and the project\nsee \"Folder of 3\"\n",
            notes[0].display(),
            notes[1].display(),
            folder.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][1], "gave the file chooser notes (only-first.txt)",
        "{report}"
    );
    assert_eq!(
        report["steps"][3]["actions"][1], "gave the file chooser project (only-project)",
        "{report}"
    );
}

#[tokio::test]
async fn a_file_chooser_the_page_ignores_is_not_pressed_again_and_again() {
    let note = temp_path("ignored.txt");
    std::fs::write(&note, "one").unwrap();
    let model = mock([("", decision("press", "001", "001"))]).await;
    let script = script(
        "ignored-upload",
        "<!doctype html><button onclick=\"fi.click()\">Choose file</button><input id=\"fi\" type=\"file\" style=\"display:none\"><p>Nothing yet</p>",
        &format!(
            "with note = file {}\nopen PAGE\ndo attach the note\n",
            note.display()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .starts_with("it would repeat this on an unchanged screen"),
        "{report}"
    );
}

#[tokio::test]
async fn a_replay_that_cannot_give_the_files_presses_once() {
    let note = temp_path("pressed-once.txt");
    std::fs::write(&note, "one").unwrap();
    let model = mock([
        ("Files pressed-once.txt", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "press-once",
        "<!doctype html><button onclick=\"fi.click()\">Choose file</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name).join(', ')\"><p id=\"out\">Nothing yet</p>",
        &format!(
            "with note = file {}\nopen PAGE\ndo attach the note\n",
            note.display()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    edit_page(&script, "type=\"file\"", "type=\"file\" webkitdirectory");
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(
        report["steps"][1]["actions"].as_array().unwrap().len(),
        1,
        "{report}"
    );
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("the page asked for a folder, but the goal names only files"),
        "{report}"
    );
}

#[tokio::test]
async fn a_zip_named_again_stays_readable_by_the_page() {
    let folder = project_folder("zipped-twice");
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"fi.click()\">Choose package</button><button onclick=\"fi.files[0].arrayBuffer().then(b => out.textContent = 'Read ' + b.byteLength + ' bytes', e => out.textContent = 'Failed ' + e.name)\">Send</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + this.files[0].name\"><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Read ", decision("done", "001", "001")),
        ("Failed ", decision("done", "001", "001")),
        ("send the package", decision("press", "002", "001")),
        ("Files zipped-twice.zip", decision("done", "001", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "zipped-twice",
        "",
        &format!(
            "with package = zip {}\nopen {}/app\ndo attach the package\ndo send the package\nsee \"Read 359 bytes\"\n",
            folder.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_zip_value_gives_the_page_its_folder_as_one_zip() {
    let folder = project_folder("zipped-project");
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"fi.click()\">Choose package</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name + ' ' + f.size).join(', ')\"><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Files zipped-project.zip", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "zipped",
        "",
        &format!(
            "with package = zip {}\nopen {}/app\ndo attach the package\nsee \"Files zipped-project.zip 359\"\n",
            folder.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let note = temp_path("not-a-folder.txt");
    std::fs::write(&note, "x").unwrap();
    let wrong = self::script(
        "zip-file",
        "",
        &format!(
            "with package = zip {}\nopen {}/app\ndo attach the package\n",
            note.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&wrong, Some(&model), false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("zip takes one folder"),
        "{report}"
    );
}

#[tokio::test]
async fn file_system_pickers_get_the_named_files_and_folders_or_are_cancelled() {
    let folder = project_folder("picked-project");
    let note = temp_path("picked.txt");
    std::fs::write(&note, "hello").unwrap();
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"showDirectoryPicker().then(async folder => { const names = []; const walk = async (handle, prefix) => { for await (const [name, child] of handle.entries()) { if (child.kind === 'directory') await walk(child, prefix + name + '/'); else names.push(prefix + name + ' ' + (await child.getFile()).size); } }; await walk(folder, ''); out.textContent = 'Picked ' + folder.name + ' ' + (folder instanceof FileSystemDirectoryHandle) + ': ' + names.sort().join(', '); }).catch(error => out.textContent = 'Picker failed ' + error.name)\">Pick folder</button><button onclick=\"showOpenFilePicker().then(async ([file]) => { const opened = await file.getFile(); out.textContent = 'Opened ' + (await opened.text()) + ' as ' + opened.type; }).catch(error => out.textContent = 'Picker failed ' + error.name)\">Pick file</button><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Picker failed", decision("done", "001", "001")),
        ("Goal: pick a folder", decision("press", "001", "002")),
        ("Opened hello", decision("done", "001", "001")),
        ("Goal: open the note", decision("press", "002", "001")),
        ("Picked picked-project", decision("done", "001", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "pickers",
        "",
        &format!(
            "with project = file {}\nwith note = file {}\nopen {}/app\ndo pick the project\nsee \"Picked picked-project true: assets/style.css 7, index.html 11, node_modules/x/i.js 1\"\nsee \"for a folder picked-project\"\ndo open the note\nsee \"Opened hello as text/plain\"\nsee \"for one file picked.txt\"\ndo pick a folder\nsee \"Picker failed AbortError\"\n",
            folder.display(),
            note.display(),
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][7]["actions"][1],
        "a file chooser opened, but the goal names no file to give it",
        "{report}"
    );
    assert!(
        !report["screen"].as_str().unwrap().contains("file picker"),
        "{report}"
    );
}

#[tokio::test]
async fn a_download_shows_on_the_screen_with_its_name_and_size() {
    let site = site(&[(
        "/app",
        "<!doctype html><a href=\"/export\">Export the version</a>",
    )])
    .await;
    Mock::given(method("GET"))
        .and(path("/export"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "content-disposition",
                    "attachment; filename=\"alta-de-explotacion-v1.zip\"",
                )
                .set_body_raw(vec![7_u8; 2048], "application/zip"),
        )
        .mount(&site)
        .await;
    let model = mock([
        ("downloaded", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "download",
        "",
        &format!(
            "open {}/app\ndo download the version\nsee \"alta-de-explotacion-v1.zip 2048 bytes\" within 10s\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("download \"alta-de-explotacion-v1.zip\" = \"2048 bytes\" · downloaded"),
        "{report}"
    );
}

#[tokio::test]
async fn a_frame_marked_as_presentation_is_still_read() {
    let script = script(
        "presentational",
        r#"<!doctype html><iframe role="presentation" srcdoc="<p>Inside presentation</p>"></iframe><iframe role="none" srcdoc="<p>Inside none</p>"></iframe>"#,
        "open PAGE\nsee \"Inside presentation\"\nsee \"Inside none\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn the_screen_reads_and_acts_inside_iframes_of_any_origin() {
    let preview = site(&[(
        "/inner",
        "<!doctype html><title>Preview</title><h2>Marker inside</h2><button onclick=\"this.textContent = 'Pressed inside'\">Inner button</button>",
    )])
    .await;
    let isolated = preview.uri().replace("127.0.0.1", "localhost");
    let hub = site(&[
        (
            "/notes",
            "<!doctype html><p>Notes inside</p><button onclick=\"this.textContent = 'Notes pressed'\">Notes button</button>",
        ),
        (
            "/frames",
            &format!(
                "<!doctype html><h1>Outer</h1><div style=\"height: 1500px\"></div><iframe title=\"Preview\" src=\"{isolated}/inner\"></iframe><iframe title=\"Notes\" sandbox=\"allow-scripts allow-same-origin\" src=\"/notes\"></iframe>"
            ),
        ),
    ])
    .await;
    let model = mock([
        ("Notes pressed", decision("done", "001", "001")),
        (
            "Goal: press the notes button",
            decision("press", "002", "001"),
        ),
        ("Pressed inside", decision("done", "001", "001")),
        ("", decision("press", "001", "002")),
    ])
    .await;
    let script = script(
        "frames",
        "",
        &format!(
            "open {}/frames\nsee \"Marker inside\"\nsee \"Notes inside\"\nsee \"{isolated}/inner\"\ndo press the inner button\nsee \"Pressed inside\"\ndo press the notes button\nsee \"Notes pressed\"\n",
            hub.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("- iframe \"Preview\"\n    - page \"Preview\""),
        "{report}"
    );
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("- iframe \"Notes\" = \"sandbox allow-scripts allow-same-origin\""),
        "{report}"
    );
}

#[test]
fn scripts_run_commands_start_services_read_their_logs_and_reuse_steps() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let site = temp_path("served");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("page.html"), "<!doctype html><p>Served page</p>").unwrap();
    let parts = temp_path("parts");
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(
        parts.join("visit.e2e"),
        format!("open http://127.0.0.1:{port}/page.html\nsee \"Served page\"\n"),
    )
    .unwrap();
    let script = script(
        "shell",
        "",
        &format!(
            "with token = run printf 'tok-%s' 987654321; echo warning >&2\nrun test \"$token\" = \"$(printf 'tok-%s' 987654321)\" && echo \"hello $token\"\nsee \"hello\"\nsee no \"987654321\"\nwith base = http://127.0.0.1:{port}\nstart site = python3 -m http.server {port} --bind 127.0.0.1 -d {} at $base/page.html\nuse parts/visit.e2e\nlogs site\nsee \"GET /page.html\"\ninject document.addEventListener('DOMContentLoaded', () => document.body.insertAdjacentHTML('beforeend', '<p>Injected early</p>'))\nreload\nsee \"Injected early\"\nrun echo done; exit 3\n",
            site.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 12, "{report}");
    assert_eq!(steps[0]["detail"], "exit 0");
    assert_eq!(steps[11]["status"], "failed");
    assert_eq!(steps[11]["detail"], "exit 3: done");
    assert!(!report.to_string().contains("tok-987654321"), "{report}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the service still runs after the script"
    );
}

#[tokio::test]
async fn extract_takes_a_value_from_the_screen_and_replays_where_it_came_from() {
    let site = site(&[
        (
            "/app",
            "<!doctype html><title>App</title><a href=\"/next/42\">Next</a>",
        ),
        ("/next/42", "<!doctype html><p>Step 42</p>"),
        ("/next/43", "<!doctype html><p>Step 43</p>"),
    ])
    .await;
    let model = mock([(
        "",
        json!({ "extract": choice("004"), "extract_reversed": choice("001") }),
    )])
    .await;
    let script = script(
        "extract",
        "",
        &format!(
            "open {}/app\nextract next = the address the Next link goes to\nopen $next\nsee \"Step\"\n",
            site.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        format!("next = \"{}/next/42\"", site.uri())
    );
    let page = site.uri();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<!doctype html><title>App</title><a href=\"/next/43\">Next</a>",
            "text/html",
        ))
        .with_priority(1)
        .mount(&site)
        .await;
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay");
    assert_eq!(
        report["steps"][1]["detail"],
        format!("next = \"{page}/next/43\"")
    );
    assert!(
        report["screen"].as_str().unwrap().contains("Step 43"),
        "{report}"
    );
}

#[tokio::test]
async fn extract_never_replays_one_of_two_alike_places() {
    let model = mock([(
        "",
        json!({ "extract": choice("004"), "extract_reversed": choice("001") }),
    )])
    .await;
    let script = script(
        "twin-links",
        "<!doctype html><a href=\"a.html\">Edit</a><a href=\"b.html\">Edit</a>",
        "open PAGE\nextract next = the address of the second Edit link\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let picked = report["steps"][1]["detail"].as_str().unwrap().to_owned();
    assert!(picked.ends_with("b.html\""), "{report}");

    let (locked, report) = act(&script, None, true);
    assert_ne!(
        report["steps"][1]["detail"].as_str(),
        Some(picked.replace("b.html", "a.html").as_str()),
        "{report}"
    );
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 2.0
            && report["steps"][1]["detail"]
                .as_str()
                .unwrap()
                .contains("a place another value shares"),
        "{report}"
    );
}

#[tokio::test]
async fn extract_replays_a_text_whose_numbers_change() {
    let model = mock([(
        "",
        json!({ "extract": choice("002"), "extract_reversed": choice("001") }),
    )])
    .await;
    let script = script(
        "order",
        "<!doctype html><p>Order 4711</p>",
        "open PAGE\nextract order = the order number\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "order = \"Order 4711\"",
        "{report}"
    );
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(!lock.contains("4711"), "{lock}");
    edit_page(&script, "Order 4711", "Order 4712");

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay", "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "order = \"Order 4712\"",
        "{report}"
    );
    edit_page(&script, "Order 4712", "Order 4713</p><p>Order 5");

    let (ambiguous, report) = act(&script, None, true);
    assert_eq!(ambiguous.status.code(), Some(1), "{report}");
}

#[tokio::test]
async fn extract_waits_for_the_place_it_recorded() {
    let model = mock([(
        "",
        json!({ "extract": choice("002"), "extract_reversed": choice("001") }),
    )])
    .await;
    let script = script(
        "late-order",
        "<!doctype html><p id=\"o\">Order 4711</p>",
        "open PAGE\nextract order = the order number\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    edit_page(
        &script,
        "<p id=\"o\">Order 4711</p>",
        "<p id=\"o\">Loading</p><script>setTimeout(() => o.textContent = 'Order 4712', 400)</script>",
    );

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "order = \"Order 4712\"",
        "{report}"
    );
}

#[test]
fn extract_never_replaces_a_secret() {
    let script = script(
        "extract-secret",
        "<!doctype html><p>Code 42</p>",
        "with token = $IU2U_TEST_SECRET\nopen PAGE\nextract token = the code\n",
    );
    let output = command(None)
        .arg("--json")
        .arg(&script)
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("$token is a secret"),
        "{report}"
    );
}

#[tokio::test]
async fn extract_takes_a_whole_field_and_leaves_out_texts_with_secrets() {
    let model = mock([(
        "",
        json!({ "extract": choice("003"), "extract_reversed": choice("001") }),
    )])
    .await;
    let code = "x".repeat(300);
    let script = script(
        "whole",
        &format!(
            "<!doctype html><input aria-label=\"Code\" value=\"{code}\"><p>Signed in as {SECRET}</p>"
        ),
        "with who = $IU2U_TEST_SECRET\nopen PAGE\nextract code = the code in the field\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        format!("code = {code:?}"),
        "{report}"
    );
}

#[tokio::test]
async fn extract_leaves_out_a_response_value_with_an_encoded_secret() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/callback"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "/cb?t=sk-live%20%207f3a"),
        )
        .mount(&api)
        .await;
    let model = mock([(
        "",
        json!({ "extract": choice("none"), "extract_reversed": choice("none") }),
    )])
    .await;
    let script = script(
        "encoded-secret",
        "",
        &format!(
            "with token = $IU2U_TEST_SECRET\nrequest GET {}/callback\nextract next = where the callback goes\n",
            api.uri()
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let asked = model.received_requests().await.unwrap();
    assert!(!asked.is_empty());
    for request in asked {
        assert!(!String::from_utf8_lossy(&request.body).contains("7f3a"));
    }
}

#[tokio::test]
async fn extract_says_when_values_were_left_out() {
    let api = MockServer::start().await;
    let people: Vec<Value> = (0..300).map(|id| json!({ "id": id })).collect();
    Mock::given(method("GET"))
        .and(path("/people"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "people": people })))
        .mount(&api)
        .await;
    let model = mock([(
        "",
        json!({ "extract": choice("none"), "extract_reversed": choice("none") }),
    )])
    .await;
    let script = script(
        "many",
        "",
        &format!(
            "request GET {}/people\nextract id = the id of the last person\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .ends_with("50 values after the first 254 were not offered"),
        "{report}"
    );
}

#[tokio::test]
async fn extract_replays_an_output_line_by_what_it_says() {
    let model = mock([(
        "",
        json!({ "extract": choice("001"), "extract_reversed": choice("002") }),
    )])
    .await;
    let script = script(
        "output-line",
        "",
        "run printf 'Created user 41\\n'\nextract id = the id of the new user\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "id = \"Created user 41\"",
        "{report}"
    );
    let shifted = std::fs::read_to_string(&script).unwrap().replace(
        "printf '",
        "printf 'warning: this node is old\\nCreated user 42\\n' || printf '",
    );
    std::fs::write(&script, shifted).unwrap();

    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "id = \"Created user 42\"",
        "{report}"
    );
}

#[test]
fn a_command_that_prints_a_lot_keeps_only_its_last_mebibyte() {
    let noisy = "head -c 3000000 /dev/zero | tr '\\0' a | fold -w 100; echo; echo LAST";
    let printed = script(
        "noisy",
        "",
        &format!("run {noisy}\nsee \"LAST\"\nsee \"only its last 1048576 bytes are kept\"\n"),
    );
    let (output, report) = act(&printed, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let value = script(
        "noisy-value",
        "",
        &format!("with big = run {noisy}\nwait 1ms\n"),
    );
    let (output, report) = act(&value, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("with big: the command printed more than 1048576 bytes"),
        "{report}"
    );
    let quiet = script(
        "noisy-errors",
        "",
        "with token = run head -c 2000000 /dev/zero | tr '\\0' x >&2; echo tok-123\nrun test \"$token\" = tok-123\n",
    );
    let (output, report) = act(&quiet, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn logs_of_a_service_that_prints_a_lot_keep_only_their_last_mebibyte() {
    let script = script(
        "noisy-logs",
        "",
        "start noisy = head -c 3000000 /dev/zero | tr '\\0' b | fold -w 100; echo; echo DONE; sleep 30\nlogs noisy\nsee \"DONE\" within 10s\nsee \"the log began earlier; only its last 1048576 bytes are kept\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn run_shows_its_output_in_the_order_it_was_written() {
    let script = script(
        "interleaved",
        "",
        "run echo first; echo second >&2; echo third\nsee \"third\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    let at = |text: &str| screen.find(text).unwrap();
    assert!(
        at("first") < at("second") && at("second") < at("third"),
        "{screen}"
    );
}

#[tokio::test]
async fn extract_from_a_long_output_offers_its_last_lines() {
    let model = mock([(
        "",
        json!({ "extract": choice("253"), "extract_reversed": choice("002") }),
    )])
    .await;
    let script = script(
        "long-extract",
        "",
        "run seq -f 'line %g' 1 400; echo 'Created user 42'\nextract id = the id of the new user\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "id = \"Created user 42\"",
        "{report}"
    );
}

#[tokio::test]
async fn extract_takes_a_field_from_a_json_response() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/prototypes"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", "/api/prototypes/p-7")
                .set_body_json(
                    json!({ "prototype": { "id": "p-7", "versions": [{ "number": 1 }] }, "token": SECRET }),
                ),
        )
        .mount(&api)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/prototypes/p-7"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&api)
        .await;
    let model = mock([
        (
            "the address",
            json!({ "extract": choice("002"), "extract_reversed": choice("006") }),
        ),
        (
            "",
            json!({ "extract": choice("006"), "extract_reversed": choice("002") }),
        ),
    ])
    .await;
    let script = script(
        "json",
        "",
        &format!(
            "with token = $IU2U_TEST_SECRET\nrequest POST {uri}/api/prototypes\nextract id = the id of the new prototype\nextract place = the address of the new prototype\nrequest DELETE {uri}$place\nsee \"HTTP 204\"\n",
            uri = api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["detail"], "id = \"p-7\"");
    assert_eq!(
        report["steps"][2]["detail"], "place = \"/api/prototypes/p-7\"",
        "{report}"
    );
    for asked in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&asked.body).contains("7f3a"));
    }
}

#[tokio::test]
async fn an_inline_image_leaves_the_rest_of_the_page_in_view() {
    let model = mock([
        (
            "Hello there",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        (
            "\"holds\"",
            json!({ "holds": yes(0.05), "sight": yes(0.0) }),
        ),
    ])
    .await;
    let image = "A".repeat(8000);
    let script = script(
        "inline-image",
        &format!(
            "<!doctype html><img alt=\"Logo\" src=\"data:image/png;base64,{image}\"><p>Hello there</p>"
        ),
        "open PAGE\nexpect the page greets the visitor\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_model_that_sees_presses_a_spot_that_no_control_stands_for() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Hit the marker"))
        .and(body_string_contains("\"operation\":{"))
        .respond_with(reply(json!({ "operation": choice("done") })))
        .mount(&model)
        .await;
    for (question, label) in [
        ("operation", "spot"),
        ("spot", "019"),
        ("spot_reversed", "030"),
        ("spot_closer", "001"),
        ("spot_closer_reversed", "064"),
    ] {
        Mock::given(method("POST"))
            .and(body_string_contains(format!("\"{question}\":{{")))
            .respond_with(reply(json!({ question: choice(label) })))
            .mount(&model)
            .await;
    }
    let script = script(
        "spot",
        "<!doctype html><body style=\"margin: 0\"><div style=\"height: 100vh; background: #eef\"></div><p id=\"out\" style=\"position: fixed; bottom: 0; margin: 0\">Waiting</p><script>document.addEventListener('click', event => { const [x, y] = [event.clientX / innerWidth, event.clientY / innerHeight]; out.textContent = x > 0.218 && x < 0.243 && y > 0.291 && y < 0.323 ? 'Hit the marker' : `Missed at ${x} ${y}`; });</script>",
        "open PAGE\ndo press the marker on the map\nsee \"Hit the marker\"\nsee no \"001\"\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed the spot 23% across and 31% down the screen",
        "{report}"
    );
    let closer = model
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .find(|body| body.contains("\"spot_closer\":{"))
        .unwrap();
    assert!(closer.contains("\"data_base64\":\"iVBORw0KGgo"), "{closer}");
    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 0, "{report}");
}

#[tokio::test]
async fn a_spot_grid_shows_above_a_modal_dialog() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Grid on top"))
        .and(body_string_contains("\"operation\":{"))
        .respond_with(reply(json!({ "operation": choice("done") })))
        .mount(&model)
        .await;
    for (question, label) in [
        ("operation", "spot"),
        ("spot", "019"),
        ("spot_reversed", "030"),
        ("spot_closer", "001"),
        ("spot_closer_reversed", "064"),
    ] {
        Mock::given(method("POST"))
            .and(body_string_contains(format!("\"{question}\":{{")))
            .respond_with(reply(json!({ question: choice(label) })))
            .mount(&model)
            .await;
    }
    let script = script(
        "spot-modal",
        "<!doctype html><body style=\"margin: 0\"><dialog id=\"map\" style=\"width: 100vw; height: 100vh; max-width: none; max-height: none; margin: 0; border: 0\"><p id=\"out\">Waiting</p></dialog><script>map.showModal(); new MutationObserver(() => { const grid = document.getElementById('__iu2u_grid'); if (grid) out.textContent = grid.matches(':popover-open') ? 'Grid on top' : 'Grid under'; }).observe(document.documentElement, { childList: true });</script>",
        "open PAGE\ndo press the marker on the map\nsee \"Grid on top\"\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_spot_grid_is_drawn_on_a_page_with_a_strict_content_policy() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("Hit the marker"))
        .and(body_string_contains("\"operation\":{"))
        .respond_with(reply(json!({ "operation": choice("done") })))
        .mount(&model)
        .await;
    for (question, label) in [
        ("operation", "spot"),
        ("spot", "019"),
        ("spot_reversed", "030"),
        ("spot_closer", "001"),
        ("spot_closer_reversed", "064"),
    ] {
        Mock::given(method("POST"))
            .and(body_string_contains(format!("\"{question}\":{{")))
            .respond_with(reply(json!({ question: choice(label) })))
            .mount(&model)
            .await;
    }
    let script = script(
        "spot-csp",
        "<!doctype html><meta http-equiv=\"Content-Security-Policy\" content=\"style-src 'self'; require-trusted-types-for 'script'\"><body style=\"margin: 0\"><div style=\"height: 100vh; background: #eef\"></div><p id=\"out\" style=\"position: fixed; bottom: 0; margin: 0\">Waiting</p><script>document.addEventListener('click', event => { const [x, y] = [event.clientX / innerWidth, event.clientY / innerHeight]; out.textContent = x > 0.218 && x < 0.243 && y > 0.291 && y < 0.323 ? 'Hit the marker' : `Missed at ${x} ${y}`; });</script>",
        "open PAGE\ndo press the marker on the map\nsee \"Hit the marker\"\nsee no \"001\"\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed the spot 23% across and 31% down the screen",
        "{report}"
    );
    let closer = model
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .find(|body| body.contains("\"spot_closer\":{"))
        .unwrap();
    assert!(closer.contains("\"data_base64\":\"iVBORw0KGgo"), "{closer}");
    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 0, "{report}");
}

#[tokio::test]
async fn a_model_that_sees_gets_a_screenshot_with_every_question() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("TypeSafe-Danger-Zone-Images", "true"))
        .and(body_string_contains(
            "Screenshot of the part of the screen in view, while the text above also lists what lies out of view: iu2u-screenshot",
        ))
        .and(body_string_contains(
            "\"iu2u-screenshot\":{\"data_base64\":\"iVBORw0KGgo",
        ))
        .respond_with(reply(json!({ "holds": yes(0.95), "sight": yes(0.9) })))
        .mount(&model)
        .await;
    let script = script(
        "vision",
        "<!doctype html><p style=\"color: green\">Latest version</p>",
        "open PAGE\nexpect the version is shown in green\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        "the model is 95% sure it holds"
    );
    for request in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&request.body).contains("given as text only"));
    }
}

#[tokio::test]
async fn a_model_without_a_screenshot_is_told_the_screen_is_text_only() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(
            "The screen is given as text only: it does not show colours, positions, sizes or pictures.",
        ))
        .respond_with(reply(json!({ "holds": yes(0.95), "sight": yes(0.0) })))
        .mount(&model)
        .await;
    let script = script(
        "text-only",
        "<!doctype html><p>Latest version</p>",
        "open PAGE\nexpect the page shows the latest version\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_model_that_sees_gets_one_screenshot_per_screen() {
    let site = site(&[(
        "/app",
        "<!doctype html><label>Name <input></label><label>City <input></label><script>onload = () => document.body.insertAdjacentHTML('beforeend', '<img alt=\"Stuck\" src=\"/stuck.png\">');</script>",
    )])
    .await;
    Mock::given(method("GET"))
        .and(path("/stuck.png"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&site)
        .await;
    let model = mock([
        ("Ana", decision("done", "001", "001")),
        ("", decision("type", "001", "002")),
    ])
    .await;
    let script = script(
        "one-shot",
        "",
        &format!(
            "with name = Ana\nopen {}/app\ndo type the name\n",
            site.uri()
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 5, "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 18.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_model_that_sees_gets_a_new_screenshot_in_each_step() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "recolored",
        "<!doctype html><p id=\"s\" style=\"color: red\">Status</p><script>setTimeout(() => s.style.color = 'green', 1000)</script>",
        "open PAGE\nexpect the status is shown\nwait 2s\nexpect the status is shown in green\n",
    );
    let output = command(Some(&model))
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let shots: Vec<Value> = model
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["assets"]["iu2u-screenshot"]
                ["data_base64"]
                .clone()
        })
        .collect();
    assert_eq!(shots.len(), 2);
    assert_ne!(shots[0], shots[1]);
}

#[tokio::test]
async fn a_claim_on_how_the_page_looks_is_asked_again_when_only_its_look_changes() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(json!({ "holds": yes(0.05), "sight": yes(0.9) })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&model)
        .await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "turns-green",
        "<!doctype html><p id=\"s\" style=\"color: red\">Status</p><script>setTimeout(() => s.style.color = 'green', 1000)</script>",
        "open PAGE\nexpect the status is shown in green within 10s\n",
    );
    let output = command(Some(&model))
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(model.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_model_that_sees_is_not_held_up_by_a_page_that_reloads_itself() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.05), "sight": yes(0.9) })).await;
    let script = script(
        "reloading",
        "<!doctype html><meta http-equiv=\"refresh\" content=\"1\"><p>Loading</p>",
        "open PAGE\nexpect the page shows the report within 8s\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 28.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_model_that_sees_gets_no_screenshot_while_a_secret_shows() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "secret-shot",
        &format!("<!doctype html><p>Token {SECRET}</p>"),
        "with token = $IU2U_TEST_SECRET\nopen PAGE\nexpect the token is shown\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .ends_with("no screenshot is sent while a secret shows on the screen"),
        "{report}"
    );
    let asked = model.received_requests().await.unwrap();
    assert!(!asked.is_empty());
    for request in asked {
        assert!(!String::from_utf8_lossy(&request.body).contains("iu2u-screenshot"));
    }
}

#[tokio::test]
async fn a_model_that_sees_gets_no_screenshot_of_a_secret_painted_outside_the_page_text() {
    for (case, (page, shot)) in [
        (format!("<p aria-hidden=\"true\">{SECRET}</p>"), false),
        (
            format!("<input aria-hidden=\"true\" value=\"{SECRET}\">"),
            false,
        ),
        (
            format!("<input type=\"password\" value=\"{SECRET}\">"),
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let model = MockServer::start().await;
        answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
        let script = script(
            &format!("painted-secret-{case}"),
            &format!("<!doctype html><p>Settings</p>{page}"),
            "with token = $IU2U_TEST_SECRET\nopen PAGE\nexpect the settings are shown\n",
        );
        let output = command(Some(&model))
            .arg("--json")
            .arg(&script)
            .env("IU2U_VISION", "true")
            .env("IU2U_TEST_SECRET", SECRET)
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{report}");
        let sent = model
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| String::from_utf8_lossy(&request.body).contains("iu2u-screenshot"));
        assert_eq!(sent, shot, "{page}: {report}");
    }
}

#[tokio::test]
async fn a_model_that_sees_gets_no_screenshot_of_a_secret_that_shows_while_it_is_taken() {
    let site = site(&[(
        "/app",
        &format!(
            "<!doctype html><p id=\"out\">Settings</p><script>onload = () => {{ document.body.insertAdjacentHTML('beforeend', '<img alt=\"Stuck\" src=\"/stuck.png\">'); setTimeout(() => out.textContent = 'Token {SECRET}', 1000); }};</script>"
        ),
    )])
    .await;
    Mock::given(method("GET"))
        .and(path("/stuck.png"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&site)
        .await;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "late-secret",
        "",
        &format!(
            "with token = $IU2U_TEST_SECRET\nopen {}/app\nexpect the settings are shown\n",
            site.uri()
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    for request in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&request.body).contains("iu2u-screenshot"));
    }
}

#[tokio::test]
async fn a_page_with_a_frame_act_did_not_read_gets_no_screenshot_while_secrets_exist() {
    let site = MockServer::start().await;
    let port = site.address().port();
    let frames: String = (1..=11)
        .map(|frame| format!("<iframe style=\"width: 90px; height: 40px\" src=\"http://localhost:{port}/frame{frame}\"></iframe>"))
        .collect();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("<!doctype html><p>Frames</p>{frames}"), "text/html"),
        )
        .mount(&site)
        .await;
    for frame in 1..=11 {
        let text = if frame == 11 {
            format!("Token {SECRET}")
        } else {
            format!("Frame {frame}")
        };
        Mock::given(method("GET"))
            .and(path(format!("/frame{frame}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(format!("<!doctype html><p>{text}</p>"), "text/html"),
            )
            .mount(&site)
            .await;
    }
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "unread-frame",
        "",
        &format!(
            "with token = $IU2U_TEST_SECRET\nopen http://127.0.0.1:{port}/app\nsee \"Frame 10\"\nexpect the frames are shown\n"
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    let requests = model.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    for request in requests {
        assert!(!String::from_utf8_lossy(&request.body).contains("iu2u-screenshot"));
    }
}

#[tokio::test]
async fn a_spot_grid_is_not_sent_once_a_secret_shows_on_the_page() {
    let model = mock_after(
        Duration::from_millis(1500),
        [
            ("\"operation\":{", json!({ "operation": choice("spot") })),
            ("\"spot\":{", json!({ "spot": choice("001") })),
        ],
    )
    .await;
    let script = script(
        "late-secret-spot",
        &format!(
            "<!doctype html><p id=\"out\">Map</p><script>setTimeout(() => out.textContent = 'Token {SECRET}', 1000);</script>"
        ),
        "with token = $IU2U_TEST_SECRET\nopen PAGE\ndo press the capital on the map\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .env("IU2U_TEST_SECRET", SECRET)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report
            .to_string()
            .contains("so its screenshot was not sent"),
        "{report}"
    );
    for request in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&request.body).contains("\"spot\":{"));
    }
}

#[tokio::test]
async fn a_model_that_sees_gets_the_screenshot_once_the_pictures_on_it_load() {
    let site = site(&[(
        "/app",
        "<!doctype html><p>Gallery</p><script>setTimeout(() => document.body.insertAdjacentHTML('beforeend', '<img alt=\"Field\" src=\"/field.png\">'), 800);</script>",
    )])
    .await;
    Mock::given(method("GET"))
        .and(path("/field.png"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1500)))
        .mount(&site)
        .await;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "pictures",
        "",
        &format!(
            "open {}/app\nsee \"Field\"\nexpect the field picture is shown\n",
            site.uri()
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][2]["seconds"].as_f64().unwrap() >= 1.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_model_that_sees_gets_the_part_of_a_scrolled_page_in_view() {
    let site = site(&[
        (
            "/top",
            "<!doctype html><html style=\"scrollbar-width: none\"><body style=\"margin: 0\"><div style=\"height: 3000px; background: blue\"></div><div style=\"height: 3000px; background: red\"></div>",
        ),
        (
            "/scrolled",
            "<!doctype html><html style=\"scrollbar-width: none\"><body style=\"margin: 0\" onload=\"scrollTo(0, 4000)\"><div style=\"height: 3000px; background: red\"></div><div style=\"height: 3000px; background: blue\"></div>",
        ),
    ])
    .await;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "scrolled",
        "",
        &format!(
            "open {0}/top\nexpect the page is blue\nopen {0}/scrolled\nexpect the page is blue\n",
            site.uri()
        ),
    );
    let output = command(Some(&model))
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let shots: Vec<Value> = model
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["assets"]["iu2u-screenshot"]
                ["data_base64"]
                .clone()
        })
        .collect();
    assert_eq!(shots.len(), 2);
    assert_eq!(shots[0], shots[1]);
}

#[tokio::test]
async fn a_claim_is_asked_again_while_the_screen_changes_for_a_moment() {
    let model = mock([
        (
            "Order shipped",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        (
            "\"holds\"",
            json!({ "holds": yes(0.05), "sight": yes(0.0) }),
        ),
    ])
    .await;
    let script = script(
        "patient",
        "<!doctype html><p id=\"o\">Packing</p><script>setTimeout(() => o.textContent = 'Order shipped', 1500);</script>",
        "open PAGE\nexpect the order is on its way\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        "the model is 95% sure it holds"
    );
    assert_eq!(report["model_calls"], 2, "{report}");
}

#[tokio::test]
async fn a_claim_that_needs_sight_warns_without_vision_and_on_replay() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "sight",
        "<!doctype html><p style=\"color: green\">Latest version</p>",
        "open PAGE\nexpect the version is shown in green\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        "the model is 95% sure it holds; this claim needs to see the page, but the model only read its text: set IU2U_VISION=true with a model that sees"
    );
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        "replayed from the text of the screen: how the page looks was not checked without the model"
    );
}

#[tokio::test]
async fn a_claim_that_needs_sight_is_asked_again_by_a_model_that_sees() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "sight-again",
        "<!doctype html><p style=\"color: red\">Delete</p>",
        "open PAGE\nexpect the Delete text is shown in red\n",
    );
    for _ in 0..2 {
        let output = command(Some(&model))
            .arg("--json")
            .arg(&script)
            .env("IU2U_VISION", "true")
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{report}");
        assert_eq!(report["steps"][1]["via"], "model", "{report}");
    }
}

#[tokio::test]
async fn a_claim_that_needs_sight_warns_when_no_screenshot_goes_with_it() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.9) })).await;
    let script = script(
        "output-sight",
        "",
        "run echo Latest\nexpect the output is shown in bold\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_VISION", "true")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["detail"],
        "the model is 95% sure it holds; this claim needs to see the screen, but the model only read its text: only a page gets a screenshot, not an app, a response or a command's output"
    );
}

#[tokio::test]
async fn a_replay_ignores_numbers_that_change_unless_the_step_mentions_them() {
    let model = mock([
        (
            "\"holds\"",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("Published", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "numbers",
        "<!doctype html><p>Updated 22 days ago, build 4711</p><p>3 items in the list</p><button onclick=\"out.textContent = 'Published at ' + Date.now()\">Publish</button><p id=\"out\"></p>",
        "open PAGE\ndo publish the prototype\nexpect the list shows 3 items\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    edit_page(
        &script,
        "22 days ago, build 4711",
        "23 days ago, build 4712",
    );
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay", "{report}");
    assert_eq!(report["steps"][2]["via"], "cache", "{report}");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay", "{report}");
    assert_eq!(report["steps"][2]["via"], "model", "{report}");
    edit_page(&script, "3 items", "4 items");
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][1]["via"], "replay", "{report}");
    assert_eq!(report["steps"][2]["status"], "failed", "{report}");
}

#[test]
fn a_before_script_starts_services_and_values_for_every_script() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let site = temp_path("suite-site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("page.html"), "<!doctype html><p>Suite page</p>").unwrap();
    let before = script(
        "setup",
        "",
        &format!(
            "with page = http://127.0.0.1:{port}/page.html\nstart site = python3 -m http.server {port} --bind 127.0.0.1 -d {} at http://127.0.0.1:{port}/page.html\n",
            site.display()
        ),
    );
    let first = script("first", "", "open $page\nsee \"Suite page\"\n");
    let second = script(
        "second",
        "",
        "logs site\nsee no \"GET /page.html\" within 0s\nrequest GET $page\nlogs site\nsee \"GET /page.html\"\n",
    );
    let output = command(None)
        .arg("--json")
        .arg("--before")
        .arg(&before)
        .arg(&first)
        .arg(&second)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert_eq!(shown.lines().count(), 3, "{shown}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the service still runs after the last script"
    );
}

#[tokio::test]
async fn a_script_value_from_a_command_wins_over_the_before_script() {
    let model = mock([
        ("Hello (secret", decision("done", "001", "001")),
        ("", decision("type", "001", "001")),
    ])
    .await;
    let before = script("shadow-setup", "", "with name = run printf Admin\n");
    let typing = script(
        "shadow-later",
        "<!doctype html><label>Name <input oninput=\"out.textContent = 'Hello ' + this.value\"></label><p id=\"out\"></p>",
        "with name = run printf Guest\nopen PAGE\ndo type the name\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg("--before")
        .arg(&before)
        .arg(&typing)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{shown}");
}

#[test]
fn the_logs_of_a_service_show_even_with_bytes_that_are_not_text() {
    let script = script(
        "latin",
        "",
        "start noisy = printf 'caf\\351 ERROR\\nready\\n'; sleep 30\nlogs noisy\nsee \"ready\" within 5s\nsee \"ERROR\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn a_before_script_that_fails_stops_the_run() {
    let before = script("failing-setup", "", "run exit 3\n");
    let after = script("never-run", "", "run echo ran\n");
    let output = command(None)
        .arg("--json")
        .arg("--before")
        .arg(&before)
        .arg(&after)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "{shown}");
    assert_eq!(shown.lines().count(), 1, "{shown}");
}

#[tokio::test]
async fn a_file_from_the_before_script_can_be_given_to_the_page() {
    let note = temp_path("shared-note.txt");
    std::fs::write(&note, "hello").unwrap();
    let site = site(&[(
        "/app",
        "<!doctype html><button onclick=\"fi.click()\">Choose file</button><input id=\"fi\" type=\"file\" style=\"display:none\" onchange=\"out.textContent = 'Files ' + [...this.files].map(f => f.name + ' ' + f.size).join(', ')\"><p id=\"out\">Nothing yet</p>",
    )])
    .await;
    let model = mock([
        ("Files shared-note.txt", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let before = script(
        "files-setup",
        "",
        &format!("with note = file {}\n", note.display()),
    );
    let attaching = script(
        "files-later",
        "",
        &format!(
            "open {}/app\ndo attach the note\nsee \"Files shared-note.txt 5\"\n",
            site.uri()
        ),
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg("--before")
        .arg(&before)
        .arg(&attaching)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{shown}");
}

#[tokio::test]
async fn a_value_from_the_before_script_can_be_typed() {
    let model = mock([
        ("Hello Sandra", decision("done", "001", "001")),
        ("", decision("type", "001", "001")),
    ])
    .await;
    let before = script("typed-setup", "", "with name = Sandra\n");
    let typing = script(
        "typed-later",
        "<!doctype html><label>Name <input oninput=\"out.textContent = 'Hello ' + this.value\"></label><p id=\"out\"></p>",
        "open PAGE\ndo type the name\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg("--before")
        .arg(&before)
        .arg(&typing)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("typed \\\"Sandra\\\""), "{shown}");
}

#[tokio::test]
async fn a_start_that_waits_goes_back_to_the_page() {
    let api = site(&[("/api", "from the api")]).await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let script = script(
        "start-within",
        "<!doctype html><p>Page text</p>",
        &format!(
            "open PAGE\nrequest GET {uri}/api\nsee \"from the api\"\nstart web = python3 -m http.server {port} --bind 127.0.0.1 at http://127.0.0.1:{port}/ within 10s\nsee \"Page text\" within 0s\n",
            uri = api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

fn interrupt_when_started(
    name: &str,
    steps: &str,
    pid_files: &[&Path],
) -> (Option<i32>, Vec<String>) {
    signal_when_started("-INT", name, steps, pid_files)
}

fn signal_when_started(
    signal: &str,
    name: &str,
    steps: &str,
    pid_files: &[&Path],
) -> (Option<i32>, Vec<String>) {
    let script = temp_path(&format!("{name}.e2e"));
    std::fs::write(&script, steps).unwrap();
    let mut running = command(None).arg(&script).spawn().unwrap();
    let started = std::time::Instant::now();
    let pids: Vec<String> = pid_files
        .iter()
        .map(|file| {
            loop {
                if let Some(pid) = std::fs::read_to_string(file)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                {
                    break pid.to_string();
                }
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .collect();
    Command::new("kill")
        .args([signal, &running.id().to_string()])
        .status()
        .unwrap();
    let status = running.wait().unwrap();
    let alive: Vec<String> = pids
        .into_iter()
        .filter(|pid| {
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        })
        .collect();
    for pid in &alive {
        Command::new("kill").args(["-9", pid]).status().unwrap();
    }
    (status.code(), alive)
}

#[test]
fn an_interrupted_run_stops_the_services_and_commands_it_started() {
    let (service, command_child) = (temp_path("idle.pid"), temp_path("ran.pid"));
    let (code, alive) = interrupt_when_started(
        "interrupted",
        &format!(
            "start idle = echo $$ > {} && exec sleep 300\nrun sleep 300 & echo $! > {}; wait\n",
            service.display(),
            command_child.display()
        ),
        &[&service, &command_child],
    );
    assert_eq!(code, Some(130));
    assert!(
        alive.is_empty(),
        "still running after the interrupt: {alive:?}"
    );
}

#[test]
fn a_run_stopped_by_a_signal_reports_the_script_it_stopped() {
    let started = temp_path("stopped-started");
    std::fs::remove_file(&started).ok();
    let page = temp_path("stopped.html");
    std::fs::write(&page, "<!doctype html><p>Hello</p>").unwrap();
    let script = temp_path("stopped.e2e");
    std::fs::write(
        &script,
        format!(
            "open file://{}\nrun touch {}\nwait 30s\n",
            page.display(),
            started.display()
        ),
    )
    .unwrap();
    let running = command(None)
        .arg("--json")
        .arg(&script)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = running.id();
    while !started.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    let output = running.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(143));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "error", "{report}");
    assert_eq!(report["error"], "stopped by a signal", "{report}");
    assert_eq!(report["script"], script.display().to_string(), "{report}");
    assert_eq!(report["steps"][1]["status"], "passed", "{report}");
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            *name == format!("iu2u-act-{pid}") || name.starts_with(&format!("iu2u-browser-{pid}-"))
        })
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_terminate_signal_stops_the_run_and_its_services() {
    let service = temp_path("terminated.pid");
    let (code, alive) = signal_when_started(
        "-TERM",
        "terminated",
        &format!(
            "start idle = echo $$ > {} && exec sleep 300\nwait 30s\n",
            service.display()
        ),
        &[&service],
    );
    assert_eq!(code, Some(143));
    assert!(
        alive.is_empty(),
        "still running after the signal: {alive:?}"
    );
}

#[test]
fn a_hang_up_stops_the_run_and_its_services() {
    let service = temp_path("hung-up.pid");
    let (code, alive) = signal_when_started(
        "-HUP",
        "hung-up",
        &format!(
            "start idle = echo $$ > {} && exec sleep 300\nwait 30s\n",
            service.display()
        ),
        &[&service],
    );
    assert_eq!(code, Some(129));
    assert!(
        alive.is_empty(),
        "still running after the signal: {alive:?}"
    );
}

#[test]
fn a_hang_up_the_run_was_told_to_ignore_does_not_stop_it() {
    use std::os::unix::process::CommandExt;
    let service = temp_path("ignored-hang-up.pid");
    let script = temp_path("ignored-hang-up.e2e");
    std::fs::write(
        &script,
        format!(
            "start idle = echo $$ > {} && exec sleep 300\nwait 2s\n",
            service.display()
        ),
    )
    .unwrap();
    let mut command = command(None);
    command.arg(&script);
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut running = command.spawn().unwrap();
    while std::fs::read_to_string(&service).map_or(true, |pid| pid.trim().is_empty()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    Command::new("kill")
        .args(["-HUP", &running.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(running.wait().unwrap().code(), Some(0));
}

#[test]
fn an_interrupt_while_a_service_stops_still_kills_it() {
    let service = temp_path("stopping.pid");
    let (code, alive) = interrupt_when_started(
        "interrupted-stop",
        &format!(
            "start stubborn = sh -c 'trap \"\" TERM; sleep 2; sleep 77 & echo $! > {}; wait'\nwait 1s\n",
            service.display()
        ),
        &[&service],
    );
    assert_eq!(code, Some(130));
    assert!(
        alive.is_empty(),
        "still running after the interrupt: {alive:?}"
    );
}

#[test]
fn an_interrupt_stops_a_service_still_waiting_to_answer() {
    let child = temp_path("waiting.pid");
    let (code, alive) = interrupt_when_started(
        "interrupted-start",
        &format!(
            "start web = sleep 300 & echo $! > {}; wait at http://127.0.0.1:9/\n",
            child.display()
        ),
        &[&child],
    );
    assert_eq!(code, Some(130));
    assert!(
        alive.is_empty(),
        "still running after the interrupt: {alive:?}"
    );
}

#[test]
fn a_service_whose_children_ignore_the_stop_is_killed_when_the_script_ends() {
    let pid = temp_path("stubborn.pid");
    let script = temp_path("stubborn.e2e");
    std::fs::write(
        &script,
        format!(
            "start stubborn = sh -c 'trap \"\" TERM; echo $$ > {}; exec sleep 77'; echo done\nwait 1s\n",
            pid.display()
        ),
    )
    .unwrap();
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let pid = std::fs::read_to_string(&pid).unwrap().trim().to_owned();
    let alive = Command::new("kill")
        .args(["-0", &pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    if alive {
        Command::new("kill").args(["-9", &pid]).status().unwrap();
    }
    assert!(!alive, "the service still runs after the script");
}

#[test]
fn stubborn_services_are_stopped_together() {
    let script = script(
        "two-stubborn",
        "",
        "start first = sh -c 'trap \"\" TERM; exec sleep 77'; echo done\nstart second = sh -c 'trap \"\" TERM; exec sleep 78'; echo done\nwait 1s\n",
    );
    let started = std::time::Instant::now();
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(started.elapsed() < Duration::from_secs(9), "{report}");
}

#[test]
fn an_interrupt_stops_a_run_waiting_for_its_script_on_stdin() {
    let mut waiting = command(None)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut started = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(waiting.stdout.as_mut().unwrap()),
        &mut started,
    )
    .unwrap();
    Command::new("kill")
        .args(["-INT", &waiting.id().to_string()])
        .status()
        .unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = waiting.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > Duration::from_secs(5) {
            waiting.kill().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.and_then(|status| status.code()), Some(130));
}

#[tokio::test]
async fn a_service_whose_address_already_answers_does_not_start() {
    let busy = site(&[("/", "old server")]).await;
    let script = script(
        "taken-address",
        "",
        &format!("start web = sleep 30 at {}/\n", busy.uri()),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("already answers"),
        "{report}"
    );

    let bare = self::script(
        "bare-address",
        "",
        "with hub = localhost:5793\nstart web = sleep 30 at $hub\n",
    );
    let (output, report) = act(&bare, None, false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .ends_with("localhost:5793 is not an http or https address"),
        "{report}"
    );
}

#[test]
fn the_folder_for_logs_and_zips_is_private() {
    let script = script(
        "private",
        "",
        "run ls -ld \"${TMPDIR:-/tmp}\"/iu2u-act-$PPID\nsee \"drwx------\"\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn a_command_leaves_nothing_running_behind_it() {
    let pid = temp_path("left-behind.pid");
    let script = script(
        "left-behind",
        "",
        &format!("run sleep 300 & echo $! > {} within 5s\n", pid.display()),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][0]["seconds"].as_f64().unwrap() < 2.0,
        "{report}"
    );
    let pid = std::fs::read_to_string(&pid).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "sleep {} still runs", pid.trim());
}

#[test]
fn a_service_that_stops_before_it_answers_shows_what_it_said() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    for (command, said) in [
        (
            "true",
            "quiet stopped (exit status: 0) before http://127.0.0.1:PORT/ answered",
        ),
        (
            "echo boom; exit 3",
            "quiet stopped (exit status: 3) before http://127.0.0.1:PORT/ answered; quiet said:\nboom",
        ),
    ] {
        let script = script(
            "quiet",
            "",
            &format!("start quiet = {command} at http://127.0.0.1:{port}/\n"),
        );
        let (output, report) = act(&script, None, false);
        assert_eq!(output.status.code(), Some(2), "{report}");
        assert!(
            report["error"]
                .as_str()
                .unwrap()
                .ends_with(&said.replace("PORT", &port.to_string())),
            "{report}"
        );
    }
}

#[test]
fn each_request_opens_its_own_connection() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = temp_path("counting.py");
    std::fs::write(
        &server,
        "import socket, sys, threading\nport = int(sys.argv[1])\ncount = 0\ns = socket.socket()\ns.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind(('127.0.0.1', port))\ns.listen()\ndef serve(c):\n    while c.recv(65536):\n        body = b'connections %d' % count\n        c.sendall(b'HTTP/1.1 200 OK\\r\\ncontent-length: %d\\r\\n\\r\\n%s' % (len(body), body))\nwhile True:\n    c, _ = s.accept()\n    count += 1\n    threading.Thread(target=serve, args=(c,), daemon=True).start()\n",
    )
    .unwrap();
    let script = script(
        "connections",
        "",
        &format!(
            "start counting = python3 {} {port} at http://127.0.0.1:{port}/\nrequest GET http://127.0.0.1:{port}/\nsee \"connections 2\"\nrequest GET http://127.0.0.1:{port}/\nsee \"connections 3\"\n",
            server.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn see_within_keeps_asking_while_the_server_is_down_for_a_moment() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = temp_path("flaky.py");
    std::fs::write(
        &server,
        "import socket, sys, time\nport = int(sys.argv[1])\ndef serve(body, times):\n    s = socket.socket()\n    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n    s.bind(('127.0.0.1', port))\n    s.listen()\n    for _ in range(times):\n        c, _ = s.accept()\n        c.recv(65536)\n        c.sendall(b'HTTP/1.1 200 OK\\r\\ncontent-length: %d\\r\\nconnection: close\\r\\n\\r\\n%s' % (len(body), body))\n        c.close()\n    s.close()\nserve(b'starting', 2)\ntime.sleep(1.5)\nserve(b'ready', 100)\n",
    )
    .unwrap();
    let script = script(
        "flaky",
        "",
        &format!(
            "start flaky = python3 {} {port} at http://127.0.0.1:{port}/\nrequest GET http://127.0.0.1:{port}/\nsee \"starting\"\nsee \"ready\" within 10s\n",
            server.display()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn see_reads_the_page_address_but_not_where_links_point() {
    let site = site(&[(
        "/app",
        "<!doctype html><a href=\"/logout?next=/login\">Sign out</a>",
    )])
    .await;
    let script = script(
        "addresses",
        "",
        &format!(
            "open {uri}/app?tab=2\nsee no \"login\" within 0s\nsee \"{uri}/app\"\nsee \"tab=2\"\nsee no \"{uri}/logout\" within 0s\nsee no \"?next=/login\" within 0s\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_claim_on_a_response_replays_on_another_day() {
    let api = MockServer::start().await;
    let answer = |day: &str| {
        ResponseTemplate::new(200)
            .insert_header("date", format!("{day}, 12 Nov 2026 10:00:00 GMT"))
            .insert_header("x-request-id", format!("{day}-7f3a9c"))
            .set_body_string("{\"status\":\"ok\"}")
    };
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(answer("Thu"))
        .mount(&api)
        .await;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "dated",
        "",
        &format!(
            "request GET {}/health\nexpect the service is healthy\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    api.reset().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(answer("Fri"))
        .mount(&api)
        .await;
    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn a_broken_json_body_is_sent_as_written() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(body_string_contains("{\"id\": 4"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad json"))
        .mount(&api)
        .await;
    let script = script(
        "broken-json",
        "",
        &format!(
            "request POST {}/orders\n  json {{\"id\": 4\nsee \"HTTP 400\"\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn within_never_sends_a_post_again() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(500).set_body_string("busy"))
        .mount(&api)
        .await;
    let script = script(
        "post-within",
        "",
        &format!(
            "request POST {}/orders\nsee \"HTTP 201\" within 30s\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(api.received_requests().await.unwrap().len(), 1);
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 1.0,
        "{report}"
    );
}

#[tokio::test]
async fn a_claim_without_within_sends_its_request_once() {
    let api = MockServer::start().await;
    let created = |id: &'static str| {
        Mock::given(method("POST"))
            .and(path("/orders"))
            .respond_with(ResponseTemplate::new(201).set_body_string(format!("created {id}")))
    };
    created("first").up_to_n_times(1).mount(&api).await;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "post-once",
        "",
        &format!(
            "request POST {}/orders\nexpect the order was created\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    api.reset().await;
    created("second").mount(&api).await;
    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{report}");
    assert_eq!(api.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_locked_claim_within_asks_a_response_again() {
    let api = MockServer::start().await;
    let job = || async {
        Mock::given(method("GET"))
            .and(path("/job"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"status\":\"running\"}"))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path("/job"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"status\":\"finished\"}"))
            .mount(&api)
            .await;
    };
    job().await;
    let model = mock([
        ("finished", json!({ "holds": yes(0.95), "sight": yes(0.0) })),
        (
            "\"holds\"",
            json!({ "holds": yes(0.05), "sight": yes(0.0) }),
        ),
    ])
    .await;
    let script = script(
        "locked-poll",
        "",
        &format!(
            "request GET {}/job\nexpect the job is complete within 10s\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    api.reset().await;
    job().await;
    let (locked, report) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn see_within_asks_a_response_again_until_it_shows_the_text() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/build"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"status\":\"building\"}"))
        .up_to_n_times(3)
        .with_priority(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/build"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"status\":\"ok\"}"))
        .mount(&api)
        .await;
    let script = script(
        "poll-response",
        "",
        &format!(
            "request GET {}/build\nsee \"building\"\nsee \"ok\" within 10s\nsee no \"building\"\nsee \"never\" within 1s\n",
            api.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps[2]["status"], "passed", "{report}");
    assert_eq!(steps[4]["status"], "failed", "{report}");
    assert_eq!(api.received_requests().await.unwrap().len(), 5, "{report}");
}

#[tokio::test]
async fn a_response_asked_again_never_outlasts_within() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/build"))
        .respond_with(ResponseTemplate::new(200).set_body_string("building"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/build"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("ok")
                .set_delay(Duration::from_secs(20)),
        )
        .mount(&api)
        .await;
    let script = script(
        "slow-poll",
        "",
        &format!("request GET {}/build\nsee \"ok\" within 1s\n", api.uri()),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["seconds"].as_f64().unwrap() < 5.0,
        "{report}"
    );
    assert!(
        report["screen"].as_str().unwrap().contains("building"),
        "{report}"
    );
}

#[test]
fn requests_that_fail_show_on_the_screen() {
    let script = script(
        "failed",
        "<!doctype html><p>Gallery</p><img alt=\"Missing\" src=\"http://127.0.0.1:9/missing.png?at=1\"><img alt=\"Retried\" src=\"http://127.0.0.1:9/missing.png?at=1&retry=1\">",
        "open PAGE\nsee \"failed: http://127.0.0.1:9/missing.png\" within 5s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    assert_eq!(
        screen
            .matches("request \"failed: http://127.0.0.1:9/missing.png\"")
            .count(),
        1,
        "{screen}"
    );
}

#[tokio::test]
async fn failed_requests_go_with_the_page_that_made_them() {
    let site = site(&[
        ("/b", "<!doctype html><p>Page B</p>"),
        (
            "/a",
            "<!doctype html><p>Page A</p><img alt=\"Logo\" src=\"http://127.0.0.1:9/logo.png\">",
        ),
    ])
    .await;
    let script = script(
        "failures-go",
        "",
        &format!(
            "open {uri}/b\nopen {uri}/a\nsee \"failed: http://127.0.0.1:9/logo.png\" within 5s\nback\nsee \"Page B\"\nsee no \"failed:\" within 0s\n",
            uri = site.uri()
        ),
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn failed_requests_show_in_the_order_of_their_addresses() {
    let script = script(
        "failed-order",
        "<!doctype html><p>Gallery</p><img alt=\"Second\" src=\"http://127.0.0.1:9/second.png\"><script>setTimeout(() => document.body.insertAdjacentHTML('beforeend', '<img alt=\"First\" src=\"http://127.0.0.1:9/first.png\">'), 300);</script>",
        "open PAGE\nsee \"failed: http://127.0.0.1:9/first.png\" within 5s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let screen = report["screen"].as_str().unwrap();
    assert!(
        screen.find("failed: http://127.0.0.1:9/first.png")
            < screen.find("failed: http://127.0.0.1:9/second.png"),
        "{screen}"
    );
}

#[test]
fn a_text_that_see_no_saw_go_is_not_seen_again() {
    let script = script(
        "seen-gone",
        "<!doctype html><p id=\"item\">Item A</p><script>setTimeout(() => item.remove(), 1000)</script>",
        "open PAGE\nsee no \"Item A\" within 5s\nsee \"Item A\" within 0s\n",
    );
    let (output, report) = act(&script, None, false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["steps"][2]["status"], "failed", "{report}");
}

#[tokio::test]
async fn a_message_that_went_is_still_seen_after_a_long_wait() {
    let model = mock([
        ("Saved", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let script = script(
        "long-toast",
        "<!doctype html><button onclick=\"toast.textContent = 'Saved'; setTimeout(() => toast.textContent = '', 1000); setTimeout(() => out.textContent = 'Done', 7000)\">Save</button><p id=\"toast\" role=\"status\"></p><p id=\"out\"></p>",
        "open PAGE\ndo save\nsee \"Done\" within 15s\nsee \"Saved\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn see_counts_a_message_that_showed_and_went_since_the_last_action() {
    let model = mock_after(
        Duration::from_millis(1500),
        [
            ("Account split", decision("done", "001", "001")),
            ("", decision("press", "001", "001")),
        ],
    )
    .await;
    let script = script(
        "toast",
        "<!doctype html><button onclick=\"toast.textContent = 'Account split'; setTimeout(() => toast.textContent = '', 1000)\">Split</button><p id=\"toast\" role=\"status\"></p>",
        "open PAGE\ndo split the account\nsee \"Account split\"\nsee no \"Account split\"\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[test]
fn several_scripts_exit_with_the_worst_result() {
    let page = "<!doctype html><p>Hello</p>";
    let failing = script("failing", page, "open PAGE\nsee \"Goodbye\"\n");
    let passing = script("passing", page, "open PAGE\nsee \"Hello\"\n");
    let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["act", "--json"])
        .arg(&failing)
        .arg(&passing)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let reports: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let statuses: Vec<(&str, &str)> = reports
        .iter()
        .map(|report| {
            (
                report["script"].as_str().unwrap(),
                report["status"].as_str().unwrap(),
            )
        })
        .collect();
    let (failing, passing) = (failing.display().to_string(), passing.display().to_string());
    assert_eq!(
        statuses,
        [(failing.as_str(), "failed"), (passing.as_str(), "passed")]
    );
}

fn shown(scripts: &[&Path], model: Option<&MockServer>, colored: bool) -> (Output, String) {
    let mut command = command(model);
    command
        .args(scripts)
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    if colored {
        command.env("CLICOLOR_FORCE", "1");
    }
    let output = command.output().unwrap();
    let shown = String::from_utf8_lossy(&output.stdout).into_owned();
    (output, shown)
}

#[test]
fn the_results_show_each_step_the_failing_line_of_its_script_and_the_totals() {
    let page = "<!doctype html><p>Hello</p>";
    let failing = script(
        "readable-failing",
        page,
        "open PAGE\nsee \"Hello\"\nsee \"Goodbye\"\n\tsee \"Hello\"\n",
    );
    let passing = script("readable-passing", page, "open PAGE\nsee \"Hello\"\n");
    let broken = script("readable-broken", page, "bogus step\n");
    let (output, shown) = shown(&[&failing, &passing, &broken], None, false);
    assert_eq!(output.status.code(), Some(2), "{shown}");
    for expected in [
        format!("▶ {}\n", failing.display()),
        "  ✓ see \"Hello\" ".into(),
        "  ✗ see \"Goodbye\" ".into(),
        "    → \"Goodbye\" is not on the screen\n".into(),
        format!(" FAIL  {}:3 › see \"Goodbye\"\n", failing.display()),
        format!(
            "Screenshot: {}\n",
            failing.with_extension("e2e.failed.png").display()
        ),
        "  > 3 | see \"Goodbye\"\n    4 | \tsee \"Hello\"\n".into(),
        format!("✓ {} ", passing.display()),
        "  → line 1: unknown step \"bogus\"".into(),
        format!(
            " FAIL  {}\nline 1: unknown step \"bogus\"",
            broken.display()
        ),
        "    Scripts  2 failed | 1 passed (3)\n".into(),
        "      Steps  1 failed | 4 passed (5)\n".into(),
    ] {
        assert!(shown.contains(&expected), "{expected:?} not in:\n{shown}");
    }
    assert!(!shown.contains('\x1b'), "{shown}");
}

#[tokio::test]
async fn the_results_say_how_much_the_model_saw_for_a_claim_that_holds() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let (output, shown) = shown(&[&long_claim("long-readable")], Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(
        shown.contains("  ✓ expect the cart total is 5 dollars")
            && shown.contains(
                "    → the model is 95% sure it holds; the model saw only the first 6000 characters"
            ),
        "{shown}"
    );
}

#[tokio::test]
async fn the_results_mark_an_unsure_claim_and_say_what_a_pass_needs() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.74996), "sight": yes(0.0) })).await;
    let script = script(
        "unsure",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page greets the visitor\n",
    );
    let (output, shown) = shown(&[&script], Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{shown}");
    for expected in [
        "  ? expect the page greets the visitor ",
        "    → the model is unsure: 74% sure it holds, a pass needs 75%\n",
        "      Steps  1 unsure | 1 passed (2)\n",
        "    Scripts  1 failed (1)\n",
    ] {
        assert!(shown.contains(expected), "{expected:?} not in:\n{shown}");
    }
}

#[tokio::test]
async fn a_model_request_that_times_out_is_sent_again() {
    let model = MockServer::start().await;
    let holds = json!({ "holds": yes(0.95), "sight": yes(0.0) });
    Mock::given(method("POST"))
        .respond_with(reply(holds.clone()).set_delay(Duration::from_secs(3)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&model)
        .await;
    answer_always(&model, holds).await;
    let script = script(
        "retried",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page says hello\n",
    );
    let output = command(Some(&model))
        .arg("--json")
        .arg(&script)
        .env("IU2U_TIMEOUT_MS", "1000")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 2, "{report}");
}

#[tokio::test]
async fn a_model_request_answered_with_a_server_error_is_sent_again() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("usage limit"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&model)
        .await;
    answer_always(&model, json!({ "holds": yes(0.95), "sight": yes(0.0) })).await;
    let script = script(
        "retried-502",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page says hello\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["model_calls"], 2, "{report}");
}

#[tokio::test]
async fn the_results_escape_control_characters_from_the_model_server() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_string("bad \u{1b}]0;owned\u{7}"))
        .mount(&model)
        .await;
    let script = script(
        "escaped",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page says hello\n",
    );
    let (output, shown) = shown(&[&script], Some(&model), true);
    assert_eq!(output.status.code(), Some(2), "{shown}");
    assert!(
        shown.contains("bad \\u{1b}]0;owned\\u{7}") && !shown.contains('\u{7}'),
        "{shown}"
    );
}

#[tokio::test]
async fn the_json_report_keeps_every_character_the_model_server_sent() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_string("bad \u{7f} body"))
        .mount(&model)
        .await;
    let script = script(
        "raw",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page says hello\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("bad \u{7f} body"),
        "{report}"
    );
    assert_eq!(model.received_requests().await.unwrap().len(), 1);
}

#[test]
fn the_results_are_colored_when_colors_are_forced() {
    let script = script(
        "colored",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nsee \"Hello\"\n",
    );
    let (output, shown) = shown(&[&script], None, true);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("\x1b[32m✓"), "{shown}");
}

#[tokio::test]
async fn one_of_several_identical_controls_replays_locked() {
    let model = mock([
        ("pressed", decision("done", "001", "001")),
        ("", decision("press", "001", "003")),
    ])
    .await;
    let script = script(
        "twins",
        &todos(&["Milk", "Eggs", "Bread"]),
        "open PAGE\ndo delete the milk\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0],
        "pressed button \"Delete\" · in \"Todos\" · 1 of 3"
    );

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["browser", "replay"]);
    let screen = replayed["screen"].as_str().unwrap();
    assert!(
        screen.contains("Eggs") && !screen.contains("Milk"),
        "{screen}"
    );
}

#[test]
fn a_step_missing_from_the_lock_fails_a_locked_run_and_says_so() {
    let script = script(
        "unlocked",
        "<!doctype html><p>Hello</p>",
        "open PAGE\nexpect the page says hello\n",
    );
    let (output, report) = act(&script, None, true);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("not in the lock file")
    );
}

#[tokio::test]
async fn act_waits_out_a_screen_that_is_still_loading_and_replays_it_locked() {
    let model = mock_after(
        Duration::from_millis(400),
        [
            ("Finished", decision("done", "001", "001")),
            ("Loading", decision("wait", "001", "001")),
            ("", decision("press", "001", "001")),
        ],
    )
    .await;
    let page = r#"<!doctype html><button onclick="out.textContent = 'Loading'; setTimeout(() => { out.textContent = 'Finished'; setTimeout(() => { out.textContent = 'Expired'; }, 4000); }, 8000)">Save</button><p id="out"></p>"#;
    let script = script("slow", page, "open PAGE\ndo save the draft\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let actions = report["steps"][1]["actions"].as_array().unwrap();
    assert!(actions.iter().filter(|action| *action == "waited").count() >= 2);

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["browser", "replay"]);

    edit_page(&script, "}, 8000)", "}, 0)");
    let (fast, replayed) = act(&script, None, true);
    assert_eq!(fast.status.code(), Some(0), "{replayed}");
}

#[tokio::test]
async fn waiting_does_not_use_up_the_actions_of_a_goal() {
    let model = mock([
        ("Finished", decision("done", "001", "001")),
        ("Loading", decision("wait", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><button onclick="let n = 0; out.textContent = 'Loading 0'; const tick = setInterval(() => { n += 1; out.textContent = n < 24 ? 'Loading ' + n : 'Finished'; if (n >= 24) clearInterval(tick); }, 600);">Export</button><p id="out"></p>"#;
    let script = script("export", page, "open PAGE\ndo export the report\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let waits = report["steps"][1]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| *action == "waited")
        .count();
    assert!(waits >= 15, "{report}");

    edit_page(&script, "'Finished'", "'Broken'");
    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(1), "{replayed}");
    assert!(replayed["seconds"].as_f64().unwrap() < 60.0, "{replayed}");
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(lock.contains("\"ms\""), "{lock}");
}

#[tokio::test]
async fn a_progress_display_that_never_stops_changing_is_waited_out() {
    let model = mock([
        ("Exported", decision("done", "001", "001")),
        ("%", decision("wait", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><button onclick="let n = 0; const tick = setInterval(() => { n += 1; out.textContent = n < 60 ? n + '%' : 'Exported'; if (n >= 60) clearInterval(tick); }, 100);">Export</button><p id="out"></p>"#;
    let script = script("progress", page, "open PAGE\ndo export the report\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(report["model_calls"].as_u64().unwrap() < 10, "{report}");
}

#[tokio::test]
async fn an_error_mid_script_keeps_what_was_already_done() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(reply(decision("type", "001", "001")))
        .up_to_n_times(3)
        .mount(&model)
        .await;
    common::fail_always(&model, 503).await;
    let (output, report) = act(&shop("midway"), Some(&model), false);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert_eq!(report["status"], "error");
    assert_eq!(report["steps"][0]["status"], "passed");
    assert_eq!(report["steps"][1]["status"], "error");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .starts_with("typed \"qa@example.com\""),
        "{report}"
    );
    assert!(
        report["error"].as_str().unwrap().contains("line 3"),
        "{report}"
    );
    assert!(
        report["error"].as_str().unwrap().ends_with(&format!(
            ": {}",
            report["steps"][1]["detail"].as_str().unwrap()
        )),
        "{report}"
    );
}

#[tokio::test]
async fn a_locked_replay_waits_for_a_control_that_shows_up_late() {
    let model = mock_after(
        Duration::from_millis(700),
        [
            ("Published", decision("done", "001", "001")),
            ("Publish", decision("press", "002", "001")),
            ("pressed", decision("wait", "001", "001")),
            ("", decision("press", "001", "001")),
        ],
    )
    .await;
    let page = r#"<!doctype html><button onclick="setTimeout(() => { publish.hidden = false; }, 1500)">Save</button><button id="publish" hidden onclick="out.textContent = 'Published'">Publish</button><p id="out"></p>"#;
    let script = script("late", page, "open PAGE\ndo save and then publish\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["browser", "replay"]);

    let (unlocked, replayed) = act(&script, Some(&model), false);
    assert_eq!(unlocked.status.code(), Some(0), "{replayed}");
    assert_eq!(replayed["model_calls"], 0, "{replayed}");
}

async fn stopped(name: &str, page: &str, model: &MockServer) -> String {
    let script = script(name, page, "open PAGE\ndo reach the goal\n");
    let (output, report) = act(&script, Some(model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    report["steps"][1]["detail"].as_str().unwrap().into()
}

#[tokio::test]
async fn a_step_stops_when_the_model_repeats_itself_gives_up_or_stays_unsure() {
    let mut unsure = decision("press", "001", "001");
    unsure["operation"] = json!({ "type": "choice", "choice": "press", "confidence": 0.4, "probabilities": { "press": 0.4, "done": 0.35, "none": 0.25 } });
    for (name, answers, detail) in [
        ("repeat", decision("press", "001", "001"), "would repeat"),
        ("stuck", decision("stuck", "001", "001"), "sees no way"),
        ("unsure", unsure, "unsure which operation"),
    ] {
        let model = mock([("", answers)]).await;
        let detail_seen = stopped(name, "<!doctype html><button>Noop</button>", &model).await;
        assert!(detail_seen.contains(detail), "{detail_seen}");
    }
}

#[tokio::test]
async fn a_step_stops_after_too_many_actions() {
    let model = mock([("", decision("press", "001", "001"))]).await;
    let page = r#"<!doctype html><button onclick="this.textContent = 'Count ' + ++count">Count 0</button><script>let count = 0;</script>"#;
    let detail = stopped("many", page, &model).await;
    assert!(detail.contains("more than 15 actions"), "{detail}");
}

#[tokio::test]
async fn a_goal_may_take_exactly_the_most_actions() {
    let model = mock([
        ("Count 15", decision("done", "001", "001")),
        ("", decision("press", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><button onclick="this.textContent = 'Count ' + ++count">Count 0</button><script>let count = 0;</script>"#;
    let script = script("fifteen", page, "open PAGE\ndo count to fifteen\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["steps"][1]["actions"].as_array().unwrap().len(), 15);
}

#[test]
fn locked_needs_script_files() {
    let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["act", "--locked"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("SCRIPTS"));
}

#[tokio::test]
async fn a_failed_step_on_a_crowded_screen_says_which_controls_were_left_out() {
    let model = mock([("", decision("stuck", "001", "001"))]).await;
    let links: String = (0..260)
        .map(|index| format!("<a href=\"#{index}\">Link {index}</a>"))
        .collect();
    let page = format!("<!doctype html>{links}<button>Pay</button>");
    let detail = stopped("crowded", &page, &model).await;
    assert!(
        detail.contains("7 controls after the first 254 were not offered"),
        "{detail}"
    );
}

#[tokio::test]
async fn a_failed_step_on_a_long_screen_says_how_much_the_model_saw() {
    let model = mock([("", decision("stuck", "001", "001"))]).await;
    let items: String = (1..=400)
        .map(|item| format!("<p>Item {item} costs ${}</p>", 1000 - item))
        .collect();
    let page = format!("<!doctype html>{items}<button>Buy</button>");
    let detail = stopped("catalog", &page, &model).await;
    assert!(
        detail.contains("the model saw only the first 6000 characters"),
        "{detail}"
    );
    let asked = model.received_requests().await.unwrap();
    assert!(String::from_utf8_lossy(&asked[0].body).contains("The screen goes on"));
}

#[tokio::test]
async fn a_step_stops_after_too_many_decisions() {
    let model = MockServer::start().await;
    for index in 0..30 {
        let (forward, reversed) = (format!("{:03}", index + 1), format!("{:03}", 30 - index));
        Mock::given(method("POST"))
            .respond_with(reply(decision("press", &forward, &reversed)))
            .up_to_n_times(3)
            .mount(&model)
            .await;
    }
    let buttons: String = (0..30)
        .map(|index| format!("<button>Pay {index}</button>"))
        .collect();
    let page = format!(r#"<!doctype html>{buttons}<div style="position: fixed; inset: 0"></div>"#);
    let detail = stopped("decisions", &page, &model).await;
    assert!(detail.contains("not reached in 30 decisions"), "{detail}");
}

#[tokio::test]
async fn scrolling_a_page_that_cannot_scroll_moves_the_box_that_can() {
    let model = mock([
        ("Box end", decision("done", "001", "001")),
        ("", decision("scroll", "001", "001")),
    ])
    .await;
    let terms: String = (0..60)
        .map(|index| format!("<p>Term {index}</p>"))
        .collect();
    let page = format!(
        r#"<!doctype html><body style="overflow: hidden; margin: 0"><div style="height: 3000px">Page</div><div id="terms" style="position: fixed; top: 0; left: 0; width: 220px; height: 300px; overflow-y: auto">{terms}</div><p id="out" style="position: fixed; top: 0; right: 0"></p><script>terms.onscroll = () => {{ if (terms.scrollTop + terms.clientHeight >= terms.scrollHeight - 1) out.textContent = 'Box end'; }};</script></body>"#
    );
    let script = script(
        "locked-page",
        &page,
        "open PAGE\ndo read the terms to the end\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
}

#[tokio::test]
async fn scrolling_moves_the_page_rather_than_a_side_box() {
    let model = mock([
        ("Page scrolled", decision("done", "001", "001")),
        ("", decision("scroll", "001", "001")),
    ])
    .await;
    let links: String = (0..100)
        .map(|index| format!("<p>Chapter {index}</p>"))
        .collect();
    let page = format!(
        r#"<!doctype html><div style="height: 3000px; margin-left: 220px">Article</div><nav id="side" style="position: fixed; top: 0; left: 0; width: 200px; height: 100vh; overflow-y: auto">{links}</nav><p id="out" style="position: fixed; top: 0; right: 0"></p><p id="moved" style="position: fixed; bottom: 0; right: 0"></p><script>side.onscroll = () => moved.textContent = 'Side box moved'; onscroll = () => out.textContent = 'Page scrolled';</script>"#
    );
    let script = script(
        "scroll-page",
        &page,
        "open PAGE\ndo read further down the article\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        !report["screen"]
            .as_str()
            .unwrap()
            .contains("Side box moved"),
        "{report}"
    );
}

#[tokio::test]
async fn scrolling_moves_the_open_list_rather_than_the_page() {
    let model = mock([
        ("List end", decision("done", "001", "001")),
        ("", decision("scroll", "001", "001")),
    ])
    .await;
    let options: String = (0..100)
        .map(|index| format!("<div role=\"option\">Issue {index}</div>"))
        .collect();
    let page = format!(
        r#"<!doctype html><div style="height: 3000px">Form</div><div id="list" role="listbox" style="position: fixed; top: 10px; left: 10px; width: 200px; height: 120px; overflow: auto">{options}</div><p id="out" style="position: fixed; top: 0; right: 0"></p><script>list.onscroll = () => {{ if (list.scrollTop + list.clientHeight >= list.scrollHeight - 1) out.textContent = 'List end'; }}; onscroll = () => out.textContent = 'Page scrolled';</script>"#
    );
    let script = script(
        "scroll",
        &page,
        "open PAGE\ndo show the last issue of the list\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let actions = report["steps"][1]["actions"].as_array().unwrap();
    assert!(actions.len() > 1, "{report}");
    assert!(
        actions.iter().all(|action| action == "scrolled down"),
        "{report}"
    );
}

#[tokio::test]
async fn scrolling_moves_a_frame_that_fills_the_page() {
    let model = mock([
        ("Frame scrolled", decision("done", "001", "001")),
        ("", decision("scroll", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><body style="margin: 0"><iframe style="display: block; width: 100%; height: 100vh; border: 0" srcdoc="<p id='out'>Top</p><div style='height: 5000px'></div><script>onscroll = () => out.textContent = 'Frame scrolled'</script>"></iframe></body>"#;
    let script = script(
        "frame-scroll",
        page,
        "open PAGE\ndo scroll down the document\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");

    let short = self::script(
        "short-frame",
        r#"<!doctype html><body style="margin: 0"><iframe style="display: block; width: 100%; height: 100vh; border: 0" srcdoc="<p>Nothing more</p>"></iframe></body>"#,
        "open PAGE\ndo scroll down the document\n",
    );
    let (output, report) = act(&short, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("unchanged screen"),
        "{report}"
    );
}

#[tokio::test]
async fn scrolling_a_frame_from_another_site_stops_on_the_repeat() {
    let model = mock([("", decision("scroll", "001", "001"))]).await;
    let site = site(&[("/frame", "<!doctype html><p>Nothing more</p>")]).await;
    let port = site.address().port();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("<!doctype html><body style=\"margin: 0\"><iframe style=\"display: block; width: 100%; height: 100vh; border: 0\" src=\"http://localhost:{port}/frame\"></iframe></body>"),
            "text/html",
        ))
        .mount(&site)
        .await;
    let script = script(
        "cross-frame-scroll",
        "",
        &format!("open http://127.0.0.1:{port}/app\ndo scroll down the document\n"),
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "it would repeat this on an unchanged screen: scrolled down",
        "{report}"
    );
}

#[tokio::test]
async fn scrolling_with_nothing_left_to_scroll_stops_on_the_repeat() {
    let model = mock([("", decision("scroll", "001", "001"))]).await;
    let script = script(
        "unscrollable",
        "<!doctype html><p>Short page</p>",
        "open PAGE\ndo show more of the page\n",
    );
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(
        report["steps"][1]["detail"], "it would repeat this on an unchanged screen: scrolled down",
        "{report}"
    );
}

#[tokio::test]
async fn escape_closes_an_open_list_and_replays() {
    let model = mock([
        ("Profile", decision("done", "001", "001")),
        ("", decision("escape", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><div id="list" role="listbox" tabindex="-1"><div role="option">Campus 2026</div></div><script>list.focus(); list.addEventListener('keydown', event => { if (event.key === 'Escape') list.outerHTML = '<button>Profile</button>'; });</script>"#;
    let script = script("escape", page, "open PAGE\ndo close the open list\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(
        report["steps"][1]["actions"][0], "pressed Escape",
        "{report}"
    );

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert!(
        replayed["screen"].as_str().unwrap().contains("Profile"),
        "{replayed}"
    );
}

#[tokio::test]
async fn a_replayed_enter_still_waits_for_the_page_to_be_ready() {
    let model = mock([
        ("Found", decision("done", "001", "001")),
        ("Ready", decision("enter", "001", "001")),
        ("typed", decision("wait", "001", "001")),
        ("", decision("type", "001", "001")),
    ])
    .await;
    let page = r#"<!doctype html><label>Search <input id="query" oninput="clearTimeout(window.ready); out.textContent = 'Wait'; window.ready = setTimeout(() => { out.textContent = 'Ready'; window.readyAt = performance.now(); }, 2000);" onkeydown="if (event.key === 'Enter') { out.textContent = out.textContent !== 'Ready' ? 'Too early' : performance.now() - window.readyAt < 4000 ? 'Found' : 'Too late'; }"></label><p id="out"></p>"#;
    let script = script("ready", page, "open PAGE\ndo search for \"shirts\"\n");
    let (output, report) = act(&script, Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{report}");

    let (locked, replayed) = act(&script, None, true);
    assert_eq!(locked.status.code(), Some(0), "{replayed}");
    assert!(
        replayed["screen"].as_str().unwrap().contains("Found"),
        "{replayed}"
    );

    edit_page(&script, "}, 2000)", "}, 0)");
    let (fast, replayed) = act(&script, None, true);
    assert_eq!(fast.status.code(), Some(0), "{replayed}");
}

#[tokio::test]
async fn act_chooses_an_option_showing_a_secret_and_replays_the_choice() {
    let model = mock([
        ("chose", decision("done", "001", "001")),
        ("", {
            let mut large = decision("choose", "001", "001");
            large["choose_001"] = choice("002");
            large["choose_001_reversed"] = choice("001");
            large
        }),
    ])
    .await;
    let page = r#"<!doctype html><label>Size <select id="size"><option value="one">Small</option><option value="one">&nbsp;&nbsp;Large for ACCOUNT</option></select></label><p id="out"></p><script>size.onchange = () => { out.textContent = 'Picked ' + size.selectedOptions[0].text; };</script>"#;
    let script = script(
        "choose",
        &page.replace("ACCOUNT", SECRET),
        "open PAGE\nwith account = $IU2U_TEST_SECRET\ndo pick the large size\nsee \"Picked Large\"\n",
    );
    let (first, report) = act(&script, Some(&model), false);
    assert_eq!(first.status.code(), Some(0), "{report}");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .starts_with(r#"chose "Large for (secret \"account\")" in combobox "Size""#),
        "{report}"
    );
    let lock = std::fs::read_to_string(format!("{}.lock", script.display())).unwrap();
    assert!(!report.to_string().contains("7f3a") && !lock.contains("7f3a"));

    let (second, replayed) = act(&script, None, true);
    assert_eq!(second.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["browser", "replay", "check"]);
    assert!(!replayed["screen"].as_str().unwrap().contains("option"));
}

#[cfg(target_os = "macos")]
const FIXTURE: &str = r#"import AppKit
final class Locked: NSView {
    override var intrinsicContentSize: NSSize { NSSize(width: 120, height: 22) }
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .textField }
    override func accessibilityLabel() -> String? { "Locked" }
}
final class Stubborn: NSTextField {
    override func isAccessibilitySelectorAllowed(_ selector: Selector) -> Bool {
        selector != #selector(NSView.setAccessibilityValue(_:)) && super.isAccessibilitySelectorAllowed(selector)
    }
    override func setAccessibilityValue(_ value: Any?) {}
    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        currentEditor()?.selectedRange = NSRange(location: (stringValue as NSString).length, length: 0)
        return became
    }
}
final class App: NSApplication {
    var manual = true
    override func accessibilityAttributeNames() -> [NSAccessibility.Attribute] {
        super.accessibilityAttributeNames() + [NSAccessibility.Attribute(rawValue: "AXManualAccessibility")]
    }
    override func accessibilityAttributeValue(_ attribute: NSAccessibility.Attribute) -> Any? {
        attribute.rawValue == "AXManualAccessibility" ? manual : super.accessibilityAttributeValue(attribute)
    }
    override func accessibilitySetValue(_ value: Any?, forAttribute attribute: NSAccessibility.Attribute) {
        guard attribute.rawValue == "AXManualAccessibility" else { return super.accessibilitySetValue(value, forAttribute: attribute) }
        manual = (value as? Bool) ?? false
        let log = URL(fileURLWithPath: Bundle.main.bundlePath).deletingLastPathComponent().appendingPathComponent("manual.log")
        try? "\(manual)".write(to: log, atomically: true, encoding: .utf8)
    }
}
final class Reject: Formatter {
    override func string(for obj: Any?) -> String? { obj as? String }
    override func getObjectValue(_ obj: AutoreleasingUnsafeMutablePointer<AnyObject?>?, for string: String, errorDescription error: AutoreleasingUnsafeMutablePointer<NSString?>?) -> Bool {
        obj?.pointee = string as NSString
        return true
    }
    override func isPartialStringValid(_ partialString: String, newEditingString newString: AutoreleasingUnsafeMutablePointer<NSString?>?, errorDescription error: AutoreleasingUnsafeMutablePointer<NSString?>?) -> Bool { false }
}
final class Upper: NSTextField {
    var kept = ""
    override var stringValue: String {
        get { super.stringValue }
        set {
            if kept.isEmpty { kept = newValue.uppercased() }
            super.stringValue = kept
        }
    }
    override func textDidChange(_ notification: Notification) {
        if kept.isEmpty { kept = currentEditor()?.string.uppercased() ?? "" }
        currentEditor()?.string = kept
        super.textDidChange(notification)
    }
}
final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    let echo = NSTextField(labelWithString: "echo")
    let frozen = Stubborn(string: "")
    let upper = Upper(string: "")
    let field = NSTextField(string: "")
    let code = Stubborn(string: "old")
    let codeEcho = NSTextField(labelWithString: "code")
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 360, height: 320), styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "Fixture"
        field.setAccessibilityLabel("Name")
        field.target = self
        field.action = #selector(submitted)
        code.setAccessibilityLabel("Code")
        code.target = self
        code.action = #selector(coded)
        frozen.setAccessibilityLabel("Frozen")
        frozen.formatter = Reject()
        upper.setAccessibilityLabel("Coupon")
        let stack = NSStackView(views: [field, Locked(), code, frozen, upper, echo, codeEcho])
        stack.orientation = .vertical
        stack.frame = NSRect(x: 20, y: 20, width: 320, height: 280)
        window.contentView!.addSubview(stack)
        window.makeKeyAndOrderFront(nil)
        DispatchQueue.main.asyncAfter(deadline: .now() + 120) { NSApp.terminate(nil) }
    }
    @objc func submitted() { echo.stringValue = "echo \(field.stringValue)" }
    @objc func coded() { codeEcho.stringValue = "code \(code.stringValue)" }
}
let app = App.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
"#;

#[cfg(target_os = "macos")]
struct Quit(std::path::PathBuf);

#[cfg(target_os = "macos")]
fn mac_app(name: &str, source: &str) -> (std::path::PathBuf, Quit) {
    let bundle = temp_path(&format!("{name}.app"));
    let executable = bundle.join("Contents/MacOS").join(name);
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"), format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>{name}</string><key>CFBundleIdentifier</key><string>dev.iu2u.{name}</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>")).unwrap();
    swift(source, &executable);
    let bundle = std::fs::canonicalize(bundle).unwrap();
    (bundle.clone(), Quit(bundle))
}

#[cfg(target_os = "macos")]
fn swift(source: &str, executable: &Path) {
    let file = executable.with_extension("swift");
    std::fs::write(&file, source).unwrap();
    let built = Command::new("swiftc")
        .arg(&file)
        .arg("-o")
        .arg(executable)
        .status()
        .unwrap();
    assert!(built.success());
}

#[cfg(target_os = "macos")]
const WINDOWLESS: &str = r#"import AppKit
final class App: NSApplication {
    var manual = false
    override func accessibilityAttributeNames() -> [NSAccessibility.Attribute] {
        super.accessibilityAttributeNames() + [NSAccessibility.Attribute(rawValue: "AXManualAccessibility")]
    }
    override func accessibilityAttributeValue(_ attribute: NSAccessibility.Attribute) -> Any? {
        attribute.rawValue == "AXManualAccessibility" ? manual : super.accessibilityAttributeValue(attribute)
    }
    override func accessibilitySetValue(_ value: Any?, forAttribute attribute: NSAccessibility.Attribute) {
        guard attribute.rawValue == "AXManualAccessibility" else { return super.accessibilitySetValue(value, forAttribute: attribute) }
        manual = (value as? Bool) ?? false
        let log = URL(fileURLWithPath: Bundle.main.bundlePath).deletingLastPathComponent().appendingPathComponent("windowless.log")
        try? "\(manual)".write(to: log, atomically: true, encoding: .utf8)
    }
}
let app = App.shared
app.setActivationPolicy(.regular)
DispatchQueue.main.asyncAfter(deadline: .now() + 60) { NSApp.terminate(nil) }
app.run()
"#;

#[cfg(target_os = "macos")]
#[test]
fn a_mac_app_with_no_window_gets_its_accessibility_switch_back() {
    let (bundle, _quit) = mac_app("Windowless", WINDOWLESS);
    let script = temp_path("windowless.e2e");
    std::fs::write(
        &script,
        format!("app {}\nsee \"never\"\n", bundle.display()),
    )
    .unwrap();
    let (output, report) = act(&script, None, false);
    let error = report["error"].as_str().unwrap_or_default();
    if error.contains("Privacy & Security") {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(error.contains("showed no window"), "{report}");
    let manual = std::fs::read_to_string(bundle.with_file_name("windowless.log")).unwrap();
    assert_eq!(manual, "false");
}

#[cfg(target_os = "macos")]
#[test]
fn a_mac_app_stopped_while_it_starts_gets_its_accessibility_switch_back() {
    let source = WINDOWLESS.replace("windowless.log", "starting.log");
    let (bundle, _quit) = mac_app("Starting", &source);
    let log = bundle.with_file_name("starting.log");
    let script = temp_path("starting-app.e2e");
    std::fs::write(&script, format!("app {}\n", bundle.display())).unwrap();
    let mut running = command(None).arg(&script).spawn().unwrap();
    while std::fs::read_to_string(&log).ok().as_deref() != Some("true") {
        if running.try_wait().unwrap().is_some() {
            eprintln!("skipped: this process may not use the accessibility API");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Command::new("kill")
        .args(["-TERM", &running.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(running.wait().unwrap().code(), Some(143));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "false");
}

#[cfg(target_os = "macos")]
#[test]
fn an_interrupted_mac_app_gets_its_accessibility_switch_back() {
    let source = FIXTURE
        .replace("var manual = true", "var manual = false")
        .replace("\"manual.log\"", "\"interrupted.log\"")
        .replace(
            "manual = (value as? Bool) ?? false",
            "manual = (value as? Bool) ?? false; if !manual { Thread.sleep(forTimeInterval: 0.5) }",
        );
    let (bundle, _quit) = mac_app("Interrupted", &source);
    let log = bundle.with_file_name("interrupted.log");
    let script = temp_path("interrupted-app.e2e");
    std::fs::write(&script, format!("app {}\nwait 30s\n", bundle.display())).unwrap();
    let mut running = command(None).arg(&script).spawn().unwrap();
    while std::fs::read_to_string(&log).ok().as_deref() != Some("true") {
        if running.try_wait().unwrap().is_some() {
            eprintln!("skipped: this process may not use the accessibility API");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Command::new("kill")
        .args(["-INT", &running.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(running.wait().unwrap().code(), Some(130));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "false");
}

#[cfg(target_os = "macos")]
#[test]
fn a_mac_app_is_driven_past_a_helper_that_started_after_it() {
    let once_the_helper_is_asked = r#"Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { timer in
            let asked = URL(fileURLWithPath: Bundle.main.bundlePath).deletingLastPathComponent().appendingPathComponent("helper.log")
            guard FileManager.default.fileExists(atPath: asked.path) else { return }
            timer.invalidate()
            self.window.makeKeyAndOrderFront(nil)
        }"#;
    let source = FIXTURE
        .replace("manual.log", "helped.log")
        .replace("window.makeKeyAndOrderFront(nil)", once_the_helper_is_asked);
    let (bundle, _quit) = mac_app("Helped", &source);
    let helper = bundle.join("Contents/MacOS/Helper");
    swift(&WINDOWLESS.replace("windowless.log", "helper.log"), &helper);
    let opened = Command::new("open")
        .arg("-g")
        .arg(&bundle)
        .status()
        .unwrap();
    assert!(opened.success());
    let mut helper = Command::new(helper).spawn().unwrap();
    let script = temp_path("helped.e2e");
    std::fs::write(&script, format!("app {}\nsee \"echo\"\n", bundle.display())).unwrap();
    let (output, report) = act(&script, None, false);
    helper.kill().ok();
    helper.wait().ok();
    let error = report["error"].as_str().unwrap_or_default();
    if error.contains("Privacy & Security") {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(output.status.code(), Some(0), "{report}");
    let manual = std::fs::read_to_string(bundle.with_file_name("helper.log")).unwrap();
    assert_eq!(manual, "false");
}

#[cfg(target_os = "macos")]
const SLOW: &str = r#"import AppKit
final class App: NSApplication {
    var answered = false
    override func accessibilityAttributeValue(_ attribute: NSAccessibility.Attribute) -> Any? {
        if attribute == .children && !answered {
            answered = true
            usleep(1_500_000)
        }
        return super.accessibilityAttributeValue(attribute)
    }
}
final class Label: NSView {
    let text: String
    init(_ text: String) { self.text = text; super.init(frame: .zero) }
    required init?(coder: NSCoder) { nil }
    override var intrinsicContentSize: NSSize { NSSize(width: 120, height: 16) }
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? {
        usleep(12_000)
        return .staticText
    }
    override func accessibilityValue() -> Any? { text }
}
final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 200, y: 100, width: 200, height: 900), styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "Labels"
        let stack = NSStackView(views: (1...40).map { Label("Label \($0)") })
        stack.orientation = .vertical
        stack.frame = NSRect(x: 20, y: 20, width: 160, height: 860)
        window.contentView!.addSubview(stack)
        window.makeKeyAndOrderFront(nil)
        DispatchQueue.main.asyncAfter(deadline: .now() + 60) { NSApp.terminate(nil) }
    }
}
let app = App.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
"#;

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_mac_app_slow_to_answer_is_still_read_whole() {
    let (bundle, _quit) = mac_app("Slow", SLOW);
    let model = mock([("", decision("done", "001", "001"))]).await;
    let script = temp_path("slow-app.e2e");
    std::fs::write(
        &script,
        format!("app {}\ndo read the labels\n", bundle.display()),
    )
    .unwrap();
    let (output, report) = act(&script, Some(&model), false);
    let error = report["error"].as_str().unwrap_or_default();
    if error.contains("Privacy & Security") {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(output.status.code(), Some(0), "{report}");
    let asked = model.received_requests().await.unwrap();
    let seen = String::from_utf8_lossy(&asked[0].body);
    assert!(
        seen.contains("Label 40") && !seen.contains("was not read"),
        "{seen}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_mac_app_that_quits_is_an_error_at_once() {
    let source = BUSY
        .replace("\"Busy\"", "\"Gone\"")
        .replace("sleep(60)", "NSApp.terminate(nil)");
    let (bundle, _quit) = mac_app("Gone", &source);
    let script = temp_path("gone.e2e");
    std::fs::write(
        &script,
        format!("app {}\nsee \"never\"\n", bundle.display()),
    )
    .unwrap();
    let (output, report) = act(&script, None, false);
    let error = report["error"].as_str().unwrap_or_default();
    if error.contains("Privacy & Security") {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(error.contains("did not answer"), "{report}");
    assert!(report["seconds"].as_f64().unwrap() < 2.5, "{report}");
}

#[cfg(target_os = "macos")]
const BUSY: &str = r#"import AppKit
final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 200, height: 100), styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "Busy"
        window.makeKeyAndOrderFront(nil)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { sleep(60) }
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
"#;

#[cfg(target_os = "macos")]
#[test]
fn a_mac_app_that_stops_answering_is_an_error_not_an_empty_screen() {
    let (bundle, _quit) = mac_app("Busy", BUSY);
    let script = temp_path("busy.e2e");
    std::fs::write(
        &script,
        format!("app {}\nsee \"never\"\n", bundle.display()),
    )
    .unwrap();
    let (output, report) = act(&script, None, false);
    let error = report["error"].as_str().unwrap_or_default();
    if error.contains("Privacy & Security") {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        error.contains("did not answer") || error.contains("showed no window"),
        "{report}"
    );
}

#[cfg(target_os = "macos")]
impl Drop for Quit {
    fn drop(&mut self) {
        Command::new("pkill").arg("-f").arg(&self.0).status().ok();
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn act_drives_a_mac_app_through_its_accessibility_tree() {
    let (bundle, _quit) = mac_app("Fixture", FIXTURE);
    let model = mock([
        (
            "\"holds\"",
            json!({ "holds": yes(0.95), "sight": yes(0.0) }),
        ),
        ("locked field", decision("type", "002", "004")),
        ("echo Ada", decision("done", "001", "001")),
        ("pressed Enter", decision("wait", "001", "001")),
        ("typed", decision("enter", "001", "001")),
        ("", decision("type", "001", "005")),
    ])
    .await;
    let script = temp_path("fixture.e2e");
    std::fs::write(&script, format!("app {}\nwith name = Ada\ndo type the name and submit it\nexpect the window shows echo Ada\nsee \"echo Ada\"\n", bundle.display())).unwrap();

    let (first, report) = act(&script, Some(&model), false);
    if report["error"]
        .as_str()
        .is_some_and(|error| error.contains("Privacy & Security"))
    {
        eprintln!("skipped: this process may not use the accessibility API");
        return;
    }
    assert_eq!(first.status.code(), Some(0), "{report}");
    assert_eq!(vias(&report), ["app", "model", "model", "check"]);
    assert_eq!(report["steps"][1]["actions"][1], "pressed Enter");
    for asked in model.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&asked.body).contains("Point at one control"));
    }

    let (second, replayed) = act(&script, None, true);
    assert_eq!(second.status.code(), Some(0), "{replayed}");
    assert_eq!(vias(&replayed), ["app", "replay", "cache", "check"]);
    assert_eq!(replayed["model_calls"], 0);

    let locked = temp_path("locked.e2e");
    std::fs::write(
        &locked,
        format!(
            "app {}\nwith name = Bob\ndo type the name into the locked field\n",
            bundle.display()
        ),
    )
    .unwrap();
    let (refused, report) = act(&locked, Some(&model), false);
    assert_eq!(refused.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .contains("cannot take focus"),
        "{report}"
    );
    assert!(
        !report["screen"].as_str().unwrap().contains("Bob"),
        "{report}"
    );

    let code_model = mock([
        ("code 42", decision("done", "001", "001")),
        ("pressed Enter", decision("wait", "001", "001")),
        ("typed", decision("enter", "001", "001")),
        ("", decision("type", "003", "003")),
    ])
    .await;
    let keys = temp_path("keys.e2e");
    std::fs::write(
        &keys,
        format!(
            "app {}\nwith code = 42\ndo type the code and submit it\nsee \"code 42\"\n",
            bundle.display()
        ),
    )
    .unwrap();
    let (typed, report) = act(&keys, Some(&code_model), false);
    assert_eq!(typed.status.code(), Some(0), "{report}");
    assert!(
        !report["screen"].as_str().unwrap().contains("old"),
        "{report}"
    );
    let manual = std::fs::read_to_string(bundle.with_file_name("manual.log")).unwrap();
    assert_eq!(manual, "true");

    let frozen_model = mock([("", decision("type", "004", "002"))]).await;
    let frozen = temp_path("frozen.e2e");
    std::fs::write(
        &frozen,
        format!(
            "app {}\nwith name = Zed\ndo type the name into the frozen field\n",
            bundle.display()
        ),
    )
    .unwrap();
    let (refused, report) = act(&frozen, Some(&frozen_model), false);
    assert_eq!(refused.status.code(), Some(1), "{report}");
    assert!(
        report["steps"][1]["actions"][0]
            .as_str()
            .unwrap()
            .contains("did not take the text"),
        "{report}"
    );

    let upper_model = mock([
        ("typed", decision("done", "001", "001")),
        ("", decision("type", "005", "001")),
    ])
    .await;
    let coupon = temp_path("mac-coupon.e2e");
    std::fs::write(
        &coupon,
        format!(
            "app {}\nwith coupon = save10\ndo type the coupon into the coupon field\n",
            bundle.display()
        ),
    )
    .unwrap();
    let (applied, report) = act(&coupon, Some(&upper_model), false);
    assert_eq!(applied.status.code(), Some(0), "{report}");
    assert!(
        report["screen"]
            .as_str()
            .unwrap()
            .contains("textbox \"Coupon\" = \"SAVE10\""),
        "{report}"
    );
}
