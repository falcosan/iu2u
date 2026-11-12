#![cfg(unix)]

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use common::{answer_always, reply, temp_path, yes};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHOP: &str = r#"<!doctype html><title>Shop</title>
<h1>Sign in</h1>
<label>Email <input id="email"></label>
<button id="go" type="button">BUTTON</button>
<p id="out"></p>
<script>go.onclick = () => { out.textContent = 'Welcome ' + email.value; };</script>"#;

const SECRET: &str = "sk-live  7f3a";

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
        "press": choice(pick),
        "press_reversed": choice(pick_reversed),
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
        ("\"holds\"", json!({ "holds": yes(0.95) })),
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
        ("\"holds\"", json!({ "holds": yes(0.95) })),
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
    answer_always(&model, json!({ "holds": yes(0.1) })).await;
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
    answer_always(&model, json!({ "holds": yes(0.1) })).await;
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
}

#[tokio::test]
async fn an_inconclusive_claim_fails_the_script_and_records_nothing() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.6) })).await;
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
async fn a_claim_about_a_changed_screen_is_asked_again() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
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
async fn a_lock_that_cannot_be_written_keeps_the_report() {
    use std::os::unix::fs::PermissionsExt;
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
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
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
    let (output, shown) = shown(&[&long_claim("long-readable")], Some(&model), false);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(
        shown.contains("  ✓ expect the cart total is 5 dollars")
            && shown.contains("    → holds 0.95; the model saw only the first 6000 characters"),
        "{shown}"
    );
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
    assert!(detail.contains("after 30 tries"), "{detail}");
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
    let script = temp_path("slow.e2e");
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
        ("\"holds\"", json!({ "holds": yes(0.95) })),
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
