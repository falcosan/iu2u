mod common;

use std::io::{BufRead, BufReader, Write};
use std::pin::pin;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{
    answer_always, answer_when, client, config, config_home, fail_always, iu2u, temp_path, text,
    yes,
};
use futures_util::{StreamExt, stream};
use iu2u::commands;
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

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
async fn filter_sends_a_request_again_after_a_passing_server_error() {
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&model)
        .await;
    answer_when(&model, "ERROR", json!({ "holds": yes(0.95) })).await;
    answer_always(&model, json!({ "holds": yes(0.05) })).await;
    let lines = ["ERROR one", "INFO two", "ERROR three"].map(|line| Ok(line.to_string()));
    let kept: Vec<String> = commands::filter(
        &client(&model),
        "This line reports a problem",
        0.5,
        stream::iter(lines),
    )
    .map(Result::unwrap)
    .collect()
    .await;
    assert_eq!(kept, ["ERROR one", "ERROR three"]);
    assert_eq!(model.received_requests().await.unwrap().len(), 4);
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
        iu2u()
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
    let with_vision = iu2u()
        .args(["filter", "This line reports a problem"])
        .env("IU2U_MODEL", "model-x")
        .env("IU2U_VISION", "yes")
        .output()
        .unwrap();
    assert_eq!(with_vision.status.code(), Some(1));
}

#[test]
fn filter_rejects_a_threshold_that_is_not_a_probability() {
    for at in ["80", "-1", "NaN"] {
        let output = iu2u()
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
    let mut filter = iu2u()
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
    let exited = tokio::time::timeout(Duration::from_secs(10), async {
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
async fn filter_stops_quietly_when_its_reader_goes_away() {
    let model = MockServer::start().await;
    answer_always(&model, json!({ "holds": yes(0.95) })).await;
    let mut filter = iu2u()
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
    let mut filter = iu2u()
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

#[test]
fn a_bad_variable_is_refused_where_it_is_read_and_shown_by_config() {
    let output = iu2u()
        .args(["filter", "This line reports a problem"])
        .env("IU2U_MODEL", "model-x")
        .env("IU2U_TIMEOUT_MS", "0")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        text(&output.stderr)
            .contains("IU2U_TIMEOUT_MS must be a positive whole number of milliseconds")
    );
    let home = config_home("bad-variable", "");
    let env = [("IU2U_TRIM", "yes")];
    let listed = text(&config(&home, &env, &[]).stdout);
    assert!(
        listed.contains("IU2U_TRIM must be true or false"),
        "{listed}"
    );
    let got = config(&home, &env, &["get", "trim"]);
    assert_eq!(got.status.code(), Some(2));
    assert!(text(&got.stderr).contains("IU2U_TRIM must be true or false"));
}

#[test]
fn switches_take_1_or_0_from_the_file_the_environment_and_config_set() {
    let home = config_home("switch-numbers", "trim = 1\n");
    assert_eq!(text(&config(&home, &[], &["get", "trim"]).stdout), "true\n");
    let off = config(&home, &[("IU2U_TRIM", "0")], &["get", "trim"]);
    assert_eq!(text(&off.stdout), "false\n");
    config(&home, &[], &["set", "compass", "1"]);
    assert!(
        std::fs::read_to_string(home.join("iu2u/config.toml"))
            .unwrap()
            .contains("compass = true")
    );
}

#[cfg(unix)]
#[test]
fn config_set_keeps_a_symlinked_file_and_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let home = temp_path("config-symlink");
    let (linked, real) = (
        home.join("iu2u/config.toml"),
        home.join("dotfiles/iu2u.toml"),
    );
    std::fs::create_dir_all(home.join("iu2u")).unwrap();
    std::fs::create_dir_all(home.join("dotfiles")).unwrap();
    std::fs::write(&real, "model = \"claude\"\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    assert!(
        config(&home, &[], &["set", "guard", "false"])
            .status
            .success()
    );
    assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(real.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "model = \"claude\"\nguard = false\n"
    );
}

#[test]
fn config_shows_each_value_and_where_it_comes_from() {
    let home = config_home(
        "config-origins",
        "model = \"decision\"\ntimeout_ms = 5000\n",
    );
    let file = home.join("iu2u/config.toml");
    let env = [("IU2U_MODEL", "claude"), ("IU2U_API_KEY", "secret-key")];
    assert_eq!(
        text(&config(&home, &env, &["get", "model"]).stdout),
        "claude\n"
    );
    let list = text(&config(&home, &env, &[]).stdout);
    for (key, line) in [
        (
            "model",
            format!(
                "claude  IU2U_MODEL, overriding decision in {}",
                file.display()
            ),
        ),
        ("timeout_ms", format!("5000    {}", file.display())),
        ("effort", "medium  default".into()),
        ("browser", "(none)  default".into()),
        ("api_key", "set     IU2U_API_KEY".into()),
    ] {
        let expected = format!("{key} {line}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            list.lines()
                .any(|row| row.split_whitespace().collect::<Vec<_>>().join(" ") == expected),
            "{key}: {list}"
        );
    }
    assert!(!list.contains("secret-key"));
    let browser = config(&home, &env, &["get", "browser"]);
    assert_eq!((browser.status.code(), browser.stdout.len()), (Some(1), 0));
    assert_eq!(
        text(&config(&home, &[], &["path"]).stdout),
        format!("{}\n", file.display())
    );
}

#[test]
fn config_set_and_unset_keep_what_was_written_by_hand() {
    let home = config_home(
        "config-edits",
        "# mine\nmodel = \"x\"  # the server's name\n\n# the guard\nguard = true\n",
    );
    for args in [
        ["set", "guard", "false"],
        ["set", "model", "y"],
        ["set", "trim", "true"],
    ] {
        assert!(config(&home, &[], &args).status.success(), "{args:?}");
    }
    let file = home.join("iu2u/config.toml");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# mine\nmodel = \"y\"  # the server's name\n\n# the guard\nguard = false\ntrim = true\n"
    );
    assert!(config(&home, &[], &["unset", "guard"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# mine\nmodel = \"y\"  # the server's name\ntrim = true\n"
    );
    let fresh = temp_path("config-fresh");
    assert!(
        config(&fresh, &[], &["set", "effort", "high"])
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(fresh.join("iu2u/config.toml")).unwrap(),
        "effort = \"high\"\n"
    );
}

#[test]
fn config_refuses_bad_values_unknown_keys_and_the_api_key() {
    let home = temp_path("config-refusals");
    for (args, says) in [
        (
            ["set", "guard", "yes"].as_slice(),
            "`guard` must be true or false",
        ),
        (
            &["set", "timeout_ms", "-1"],
            "`timeout_ms` must be a positive whole number",
        ),
        (
            &["set", "timeout_ms", "0"],
            "`timeout_ms` must be a positive whole number",
        ),
        (
            &["set", "effort", "huge"],
            "`effort` must be one of low, medium",
        ),
        (
            &["set", "base_url", "ftp://host"],
            "`base_url` must be an http or https URL",
        ),
        (
            &["set", "colour", "red"],
            "unknown key `colour`; the keys are model, effort",
        ),
        (&["set", "api_key", "k"], "/plugin configure iu2u@iu2u"),
        (&["get", "api_key"], "IU2U_API_KEY"),
    ] {
        let output = config(&home, &[], args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            text(&output.stderr).contains(says),
            "{args:?}: {}",
            text(&output.stderr)
        );
    }
    assert!(!home.exists());
}

#[test]
fn a_bad_config_file_is_named_with_its_key_or_line_and_left_alone() {
    for (name, written, says) in [
        (
            "config-unknown",
            "colour = \"red\"\n",
            "unknown key `colour`",
        ),
        (
            "config-type",
            "guard = \"yes\"\n",
            "`guard` must be true or false",
        ),
        ("config-syntax", "model = \"x\"\nguard =\n", "line 2"),
    ] {
        let home = config_home(name, written);
        let file = home.join("iu2u/config.toml");
        for args in [[].as_slice(), &["set", "guard", "false"]] {
            let output = config(&home, &[], args);
            let stderr = text(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{name} {args:?}");
            assert!(
                stderr.contains(&file.display().to_string()) && stderr.contains(says),
                "{name}: {stderr}"
            );
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), written);
    }
}

#[test]
fn config_unset_removes_a_key_the_file_got_wrong() {
    for (name, written, key, left) in [
        (
            "unset-unknown",
            "colour = \"red\"\nguard = false\n",
            "colour",
            "guard = false\n",
        ),
        (
            "unset-type",
            "guard = \"yes\"\ntrim = true\n",
            "guard",
            "trim = true\n",
        ),
    ] {
        let home = config_home(name, written);
        assert!(
            config(&home, &[], &["unset", key]).status.success(),
            "{name}"
        );
        assert!(config(&home, &[], &[]).status.success(), "{name}");
        assert_eq!(
            std::fs::read_to_string(home.join("iu2u/config.toml")).unwrap(),
            left
        );
    }
    let written = "model = \"x\"\nguard =\n";
    let broken = config_home("unset-syntax", written);
    let output = config(&broken, &[], &["unset", "guard"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("line 2"));
    assert_eq!(
        std::fs::read_to_string(broken.join("iu2u/config.toml")).unwrap(),
        written
    );
    let unknown = config(&config_home("unset-absent", ""), &[], &["unset", "colour"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(text(&unknown.stderr).contains("unknown key `colour`"));
}

#[test]
fn without_a_home_only_the_environment_is_read() {
    let output = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["config", "path"])
        .env_clear()
        .env("XDG_CONFIG_HOME", "relative/dir")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("XDG_CONFIG_HOME"));
    let effort = Command::new(env!("CARGO_BIN_EXE_iu2u"))
        .args(["config", "get", "effort"])
        .env_clear()
        .env("IU2U_EFFORT", "high")
        .output()
        .unwrap();
    assert_eq!(text(&effort.stdout), "high\n");
}

#[cfg(local_model)]
#[test]
fn config_set_stores_a_hugging_face_link_in_short_form_and_names_init() {
    let home = temp_path("config-link");
    let output = config(
        &home,
        &[],
        &[
            "set",
            "model",
            "https://huggingface.co/o/r/blob/main/m-Q8_0.gguf",
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("is not installed yet: run iu2u init"));
    assert_eq!(
        std::fs::read_to_string(home.join("iu2u/config.toml")).unwrap(),
        "model = \"decision:o/r:m-Q8_0.gguf\"\n"
    );
    let pinned = format!(
        "https://huggingface.co/o/r/blob/{}/m-Q8_0.gguf",
        "c".repeat(40)
    );
    assert!(
        config(&home, &[], &["set", "model", &pinned])
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(home.join("iu2u/config.toml")).unwrap(),
        format!("model = \"{pinned}\"\n")
    );
}
