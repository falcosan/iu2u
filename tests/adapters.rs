mod common;

use std::process::{Output, Stdio};

#[cfg(unix)]
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

async fn filter(lines: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iu2u"));
    common::cleared_env(command.as_std_mut())
        .args(["filter", "This line reports a problem"])
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut filter = command.spawn().unwrap();
    let mut input = filter.stdin.take().unwrap();
    if let Err(error) = input.write_all(lines.as_bytes()).await {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}");
    }
    drop(input);
    filter.wait_with_output().await.unwrap()
}

#[cfg(unix)]
fn fake_claude(name: &str, script: &str) -> (std::path::PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = common::temp_path(name);
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("claude");
    std::fs::write(&program, format!("#!/bin/sh\n{script}")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.display());
    (dir, path)
}

#[cfg(unix)]
#[tokio::test]
async fn claude_answers_in_a_session_that_loads_no_settings_hooks_or_tools() {
    let (dir, path) = fake_claude(
        "claude-answers",
        r#"printf '%s\n' "$@" "memory=$CLAUDE_CODE_DISABLE_AUTO_MEMORY" "session=$IU2U_ADAPTER_SESSION" "effort=${CLAUDE_CODE_EFFORT_LEVEL-unset}" > "$0.args"
case $(cat) in *ERROR*) holds=0.9 ;; *) holds=0.1 ;; esac
echo "{\"is_error\": false, \"structured_output\": {\"q0\": $holds}}""#,
    );
    let output = filter(
        "INFO started\nERROR refused\n",
        &[
            ("IU2U_MODEL", "claude"),
            ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
            ("PATH", &path),
        ],
    )
    .await;
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ERROR refused\n");
    let args = std::fs::read_to_string(dir.join("claude.args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    for expected in [
        ["--model", "haiku"],
        ["--effort", "medium"],
        ["--setting-sources", ""],
        ["--tools", ""],
        ["--output-format", "json"],
    ] {
        assert!(args.windows(2).any(|pair| pair == expected), "{args:?}");
    }
    assert!(args.contains(&"--strict-mcp-config") && args.contains(&"--no-session-persistence"));
    assert!(
        args.contains(&"memory=1")
            && args.contains(&"session=1")
            && args.contains(&"effort=unset")
            && args.contains(&"--system-prompt"),
        "{args:?}"
    );
    let schema = args[args.iter().position(|arg| *arg == "--json-schema").unwrap() + 1];
    let schema: Value = serde_json::from_str(schema).unwrap();
    assert_eq!(schema["required"], json!(["q0"]));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_claude_session_reaches_the_caller() {
    let (_, path) = fake_claude(
        "claude-fails",
        r#"echo '{"is_error": true, "result": "Not logged in · Please run /login"}'
exit 1"#,
    );
    let output = filter(
        "ERROR refused\n",
        &[("IU2U_MODEL", "claude"), ("PATH", &path)],
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr.contains("claude failed: Not logged in"), "{stderr}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_claude_session_that_hangs_is_stopped_at_the_timeout() {
    let (_, path) = fake_claude("claude-hangs", "exec sleep 30");
    let started = std::time::Instant::now();
    let output = filter(
        "ERROR refused\n",
        &[
            ("IU2U_MODEL", "claude"),
            ("IU2U_TIMEOUT_MS", "200"),
            ("PATH", &path),
        ],
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(started.elapsed().as_secs() < 10, "{stderr}");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("claude did not answer within 200ms"),
        "{stderr}"
    );
}

#[tokio::test]
async fn an_unknown_effort_is_refused_before_claude_runs() {
    let output = filter(
        "ERROR refused\n",
        &[("IU2U_MODEL", "claude"), ("IU2U_EFFORT", "hi")],
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("IU2U_EFFORT must be one of low, medium, high, xhigh, max"),
        "{stderr}"
    );
}
