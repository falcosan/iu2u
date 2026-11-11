use std::collections::HashMap;
use std::ffi::OsString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use super::screen::{Element, Kind, Node, words};
use super::surface::{self, Action, Probe};
use crate::model::setting;

const APPLICATIONS: [&str; 4] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
];
const COMMANDS: [&str; 6] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "brave-browser",
];
const REPLY_WITHIN: Duration = Duration::from_secs(30);
const CLOSE_WITHIN: Duration = Duration::from_secs(5);
const SCROLL_PIXELS: u32 = 600;
const MAX_DIALOGS: usize = 10;
const LONG_REQUEST: Duration = Duration::from_secs(2);
const STDERR_CHARS: usize = 500;
const PROFILE_PREFIX: &str = "iu2u-browser-";
const BUSY_REQUESTS: [&str; 5] = ["Document", "Stylesheet", "Script", "XHR", "Fetch"];
const WATCH: &str = "window.__iu2u ?? (new MutationObserver(() => window.__iu2u++).observe(document, { subtree: true, childList: true, attributes: true, characterData: true }), window.__iu2u = 0)";
const HIT: &str = "function(x, y) { const hit = this.getRootNode().elementFromPoint(x, y); return !!hit && (hit === this || this.contains(hit) || hit.closest('label')?.control === this); }";
const FOCUS_AND_SELECT: &str = "function() { this.focus(); if (this.select) { this.select(); } else { getSelection().selectAllChildren(this); } const active = this.getRootNode().activeElement; return active === this || this.contains(active); }";
const TOOK: &str =
    "function(text) { return typeof this.value === 'string' ? this.value === text : null; }";
const CHOOSE: &str = "function(label) { const words = text => text.trim().split(/\\s+/).join(' '); const option = [...this.options].find(o => !o.disabled && (words(o.label) === label || words(o.text) === label)); if (!option) return false; option.selected = true; this.dispatchEvent(new Event('input', { bubbles: true })); this.dispatchEvent(new Event('change', { bubbles: true })); return true; }";
const PRESSABLE: [&str; 11] = [
    "button",
    "checkbox",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "switch",
    "tab",
    "treeitem",
];
const PRESSABLE_WHEN_FOCUSABLE: [&str; 6] =
    ["cell", "gridcell", "group", "image", "listitem", "row"];
const TYPABLE: [&str; 3] = ["searchbox", "spinbutton", "textbox"];
const HIDDEN: [&str; 7] = [
    "InlineTextBox",
    "LabelText",
    "LineBreak",
    "MenuListPopup",
    "generic",
    "none",
    "presentation",
];

static LAUNCHED: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct PageState {
    main_frame: String,
    requests: HashMap<String, Instant>,
    loading: bool,
    dialogs: Vec<(String, bool)>,
}

struct Connection {
    writer: tokio::sync::Mutex<pipe::Sender>,
    replies: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    last_id: AtomicU64,
    page: Mutex<PageState>,
}

pub(super) struct Browser {
    connection: Arc<Connection>,
    session: String,
    child: Child,
    profile: PathBuf,
}

impl Browser {
    pub(super) async fn launch(headful: bool) -> Result<Self> {
        let executable = find()?;
        let temp = std::env::temp_dir();
        sweep_profiles(&temp);
        let profile = temp.join(format!(
            "{PROFILE_PREFIX}{}-{}",
            std::process::id(),
            LAUNCHED.fetch_add(1, Ordering::Relaxed)
        ));
        private_dir(&profile)
            .with_context(|| format!("cannot create the browser profile {}", profile.display()))?;
        let started = Self::start(&executable, profile.clone(), headful).await;
        if started.is_err() {
            let said = std::fs::read_to_string(profile.join("stderr.log")).unwrap_or_default();
            std::fs::remove_dir_all(&profile).ok();
            let said = said.trim();
            if !said.is_empty() {
                let from = said
                    .char_indices()
                    .rev()
                    .nth(STDERR_CHARS)
                    .map_or(0, |(at, character)| at + character.len_utf8());
                let tail = &said[from..];
                return started.with_context(|| format!("the browser said: {tail}"));
            }
        }
        started
    }

    async fn start(executable: &Path, profile: PathBuf, headful: bool) -> Result<Self> {
        let (child_reads, we_write) = std::io::pipe()?;
        let (we_read, child_writes) = std::io::pipe()?;
        let (child_reads, child_writes) = (above_stdio(child_reads)?, above_stdio(child_writes)?);
        let (read_fd, write_fd) = (child_reads.as_raw_fd(), child_writes.as_raw_fd());
        let mut command = Command::new(executable);
        command
            .arg("--remote-debugging-pipe")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args((!headful).then_some("--headless"))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=1280,900",
            ])
            .args(
                (cfg!(target_os = "linux")
                    && (std::env::var_os("CI").is_some() || unsafe { libc::geteuid() } == 0))
                    .then_some("--no-sandbox"),
            )
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(profile.join("stderr.log"))?)
            .kill_on_drop(true);
        unsafe {
            command.as_std_mut().pre_exec(move || {
                if libc::dup2(read_fd, 3) < 0 || libc::dup2(write_fd, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .with_context(|| format!("cannot start {}", executable.display()))?;
        drop((child_reads, child_writes));
        let connection = Arc::new(Connection {
            writer: tokio::sync::Mutex::new(pipe::Sender::from_owned_fd(we_write.into())?),
            replies: Mutex::default(),
            last_id: AtomicU64::new(0),
            page: Mutex::default(),
        });
        tokio::spawn(read(
            pipe::Receiver::from_owned_fd(we_read.into())?,
            connection.clone(),
        ));
        let target = connection
            .send(None, "Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let target = target["targetId"].as_str().context("no page target")?;
        let attached = connection
            .send(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
            )
            .await?;
        let session = attached["sessionId"].as_str().context("no page session")?;
        connection.page.lock().unwrap().main_frame = target.into();
        connection
            .send(
                None,
                "Target.setDiscoverTargets",
                json!({ "discover": true }),
            )
            .await?;
        let browser = Self {
            session: session.into(),
            connection,
            child,
            profile,
        };
        for method in ["Page.enable", "Network.enable", "Accessibility.enable"] {
            browser.call(method, json!({})).await?;
        }
        browser
            .call(
                "Emulation.setFocusEmulationEnabled",
                json!({ "enabled": true }),
            )
            .await?;
        browser
            .call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": WATCH }),
            )
            .await?;
        Ok(browser)
    }

    pub(super) async fn open(&mut self, url: &str) -> Result<()> {
        self.connection.page.lock().unwrap().dialogs.clear();
        let started = Instant::now();
        let opened = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(error) = opened["errorText"].as_str() {
            bail!("cannot open {url}: {error}");
        }
        self.settle(None, started).await;
        Ok(())
    }

    pub(super) async fn observe(&mut self) -> Result<Vec<Node>> {
        let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = tree["nodes"].as_array().context("no accessibility tree")?;
        let index: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(position, node)| Some((node["nodeId"].as_str()?, position)))
            .collect();
        let mut arena: Vec<Node> = nodes
            .iter()
            .map(|node| to_node(node, nodes, &index))
            .collect();
        for (name, shown) in &mut self.connection.page.lock().unwrap().dialogs {
            *shown = true;
            arena.push(Node {
                role: "dialog".into(),
                name: name.clone(),
                ..Node::default()
            });
            let added = arena.len() - 1;
            arena[0].children.push(added);
        }
        Ok(arena)
    }

    pub(super) async fn act(&mut self, action: &Action) -> Result<()> {
        self.connection
            .page
            .lock()
            .unwrap()
            .dialogs
            .retain(|(_, shown)| !shown);
        let before = self.revision().await;
        let started = Instant::now();
        match action {
            Action::Press(element) => self.press(element).await?,
            Action::Type(element, input) => {
                let before = self.fresh(element).await?;
                if self.on(element, FOCUS_AND_SELECT, json!([])).await? != json!(true) {
                    bail!("{} cannot take focus", element.describe());
                }
                self.call("Input.insertText", json!({ "text": input.text }))
                    .await?;
                if !self.took(element, &input.text, &before).await? {
                    self.on(element, FOCUS_AND_SELECT, json!([])).await?;
                    for character in input.text.chars() {
                        let code = if character.is_ascii_alphanumeric() {
                            u32::from(character.to_ascii_uppercase())
                        } else {
                            0
                        };
                        self.key(&character.to_string(), code, &character.to_string())
                            .await?;
                    }
                    if !self.took(element, &input.text, &before).await? {
                        bail!("{} did not take the text", element.describe());
                    }
                }
            }
            Action::Choose(element, option) => {
                self.fresh(element).await?;
                let raw = match &element.kind {
                    Kind::Choose(options) => options
                        .iter()
                        .position(|shown| shown == option)
                        .and_then(|index| element.raw_options.get(index)),
                    _ => None,
                }
                .unwrap_or(option);
                if self.on(element, CHOOSE, json!([{ "value": raw }])).await? != json!(true) {
                    bail!("{} has no option {option:?}", element.describe());
                }
            }
            Action::Enter => self.key("Enter", 13, "\r").await?,
            Action::Scroll => {
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": 640, "y": 450, "deltaX": 0, "deltaY": SCROLL_PIXELS }),
                )
                .await?;
            }
            Action::Wait => {}
        }
        self.settle(before, started).await;
        Ok(())
    }

    pub(super) async fn close(mut self) {
        self.connection
            .send(None, "Browser.close", json!({}))
            .await
            .ok();
        if tokio::time::timeout(CLOSE_WITHIN, self.child.wait())
            .await
            .is_err()
        {
            tokio::time::timeout(CLOSE_WITHIN, self.child.kill())
                .await
                .ok();
        }
        std::fs::remove_dir_all(&self.profile).ok();
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.connection
            .send(Some(&self.session), method, params)
            .await
    }

    async fn revision(&self) -> Option<f64> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": WATCH, "returnByValue": true }),
            )
            .await
            .ok()?;
        result["result"]["value"].as_f64()
    }

    async fn settle(&self, before: Option<f64>, started: Instant) {
        surface::settle(before, started, async || {
            let (busy, loading) = {
                let page = self.connection.page.lock().unwrap();
                let busy = page.loading
                    || page
                        .requests
                        .values()
                        .any(|at| *at >= started && at.elapsed() < LONG_REQUEST);
                (busy, page.loading)
            };
            Probe {
                revision: self.revision().await,
                busy,
                loading,
            }
        })
        .await;
    }

    async fn fresh(&self, element: &Element) -> Result<String> {
        let current = self
            .call(
                "Accessibility.getPartialAXTree",
                json!({ "backendNodeId": element.handle, "fetchRelatives": false }),
            )
            .await
            .with_context(|| format!("{} is gone", element.describe()))?;
        let node = &current["nodes"][0];
        let (role, name) = (role_of(node), text(&node["name"]));
        if role != element.role || name != element.raw_name {
            bail!("{} changed before it could be used", element.describe());
        }
        Ok(text(&node["value"]))
    }

    async fn took(&self, element: &Element, text: &str, before: &str) -> Result<bool> {
        if self.on(element, TOOK, json!([{ "value": text }])).await? == json!(true) {
            return Ok(true);
        }
        let after = self.fresh(element).await?;
        Ok(after != before || after == words(text))
    }

    async fn on(&self, element: &Element, function: &str, arguments: Value) -> Result<Value> {
        let resolved = self
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": element.handle }),
            )
            .await?;
        let object = resolved["object"]["objectId"]
            .as_str()
            .with_context(|| format!("{} is gone", element.describe()))?;
        let result = self
            .call(
                "Runtime.callFunctionOn",
                json!({ "objectId": object, "functionDeclaration": function, "arguments": arguments, "returnByValue": true }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            bail!(
                "{}",
                exception["exception"]["description"]
                    .as_str()
                    .unwrap_or("script failed")
            );
        }
        Ok(result["result"]["value"].clone())
    }

    async fn press(&self, element: &Element) -> Result<()> {
        self.fresh(element).await?;
        self.call(
            "DOM.scrollIntoViewIfNeeded",
            json!({ "backendNodeId": element.handle }),
        )
        .await?;
        let quads = self
            .call(
                "DOM.getContentQuads",
                json!({ "backendNodeId": element.handle }),
            )
            .await?;
        let quad: Vec<f64> = quads["quads"][0]
            .as_array()
            .with_context(|| format!("{} is not visible", element.describe()))?
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        let [left, top, _, _, right, bottom, ..] = quad[..] else {
            bail!("{} is not visible", element.describe());
        };
        let (x, y) = ((left + right) / 2.0, (top + bottom) / 2.0);
        if self
            .on(element, HIT, json!([{ "value": x }, { "value": y }]))
            .await?
            != json!(true)
        {
            bail!("{} is covered by another element", element.describe());
        }
        for (kind, button) in [
            ("mouseMoved", "none"),
            ("mousePressed", "left"),
            ("mouseReleased", "left"),
        ] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": button, "clickCount": 1 }),
            )
            .await?;
        }
        Ok(())
    }

    async fn key(&self, key: &str, code: u32, text: &str) -> Result<()> {
        for (kind, text) in [("keyDown", text), ("keyUp", "")] {
            self.call(
                "Input.dispatchKeyEvent",
                json!({ "type": kind, "key": key, "code": key, "windowsVirtualKeyCode": code, "text": text }),
            )
            .await?;
        }
        Ok(())
    }
}

impl Connection {
    async fn send(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply, replied) = oneshot::channel();
        self.replies.lock().unwrap().insert(id, reply);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(0);
        self.writer.lock().await.write_all(&bytes).await?;
        let reply = tokio::time::timeout(REPLY_WITHIN, replied)
            .await
            .with_context(|| format!("the browser did not answer {method}"))?
            .context("the browser closed")?;
        if let Some(error) = reply.get("error") {
            bail!(
                "{method}: {}",
                error["message"].as_str().unwrap_or("failed")
            );
        }
        Ok(reply["result"].clone())
    }

    fn track(self: &Arc<Self>, event: &Value) {
        let params = &event["params"];
        let mut page = self.page.lock().unwrap();
        let main_frame = params["frameId"] == page.main_frame.as_str();
        match event["method"].as_str().unwrap_or_default() {
            "Network.requestWillBeSent"
                if BUSY_REQUESTS.contains(&params["type"].as_str().unwrap_or_default()) =>
            {
                page.requests
                    .insert(text(&params["requestId"]), Instant::now());
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                page.requests
                    .remove(params["requestId"].as_str().unwrap_or_default());
            }
            "Page.frameStartedLoading" if main_frame => page.loading = true,
            "Page.frameStoppedLoading" if main_frame => page.loading = false,
            "Page.javascriptDialogOpening" => {
                let accept = ["beforeunload", "confirm"].contains(&text(&params["type"]).as_str());
                if page.dialogs.len() < MAX_DIALOGS {
                    let handled = if accept { "accepted" } else { "dismissed" };
                    page.dialogs
                        .push((format!("{} ({handled})", text(&params["message"])), false));
                }
                let connection = self.clone();
                let session = event["sessionId"].as_str().map(str::to_owned);
                tokio::spawn(async move {
                    connection
                        .send(
                            session.as_deref(),
                            "Page.handleJavaScriptDialog",
                            json!({ "accept": accept }),
                        )
                        .await
                        .ok();
                });
            }
            "Target.targetCreated"
                if params["targetInfo"]["type"] == "page"
                    && params["targetInfo"]["targetId"] != page.main_frame.as_str() =>
            {
                let connection = self.clone();
                let target = page.main_frame.clone();
                tokio::spawn(async move {
                    connection
                        .send(None, "Target.activateTarget", json!({ "targetId": target }))
                        .await
                        .ok();
                });
            }
            _ => {}
        }
    }
}

async fn read(receiver: pipe::Receiver, connection: Arc<Connection>) {
    let mut reader = BufReader::new(receiver);
    let mut buffer = Vec::new();
    while reader
        .read_until(0, &mut buffer)
        .await
        .is_ok_and(|read| read > 0)
    {
        buffer.pop();
        if let Ok(message) = serde_json::from_slice::<Value>(&buffer) {
            match message["id"].as_u64() {
                Some(id) => {
                    if let Some(reply) = connection.replies.lock().unwrap().remove(&id) {
                        reply.send(message).ok();
                    }
                }
                None => connection.track(&message),
            }
        }
        buffer.clear();
    }
    connection.replies.lock().unwrap().clear();
}

fn find() -> Result<PathBuf> {
    if let Some(path) = setting("BROWSER") {
        return Ok(path.into());
    }
    candidates(std::env::var_os("HOME"), std::env::var_os("PATH"))
        .into_iter()
        .find(|path| path.is_file())
        .context("no Chrome, Chromium, Edge or Brave found; set IU2U_BROWSER to the browser's executable")
}

fn candidates(home: Option<OsString>, path: Option<OsString>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = APPLICATIONS.map(PathBuf::from).into();
    if let Some(home) = home.map(PathBuf::from) {
        candidates.extend(APPLICATIONS.map(|app| home.join(&app[1..])));
    }
    for dir in path.iter().flat_map(std::env::split_paths) {
        candidates.extend(COMMANDS.map(|name| dir.join(name)));
    }
    candidates
}

fn sweep_profiles(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|name| name.strip_prefix(PROFILE_PREFIX))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<libc::pid_t>().ok());
        let gone = pid.is_some_and(|pid| {
            pid > 0
                && unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        if gone {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(path).ok();
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

fn above_stdio(fd: impl Into<OwnedFd>) -> std::io::Result<OwnedFd> {
    let fd = fd.into();
    let copy = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

fn role_of(node: &Value) -> String {
    let role = text(&node["role"]);
    let editable_root = flag(node, "focusable") && property(node, "editable").is_some();
    let kept =
        TYPABLE.contains(&role.as_str()) || ["combobox", "RootWebArea"].contains(&role.as_str());
    if editable_root && !kept {
        "textbox".into()
    } else {
        role
    }
}

fn flag(node: &Value, name: &str) -> bool {
    property(node, name).is_some_and(|value| value == true || value == "true")
}

fn text(value: &Value) -> String {
    match value.get("value").unwrap_or(value) {
        Value::String(raw) => words(raw),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

fn property<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node["properties"]
        .as_array()?
        .iter()
        .find(|property| property["name"] == name)
        .map(|property| &property["value"]["value"])
}

fn to_node(node: &Value, nodes: &[Value], index: &HashMap<&str, usize>) -> Node {
    let children_of = |node: &Value| -> Vec<usize> {
        node["childIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| index.get(id.as_str()?).copied())
            .collect()
    };
    let is_popup = |&child: &usize| text(&nodes[child]["role"]) == "MenuListPopup";
    let mut children = children_of(node);
    let flag = |name: &str| flag(node, name);
    let role = role_of(node);
    let shown = !node["ignored"].as_bool().unwrap_or(false) && !HIDDEN.contains(&role.as_str());
    let mut options = Vec::new();
    let mut pending: Vec<usize> = children.iter().rev().copied().filter(is_popup).collect();
    for _ in 0..nodes.len() {
        let Some(at) = pending.pop() else { break };
        if text(&nodes[at]["role"]) == "option" && !self::flag(&nodes[at], "disabled") {
            options.push(text(&nodes[at]["name"]));
        }
        pending.extend(children_of(&nodes[at]).into_iter().rev());
    }
    if !options.is_empty() {
        children.retain(|child| !is_popup(child));
    }
    let kind = if !options.is_empty() {
        Some(Kind::Choose(options))
    } else if TYPABLE.contains(&role.as_str())
        || (role == "combobox" && property(node, "editable").is_some())
    {
        Some(Kind::Type)
    } else if PRESSABLE.contains(&role.as_str())
        || role == "combobox"
        || (flag("focusable") && PRESSABLE_WHEN_FOCUSABLE.contains(&role.as_str()))
    {
        Some(Kind::Press)
    } else {
        None
    };
    let mut states = Vec::new();
    match property(node, "checked").and_then(Value::as_str) {
        Some("true") => states.push("checked"),
        Some("false") => states.push("unchecked"),
        Some("mixed") => states.push("mixed"),
        _ => {}
    }
    for (name, state) in [
        ("disabled", "disabled"),
        ("expanded", "expanded"),
        ("selected", "selected"),
        ("pressed", "pressed"),
        ("focused", "focused"),
        ("required", "required"),
    ] {
        if flag(name) {
            states.push(state);
        }
    }
    let value = text(&node["value"]);
    if kind == Some(Kind::Type) && value.is_empty() {
        states.push("empty");
    }
    let name = text(&node["name"]);
    let role = match role.as_str() {
        "StaticText" => "text".into(),
        "RootWebArea" => "page".into(),
        _ if shown => role,
        _ => String::new(),
    };
    Node {
        name: if role.is_empty() { String::new() } else { name },
        role,
        value,
        states,
        kind: kind.filter(|_| shown),
        handle: node["backendDOMNodeId"].as_u64().unwrap_or_default() as usize,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_left_by_a_killed_run_is_replaced_by_a_private_one() {
        use std::os::unix::fs::PermissionsExt;
        let profile =
            std::env::temp_dir().join(format!("iu2u-profile-test-{}", std::process::id()));
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        private_dir(&profile).unwrap();
        let mode = std::fs::metadata(&profile).unwrap().permissions().mode();
        let left = std::fs::read_dir(&profile).unwrap().count();
        std::fs::remove_dir_all(&profile).unwrap();
        assert_eq!((mode & 0o777, left), (0o700, 0));
    }

    #[test]
    fn profiles_of_runs_that_died_are_swept_and_live_ones_kept() {
        let dir = std::env::temp_dir().join(format!("iu2u-sweep-test-{}", std::process::id()));
        let dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait_with_output().unwrap();
        let name = |pid: u32| format!("{PROFILE_PREFIX}{pid}-0");
        for kept in [name(dead_pid), name(std::process::id()), "other".into()] {
            std::fs::create_dir_all(dir.join(kept).join("Default")).unwrap();
        }
        sweep_profiles(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        let mut expected = vec![name(std::process::id()), "other".to_string()];
        expected.sort();
        assert_eq!(left, expected);
    }

    #[test]
    fn disabled_options_are_not_offered() {
        let nodes = [
            json!({ "nodeId": "1", "role": { "value": "combobox" }, "name": { "value": "Size" }, "childIds": ["2"] }),
            json!({ "nodeId": "2", "role": { "value": "MenuListPopup" }, "childIds": ["3", "4"] }),
            json!({ "nodeId": "3", "role": { "value": "option" }, "name": { "value": "Small" } }),
            json!({ "nodeId": "4", "role": { "value": "option" }, "name": { "value": "XXL (sold out)" }, "properties": [{ "name": "disabled", "value": { "value": true } }] }),
        ];
        let index: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node["nodeId"].as_str().unwrap(), position))
            .collect();
        let combobox = to_node(&nodes[0], &nodes, &index);
        assert_eq!(combobox.kind, Some(Kind::Choose(vec!["Small".into()])));
    }

    #[test]
    fn a_browser_installed_for_the_user_alone_is_found() {
        let found = candidates(Some("/Users/qa".into()), Some("/opt/bin".into()));
        assert!(found.contains(&PathBuf::from(
            "/Users/qa/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"
        )));
        assert!(found.contains(&PathBuf::from("/opt/bin/brave-browser")));
    }

    #[test]
    fn values_are_read_as_text_or_numbers() {
        assert_eq!(
            text(&json!({ "type": "string", "value": " Red   shirt " })),
            "Red shirt"
        );
        assert_eq!(text(&json!({ "type": "number", "value": 1 })), "1");
        assert_eq!(text(&json!({ "type": "boolean", "value": true })), "");
    }
}
