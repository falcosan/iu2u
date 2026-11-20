#![cfg(local_model)]

mod common;

use common::text;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::MockServer;

const GLIDE: &str = "371adada0c5b4d0193aa3212b831fdf04cf8f5889a51a9fc1012a9ecc08eb4be";
const FAKE_RUNTIME: &str = r#"
import http.server, json, os, socketserver, sys, time
args = sys.argv[1:]
here = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(here, "starts"), "a") as starts:
    starts.write(json.dumps({"pid": os.getpid(), "parent": os.getppid(), "cwd": os.getcwd(), "args": args,
        "leaked": sorted(k for k in os.environ if k.startswith(("LLAMA_", "HF_")))}) + "\n")
if os.environ.get("FAKE_FAIL"):
    sys.stderr.write(os.environ["FAKE_FAIL"] + "\n")
    sys.exit(1)
idle = float(os.environ.get("FAKE_IDLE", args[args.index("--sleep-idle-seconds") + 1]))
loaded_at = time.time() + float(os.environ.get("FAKE_LOAD", "0"))
last = [time.time()]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        if time.time() < loaded_at:
            return self.reply(503, {"error": {"message": "Loading model"}})
        if self.path == "/props":
            return self.reply(200, {"is_sleeping": time.time() - last[0] > idle})
        self.reply(200, {"status": "ok"})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if time.time() < loaded_at:
            return self.reply(503, {"error": {"message": "Loading model"}})
        last[0] = time.time()
        if os.environ.get("FAKE_STATUS"):
            return self.reply(int(os.environ["FAKE_STATUS"]), {"error": {"message": "not a decision model"}})
        holds = 0.9 if "ERROR" in body["state"] else 0.1
        self.reply(200, {"model": body["model"], "answers": {
            question: {"type": "noul", "noul": holds} for question in body["questions"]}})
socketserver.ThreadingUnixStreamServer(args[args.index("--host") + 1], Handler).serve_forever()
"#;

fn local(name: &str, installed: bool) -> PathBuf {
    let base = common::config_home(name, "model = \"decision\"\n");
    let runtime = data(&base).join("runtime/b12359");
    std::fs::create_dir_all(&runtime).unwrap();
    let server = runtime.join("llama-server");
    std::fs::write(&server, format!("#!{}{FAKE_RUNTIME}", python())).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    if installed {
        std::fs::create_dir_all(data(&base).join("models")).unwrap();
        std::fs::write(data(&base).join("models").join(GLIDE), "weights").unwrap();
    }
    base
}

const LINK: &str =
    "https://huggingface.co/o/r/blob/cccccccccccccccccccccccccccccccccccccccc/m.gguf";

fn install_linked(base: &Path) {
    let record = json!({ "repository": "o/r", "revision": "c".repeat(40), "file": "m.gguf",
        "size": 7, "sha256": "d".repeat(64) });
    std::fs::create_dir_all(data(base).join("models/o/r")).unwrap();
    std::fs::write(
        data(base).join("models/o/r/m.gguf.json"),
        record.to_string(),
    )
    .unwrap();
    std::fs::write(data(base).join("models").join("d".repeat(64)), "weights").unwrap();
}

fn python() -> &'static str {
    static PYTHON: OnceLock<String> = OnceLock::new();
    PYTHON.get_or_init(|| {
        let found = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        String::from_utf8(found.stdout).unwrap().trim().to_owned()
    })
}

fn data(base: &Path) -> PathBuf {
    base.join("data/iu2u")
}

fn iu2u(base: &Path, env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iu2u"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", base)
        .env("XDG_DATA_HOME", base.join("data"))
        .env("FAKE_IDLE", "2")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn filter_spawned(base: &Path, env: &[(&str, &str)]) -> Child {
    let mut child = iu2u(base, env)
        .args(["filter", "This line reports a problem"])
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"INFO started\nERROR refused\n")
        .unwrap();
    child
}

fn filter(base: &Path, env: &[(&str, &str)]) -> Output {
    filter_spawned(base, env).wait_with_output().unwrap()
}

fn answered(output: &Output) {
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ERROR refused\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn starts(base: &Path) -> Vec<Value> {
    std::fs::read_to_string(data(base).join("runtime/b12359/starts"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn alive(pid: &Value) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn kill(pids: &[&Value]) {
    for pid in pids {
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();
    }
    wait_until(|| pids.iter().all(|pid| !alive(pid)));
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn sockets(base: &Path) -> usize {
    std::fs::read_dir(data(base).join("run"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sock"))
        .count()
}

#[test]
fn a_request_starts_a_private_server_that_stops_when_idle() {
    let base = local("first-request", true);
    answered(&filter(
        &base,
        &[("LLAMA_ARG_THREADS", "1"), ("HF_TOKEN", "secret")],
    ));
    let run = data(&base).join("run");
    let mode = std::fs::metadata(&run).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    let started = &starts(&base)[0];
    assert_eq!(started["leaked"], json!([]));
    assert_eq!(
        started["cwd"],
        run.canonicalize().unwrap().to_str().unwrap()
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/cwd", started["parent"])).unwrap(),
        data(&base).canonicalize().unwrap()
    );
    let args = started["args"].as_array().unwrap();
    for expected in [
        ["-c", "32768"],
        ["--log-verbosity", "1"],
        ["--sleep-idle-seconds", "300"],
        [
            "-m",
            data(&base).join("models").join(GLIDE).to_str().unwrap(),
        ],
    ] {
        assert!(args.windows(2).any(|pair| pair == expected), "{args:?}");
    }
    assert!(args.contains(&json!("--offline")) && args.contains(&json!("--no-webui")));
    wait_until(|| sockets(&base) == 0 && !alive(&started["pid"]) && !alive(&started["parent"]));
    answered(&filter(&base, &[]));
    assert_eq!(starts(&base).len(), 2);
}

#[test]
fn sixteen_requests_at_once_on_a_loading_server_start_one_server() {
    let base = local("sixteen", true);
    let filters: Vec<Child> = (0..16)
        .map(|_| filter_spawned(&base, &[("FAKE_LOAD", "1")]))
        .collect();
    for filter in filters {
        answered(&filter.wait_with_output().unwrap());
    }
    assert_eq!(starts(&base).len(), 1);
}

#[test]
fn a_killed_server_or_supervisor_is_replaced_by_the_next_request() {
    let base = local("killed", true);
    let env = [("FAKE_IDLE", "60")];
    answered(&filter(&base, &env));
    let first = starts(&base)[0].clone();
    kill(&[&first["pid"]]);
    answered(&filter(&base, &env));
    let second = starts(&base)[1].clone();
    wait_until(|| !alive(&first["parent"]));
    kill(&[&second["parent"], &second["pid"]]);
    assert_eq!(sockets(&base), 1);
    answered(&filter(&base, &env));
    let third = starts(&base)[2].clone();
    kill(&[&third["parent"]]);
    answered(&filter(&base, &env));
    assert_eq!(starts(&base).len(), 3);
    kill(&[&third["pid"]]);
}

#[test]
fn a_model_that_is_not_installed_fails_at_once_and_names_init() {
    let base = local("not-installed", false);
    let started = Instant::now();
    let output = filter(&base, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("is not installed: run iu2u init") && !stderr.contains("unreachable"),
        "{stderr}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(starts(&base).is_empty());
}

#[test]
fn a_server_that_cannot_start_is_explained_from_its_log() {
    let base = local("cannot-start", true);
    let output = filter(
        &base,
        &[(
            "FAKE_FAIL",
            "llama-server: error while loading shared libraries: libgomp.so.1: cannot open shared object file",
        )],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("stopped before it was ready")
            && stderr.contains("package libgomp1")
            && stderr.contains(&data(&base).join("run").display().to_string())
            && stderr.contains("libgomp.so.1: cannot open"),
        "{stderr}"
    );
    assert_eq!(starts(&base).len(), 1);
}

fn run(base: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    iu2u(base, env)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn status_tells_what_runs_and_stop_leaves_nothing_running() {
    let base = local("status-stop", true);
    let env = [("FAKE_IDLE", "60")];
    let status = text(&run(&base, &["status"], &env).stdout);
    let file = base.join("iu2u/config.toml");
    for expected in [
        format!("model      decision, from {}", file.display()),
        format!(
            "runtime    {}",
            data(&base).join("runtime/b12359").display()
        ),
        format!(
            "file       {}, 7 bytes",
            data(&base).join("models").join(GLIDE).display()
        ),
        "server     not running".into(),
        format!("log        {}", data(&base).join("run").display()),
    ] {
        assert!(status.contains(&expected), "{expected}\n{status}");
    }
    answered(&filter(&base, &env));
    assert!(
        text(&run(&base, &["status"], &env).stdout).contains("server     running, model loaded")
    );
    let started = starts(&base)[0].clone();
    let stopped = run(&base, &["stop"], &env);
    assert_eq!(text(&stopped.stdout), "Stopped 1 local server.\n");
    assert!(!alive(&started["pid"]));
    let again = run(&base, &["stop"], &env);
    assert_eq!((again.status.code(), again.stdout.len()), (Some(0), 0));
}

#[test]
fn warm_starts_the_server_at_once_and_replaces_one_that_lost_its_supervisor() {
    let base = local("warm", true);
    let env = [("FAKE_IDLE", "60"), ("FAKE_LOAD", "1.5")];
    let started = Instant::now();
    let warm = run(&base, &["hook", "warm"], &env);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!((warm.status.code(), warm.stdout.len()), (Some(0), 0));
    wait_until(|| sockets(&base) == 1);
    answered(&filter(&base, &env));
    let first = starts(&base)[0].clone();
    kill(&[&first["parent"]]);
    answered(&filter(&base, &env));
    run(&base, &["hook", "warm"], &env);
    wait_until(|| starts(&base).len() == 2 && !alive(&first["pid"]));
    answered(&filter(&base, &env));
    run(&base, &["stop"], &env);
    let broken = local("warm-broken", false);
    std::fs::write(broken.join("iu2u/config.toml"), "colour = 1\n").unwrap();
    let warm = run(&broken, &["hook", "warm"], &[]);
    assert_eq!((warm.status.code(), warm.stdout.len()), (Some(0), 0));
}

const HANDS_OVER: &str = r#"
import os, signal, socket, sys
path = sys.argv[1]
held = []
def listen():
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path)
    listener.listen()
    return listener
def hand_over(*_):
    read, write = os.pipe()
    if os.fork() == 0:
        os.unlink(path)
        successor = listen()
        with open(path + ".successor", "w") as pid:
            pid.write(str(os.getpid()))
        os.write(write, b"x")
        while True:
            held.append(successor.accept()[0])
    os.read(read, 1)
    os._exit(0)
listener = listen()
signal.alarm(30)
signal.signal(signal.SIGTERM, hand_over)
while True:
    held.append(listener.accept()[0])
"#;

#[test]
fn stop_returns_when_the_stopped_server_hands_its_socket_to_a_new_one() {
    let base = local("hand-over", false);
    let socket = data(&base).join("run/0123456789abcdef.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let mut first = Command::new(python())
        .args(["-c", HANDS_OVER])
        .arg(&socket)
        .spawn()
        .unwrap();
    wait_until(|| socket.exists());
    let mut stop = iu2u(&base, &[]).arg("stop").spawn().unwrap();
    let started = Instant::now();
    let returned = loop {
        if stop.try_wait().unwrap().is_some() {
            break true;
        }
        if started.elapsed() > Duration::from_secs(4) {
            stop.kill().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    wait_until(|| first.try_wait().unwrap().is_some());
    let successor = std::fs::read_to_string(socket.with_extension("sock.successor")).unwrap();
    let successor = Value::from(successor.parse::<u32>().unwrap());
    let still_serves = alive(&successor);
    kill(&[&successor]);
    assert!(
        returned,
        "iu2u stop still waits for a server it did not stop"
    );
    assert!(still_serves);
}

#[test]
fn init_that_cannot_install_leaves_the_config_as_it_was() {
    let base = local("init-cannot-install", false);
    install_linked(&base);
    let records = data(&base).join("models/o/r");
    std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o555)).unwrap();
    let config = base.join("iu2u/config.toml");
    std::fs::write(&config, format!("model = \"{LINK}\"\n")).unwrap();
    let output = run(&base, &["init", "--yes"], &[]);
    std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("cannot write"));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        format!("model = \"{LINK}\"\n")
    );
}

#[test]
fn init_skips_the_test_question_when_a_variable_overrides_the_model_it_wrote() {
    let base = local("init-overridden", true);
    install_linked(&base);
    let output = run(
        &base,
        &["init", "--yes"],
        &[("IU2U_MODEL", "decision:o/r"), ("FAKE_STATUS", "501")],
    );
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("IU2U_MODEL=decision:o/r in this shell overrides the model just written")
            && stderr.contains("Skipped the test question"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("iu2u/config.toml")).unwrap(),
        "model = \"decision\"\n"
    );
    let server = local("init-overridden-server", false);
    std::fs::write(
        server.join("iu2u/config.toml"),
        "model = \"model-x\"\nbase_url = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();
    let output = run(
        &server,
        &["init", "--yes"],
        &[("IU2U_BASE_URL", "http://127.0.0.1:8")],
    );
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(
            "IU2U_BASE_URL=http://127.0.0.1:8 in this shell overrides the base_url just written"
        ) && stderr.contains("Skipped the test question"),
        "{stderr}"
    );
}

#[tokio::test]
async fn init_for_a_server_still_lists_the_local_models_left_behind() {
    let model = MockServer::start().await;
    common::answer_always(&model, json!({ "test": common::yes(0.97) })).await;
    let base = local("init-server-tidy", false);
    install_linked(&base);
    std::fs::write(
        base.join("iu2u/config.toml"),
        format!("model = \"model-x\"\nbase_url = \"{}\"\n", model.uri()),
    )
    .unwrap();
    let output = run(&base, &["init", "--yes"], &[]);
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("1) o/r m.gguf, 7 bytes"), "{stderr}");
}

#[test]
fn a_background_process_of_another_build_tells_the_caller_and_starts_nothing() {
    let base = local("other-build", true);
    let output = run(&base, &["serve", GLIDE, "0123456789abcdef"], &[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let said = std::fs::read_to_string(data(&base).join("run/0123456789abcdef.log")).unwrap();
    assert!(said.contains("restart that program"), "{said}");
    assert!(starts(&base).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn a_process_whose_binary_was_replaced_still_starts_its_server() {
    let base = local("replaced-binary", true);
    let copy = base.join("iu2u-before-upgrade");
    std::fs::copy(env!("CARGO_BIN_EXE_iu2u"), &copy).unwrap();
    let env = iu2u(&base, &[]);
    let mut filter = Command::new(&copy)
        .args(["filter", "This line reports a problem"])
        .env_clear()
        .envs(
            env.get_envs()
                .filter_map(|(name, value)| Some((name, value?))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::fs::remove_file(&copy).unwrap();
    filter
        .stdin
        .take()
        .unwrap()
        .write_all(b"INFO started\nERROR refused\n")
        .unwrap();
    answered(&filter.wait_with_output().unwrap());
    run(&base, &["stop"], &[]);
}

#[test]
fn init_stops_the_model_it_replaces_before_its_test_question() {
    let base = local("init-replaces", true);
    install_linked(&base);
    let env = [("FAKE_IDLE", "60")];
    answered(&filter(&base, &env));
    let replaced = starts(&base)[0].clone();
    std::fs::write(
        base.join("iu2u/config.toml"),
        "model = \"decision:o/r:m.gguf\"\n",
    )
    .unwrap();
    let output = run(&base, &["init", "--yes"], &env);
    assert!(output.status.success(), "{}", text(&output.stderr));
    wait_until(|| !alive(&replaced["pid"]));
    run(&base, &["stop"], &[]);
}

#[tokio::test]
async fn init_for_a_server_that_wants_a_key_skips_the_test_and_says_where_it_goes() {
    let model = MockServer::start().await;
    common::fail_always(&model, 401).await;
    let base = local("init-key", false);
    std::fs::write(
        base.join("iu2u/config.toml"),
        format!("model = \"claudette\"\nbase_url = \"{}\"\n", model.uri()),
    )
    .unwrap();
    let output = run(&base, &["init", "--yes"], &[]);
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    for expected in [
        "the server asks for an API key",
        "claude plugin install iu2u@iu2u",
        "If your server needs an API key",
    ] {
        assert!(stderr.contains(expected), "{expected}\n{stderr}");
    }
}

#[test]
fn init_needs_a_terminal_or_yes() {
    let base = local("init-no-terminal", true);
    let output = run(&base, &["init"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("iu2u init --yes"));
}

#[test]
fn init_yes_keeps_the_configured_model_and_tests_it() {
    let base = local("init-yes", true);
    let output = run(&base, &["init", "--yes"], &[]);
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(text(&output.stdout).starts_with("The model answered in "));
    for expected in [
        "use fastino_GLiDE-9B-Q4_K_M.gguf, already installed",
        "claude plugin install iu2u@iu2u",
    ] {
        assert!(stderr.contains(expected), "{expected}\n{stderr}");
    }
    run(&base, &["stop"], &[]);
    let unset = local("init-yes-unset", false);
    std::fs::write(unset.join("iu2u/config.toml"), "").unwrap();
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        let output = run(&unset, &["init", "--yes"], &[]);
        assert_eq!(output.status.code(), Some(2));
        assert!(text(&output.stderr).contains("iu2u config set model"));
    }
}

#[test]
fn init_says_when_a_file_is_not_a_decision_model() {
    let base = local("init-not-decision", false);
    install_linked(&base);
    let config = base.join("iu2u/config.toml");
    std::fs::write(&config, format!("# mine\nmodel = \"{LINK}\"\n")).unwrap();
    let output = run(
        &base,
        &["init", "--yes"],
        &[("FAKE_STATUS", "501"), ("FAKE_IDLE", "60")],
    );
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("m.gguf is not served as a decision model") && stderr.contains("Put back"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        format!("# mine\nmodel = \"{LINK}\"\n")
    );
    let tested = &starts(&base)[0];
    wait_until(|| !alive(&tested["pid"]));
}

const TERMINAL: &str = r#"
import os, pty, sys
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[1], sys.argv[1:])
os.write(fd, sys.stdin.buffer.read())
out = b""
while True:
    try:
        data = os.read(fd, 65536)
    except OSError:
        break
    if not data:
        break
    out += data
_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(out)
sys.exit(os.waitstatus_to_exitcode(status))
"#;

fn init_on_a_terminal(base: &Path, answers: &str) -> Output {
    let mut child = iu2u(base, &[]);
    let mut terminal = Command::new(python());
    terminal
        .args(["-c", TERMINAL, env!("CARGO_BIN_EXE_iu2u"), "init"])
        .env_clear()
        .envs(
            child
                .get_envs()
                .filter_map(|(name, value)| Some((name, value?))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    child.stdin(Stdio::null());
    let mut terminal = terminal.spawn().unwrap();
    terminal
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    terminal.wait_with_output().unwrap()
}

#[tokio::test]
async fn init_asks_on_a_terminal_and_writes_nothing_until_confirmed() {
    let model = MockServer::start().await;
    common::answer_always(&model, json!({ "test": common::yes(0.97) })).await;
    let base = local("init-server", false);
    std::fs::remove_file(base.join("iu2u/config.toml")).unwrap();
    let answers = format!("4\nlocalhost:11435\n{}\nmodel-x\n3 5\n", model.uri());
    let declined = init_on_a_terminal(&base, &format!("{answers}n\n"));
    assert!(declined.status.success(), "{}", text(&declined.stdout));
    assert!(!base.join("iu2u/config.toml").exists());
    let accepted = init_on_a_terminal(&base, &format!("{answers}y\n"));
    let printed = text(&accepted.stdout);
    assert!(accepted.status.success(), "{printed}");
    assert!(printed.contains("The model answered in ") && printed.contains("0.97"));
    assert!(printed.contains("`base_url` must be an http or https URL"));
    assert!(printed.contains("/plugin configure iu2u@iu2u"));
    assert_eq!(
        std::fs::read_to_string(base.join("iu2u/config.toml")).unwrap(),
        format!(
            "model = \"model-x\"\nbase_url = \"{}\"\nguard = false\nproof = false\ntrim = true\ncompass = false\nprune = true\n",
            model.uri()
        )
    );
}

#[test]
fn init_on_a_terminal_lists_other_models_and_deletes_the_ones_picked() {
    let base = local("init-tidy", true);
    install_linked(&base);
    std::fs::create_dir_all(data(&base).join("runtime/b4000")).unwrap();
    let partial = data(&base).join(format!("models/{}.partial", "e".repeat(64)));
    std::fs::write(&partial, "part").unwrap();
    let output = init_on_a_terminal(&base, "1\n\ny\n1, 2\n1 2 1\n");
    let printed = text(&output.stdout);
    assert!(output.status.success(), "{printed}");
    for expected in [
        "Removed the old runtime b4000.",
        "1) o/r m.gguf, 7 bytes",
        "2) an unfinished download, 4 bytes",
        "Deleted m.gguf.",
        "Answer with numbers from 1 to 2, or none.",
        "Deleted the unfinished download.",
        "The model started in",
    ] {
        assert!(printed.contains(expected), "{expected}\n{printed}");
    }
    assert!(!data(&base).join("models").join("d".repeat(64)).exists() && !partial.exists());
    assert!(data(&base).join("models").join(GLIDE).exists());
    run(&base, &["stop"], &[]);
}

#[test]
fn init_on_a_terminal_uses_an_installed_linked_model_as_recorded() {
    let base = local("init-linked", false);
    install_linked(&base);
    std::fs::write(
        base.join("iu2u/config.toml"),
        "model = \"decision:o/r:m.gguf\"\n",
    )
    .unwrap();
    let output = init_on_a_terminal(&base, "2\nhttps://huggingface.co/o\n\n\ny\n");
    let printed = text(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(
        printed.contains("https://huggingface.co/o is not a Hugging Face model link")
            && printed.contains("use m.gguf, already installed"),
        "{printed}"
    );
    run(&base, &["stop"], &[]);
}
