use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::try_join_all;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use super::decide::{self, Grid};
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
pub(super) const SUITE_PREFIX: &str = "iu2u-act-";
const BUSY_REQUESTS: [&str; 5] = ["Document", "Stylesheet", "Script", "XHR", "Fetch"];
const WATCH: &str = "window.__iu2u ?? (new MutationObserver(() => window.__iu2u++).observe(document, { subtree: true, childList: true, attributes: true, characterData: true }), window.__iu2u = 0)";
const STABLE: &str = "function(within, still) { return new Promise(done => { const chain = new Set(); for (let node = this; node; node = node.parentNode ?? node.host) chain.add(node); let last = null; const frame = () => { const box = this.getBoundingClientRect(); const at = [box.x + box.width / 2, box.y + box.height / 2]; if (last !== null) { done(Math.hypot(at[0] - last[0], at[1] - last[1]) < still && !document.getAnimations().some(animation => animation.pending && animation.timeline?.currentTime != null && chain.has(animation.effect?.target))); return; } last = at; requestAnimationFrame(frame); }; requestAnimationFrame(frame); setTimeout(() => done(null), within); }); }";
const FRAMES_WITHIN: Duration = Duration::from_millis(500);
const STILL_PIXELS: f64 = 1.0;
const ACTIONABLE_WITHIN: Duration = Duration::from_secs(5);
const ALIGN: &str =
    "function(at) { this.scrollIntoView({ block: at, inline: at, behavior: 'instant' }); }";
const ALIGNMENTS: [Option<&str>; 4] = [None, Some("end"), Some("center"), Some("start")];
const HIT: &str = "function(x, y) { const hit = this.getRootNode().elementFromPoint(x, y); return !!hit && (hit === this || this.contains(hit) || hit.closest('label')?.control === this); }";
const FOCUS_AND_SELECT: &str = "function() { this.focus(); if (this.select) { this.select(); } else { getSelection().selectAllChildren(this); } const active = this.getRootNode().activeElement; return active === this || this.contains(active); }";
const PICKERS: &str = "(() => { const picks = window.__iu2uPicks = {}; let last = 0; const ask = (kind, options) => new Promise((resolve, reject) => { const id = ++last; picks[id] = { resolve, reject }; __iu2uPick(JSON.stringify({ id, kind, multiple: !!(options && options.multiple) })); }); window.showOpenFilePicker = options => ask('file', options); window.showDirectoryPicker = options => ask('directory', options); })()";
const PICKED: &str = "(id, tree, many) => { const pick = window.__iu2uPicks[id]; delete window.__iu2uPicks[id]; if (!tree) { pick.reject(new DOMException('The user aborted a request.', 'AbortError')); return; } const bytes = text => Uint8Array.from(atob(text), c => c.charCodeAt(0)); const like = (handle, kind) => { const type = globalThis[kind]; if (type) Object.setPrototypeOf(handle, type.prototype); return handle; }; const common = { queryPermission: async () => 'granted', requestPermission: async () => 'granted' }; const file = node => { const blob = new File([bytes(node.data)], node.name, { type: node.type }); const handle = { ...common, kind: 'file', name: node.name, getFile: async () => blob, isSameEntry: async other => other === handle }; return like(handle, 'FileSystemFileHandle'); }; const directory = node => { const children = new Map(node.children.map(child => [child.name, child.children ? directory(child) : file(child)])); const get = kind => async name => { const child = children.get(name); if (!child || child.kind !== kind) throw new DOMException(name + ' was not found', child ? 'TypeMismatchError' : 'NotFoundError'); return child; }; const handle = { ...common, kind: 'directory', name: node.name, entries: async function* () { yield* children.entries(); }, keys: async function* () { yield* children.keys(); }, values: async function* () { yield* children.values(); }, [Symbol.asyncIterator]() { return this.entries(); }, getFileHandle: get('file'), getDirectoryHandle: get('directory'), isSameEntry: async other => other === handle }; return like(handle, 'FileSystemDirectoryHandle'); }; pick.resolve(tree.children ? directory(tree) : many ? tree.files.map(file) : [file(tree.files[0])]); }";
const NEW_TABS: &str = "[...document.links].filter(a => a.target && !['_self', '_parent', '_top'].includes(a.target.toLowerCase())).map(a => a.href)";
const MAX_PICKED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FRAMES: usize = 10;
const MAX_FAILURES: usize = 20;
const SCREENSHOT_SIDE: f64 = 1568.0;
const MAX_ZOOM: f64 = 4.0;
const VIEWPORT: (u32, u32) = (1280, 900);
const GRID: &str = "({ left, top, width, height, columns, rows, labels, scale }) => { document.getElementById('__iu2u_grid')?.remove(); const host = document.createElement('div'); host.id = '__iu2u_grid'; host.popover = 'manual'; host.style.cssText = `all: initial; position: fixed; left: ${left}px; top: ${top}px; width: ${width}px; height: ${height}px; z-index: 2147483647; pointer-events: none`; const root = host.attachShadow({ mode: 'closed' }); const [wide, high] = [width / columns, height / rows]; const size = Math.min(18, Math.max(11, Math.min(wide, high) * scale * 0.16)) / scale; const drawing = 'http://www.w3.org/2000/svg'; const svg = document.createElementNS(drawing, 'svg'); svg.setAttribute('width', width); svg.setAttribute('height', height); svg.style.cssText = 'position: absolute; left: 0; top: 0'; const lines = document.createElementNS(drawing, 'path'); lines.setAttribute('d', [...Array(columns + 1).keys()].map(column => `M${column * wide} 0V${height}`).concat([...Array(rows + 1).keys()].map(row => `M0 ${row * high}H${width}`)).join('')); lines.setAttribute('stroke', 'rgba(255, 0, 255, 0.75)'); lines.setAttribute('stroke-width', 1 / scale); lines.setAttribute('fill', 'none'); svg.append(lines); root.append(svg); labels.forEach((label, cell) => { const tag = document.createElement('span'); tag.textContent = label; tag.style.cssText = `position: absolute; left: ${(cell % columns) * wide}px; top: ${Math.floor(cell / columns) * high}px; font: bold ${size}px/1 sans-serif; color: #fff; background: rgba(0, 0, 0, 0.55); padding: ${1 / scale}px ${3 / scale}px`; root.append(tag); }); document.documentElement.append(host); host.showPopover?.(); return true; }";
const GRID_GONE: &str = "document.getElementById('__iu2u_grid')?.remove()";
const IMAGES_SHOWN: &str = "[...document.images].every(image => { const box = image.getBoundingClientRect(); return image.complete || box.bottom <= 0 || box.right <= 0 || box.top >= innerHeight || box.left >= innerWidth; })";
const IMAGES_WITHIN: Duration = Duration::from_secs(5);
const SHOT_WITHIN: Duration = Duration::from_secs(3);
const SHOT_TRIES: usize = 3;
const CLOCK: &str = "(() => { if (globalThis.__iu2uClock) return; const RealDate = Date, realSet = setTimeout, realClear = clearTimeout, realNow = Date.now, realPerformance = performance.now.bind(performance), timers = new Map(); let offset = 0, last = 0, pump, running = false, frozen, read = false; const now = () => realNow() + offset, date = () => frozen === undefined ? now() : (read = true, frozen), tick = () => { if (read) { frozen += 1; read = false; } }; const first = until => { let found; for (const timer of timers.values()) if (timer.at <= until && (!found || timer.at < found.at)) found = timer; return found; }; const fire = timer => { if (timer.every) timer.at += timer.every; else timers.delete(timer.id); try { typeof timer.run === 'function' ? Reflect.apply(timer.run, globalThis, timer.args) : (0, eval)(String(timer.run)); } catch (error) { realSet(() => { throw error; }); } }; const schedule = () => { realClear(pump); const next = first(Infinity); if (next) pump = realSet(() => { const due = first(now()); if (due) fire(due); schedule(); }, next.at - now()); }; const add = (run, ms, args, repeat) => { const delay = Math.max(running ? 1 : 0, Number(ms) || 0); timers.set(++last, { id: last, run, args, at: now() + delay, every: repeat && Math.max(1, delay) }); schedule(); return last; }; const clear = id => { if (timers.delete(id)) schedule(); }; const turn = () => new Promise(resolve => { const channel = new MessageChannel(); channel.port1.onmessage = resolve; channel.port2.postMessage(0); }); const run = async ms => { const end = now() + ms; await Promise.all(Array.from(frames, frame => { try { return frame.__iu2uClock?.run(ms); } catch {} })); running = true; for (let due; (due = first(end)); await turn()) { offset += Math.max(0, due.at - now()); fire(due); } running = false; offset += Math.max(0, end - now()); schedule(); }; function FakeDate(...args) { return new.target ? Reflect.construct(RealDate, args.length ? args : [date()], new.target) : new RealDate(date()).toString(); } FakeDate.prototype = RealDate.prototype; FakeDate.now = date; Object.setPrototypeOf(FakeDate, RealDate); performance.now = () => realPerformance() + offset; const stamp = Object.getOwnPropertyDescriptor(Event.prototype, 'timeStamp').get; Object.defineProperty(Event.prototype, 'timeStamp', { get() { return stamp.call(this) + offset; }, configurable: true }); Object.assign(globalThis, { Date: FakeDate, setTimeout: (run, ms, ...args) => add(run, ms, args, false), setInterval: (run, ms, ...args) => add(run, ms, args, true), clearTimeout: clear, clearInterval: clear }); Object.defineProperty(globalThis, '__iu2uClock', { value: { run, freeze: at => { frozen = at; for (const key in globalThis) if (key.startsWith('on')) addEventListener(key.slice(2), tick, true); } } }); })()";
const SCROLL_POINT: &str = "(() => { const shown = e => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth; }; const scrolls = [...document.querySelectorAll('*')].filter(e => e.scrollHeight - e.scrollTop - e.clientHeight > 1 && ['auto', 'scroll', 'overlay'].includes(getComputedStyle(e).overflowY) && shown(e)); const layered = scrolls.filter(e => e.closest('dialog[open], [role=dialog], [role=alertdialog], [role=listbox], [role=menu], [popover]')); const area = e => e.clientWidth * e.clientHeight; const center = e => { const r = e.getBoundingClientRect(); const left = Math.max(0, r.left), top = Math.max(0, r.top); return [(left + Math.min(innerWidth, r.right)) / 2, (top + Math.min(innerHeight, r.bottom)) / 2]; }; const open = layered.pop(); if (open) return [...center(open), true]; const page = document.scrollingElement; const locked = [document.documentElement, document.body].some(e => e && ['hidden', 'clip'].includes(getComputedStyle(e).overflowY)); if (page && !locked && page.scrollHeight - scrollY - innerHeight > 1) return [innerWidth / 2, innerHeight / 2, true]; const pick = scrolls.sort((a, b) => area(b) - area(a))[0]; if (pick) return [...center(pick), true]; const frame = [...document.querySelectorAll('iframe')].filter(f => { const inner = f.contentDocument?.scrollingElement; return shown(f) && (!inner || inner.scrollHeight - inner.scrollTop - inner.clientHeight > 1); }).sort((a, b) => area(b) - area(a))[0]; return frame ? [...center(frame), Boolean(frame.contentDocument)] : null; })()";
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
    session: String,
    owners: HashMap<String, String>,
    requests: HashMap<String, Instant>,
    loading: bool,
    dialogs: Vec<(String, bool)>,
    routes: Vec<Routed>,
    held: Vec<(String, String, String, Option<String>)>,
    held_network: HashSet<String>,
    opened: Option<String>,
    blank: HashSet<String>,
    chooser: Option<Chooser>,
    picker: Option<Picker>,
    given: Vec<(&'static str, &'static str, String)>,
    downloads: Vec<Download>,
    isolated: HashMap<String, String>,
    addresses: HashMap<String, String>,
    failures: Vec<(String, String)>,
    injected: Vec<String>,
    prepared: HashMap<String, String>,
    closed: bool,
}

struct Download {
    guid: String,
    name: String,
    state: &'static str,
    bytes: u64,
}

pub(super) struct Chooser {
    node: u64,
    session: Option<String>,
}

pub(super) struct Picker {
    id: u64,
    directory: bool,
    multiple: bool,
    context: u64,
    session: Option<String>,
}

pub(super) enum Handling {
    To(String),
    Answer {
        status: u16,
        body: Vec<u8>,
        content_type: String,
    },
    Hold,
}

struct Routed {
    pattern: String,
    once: bool,
    handling: Handling,
    hits: usize,
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
    frames: Vec<Frame>,
    behind: Vec<(String, String)>,
    painted: String,
    unread: bool,
}

struct Frame {
    session: String,
    owner: Option<(usize, usize)>,
    isolated: bool,
}

impl Browser {
    pub(super) async fn launch(headful: bool, dir: &Path) -> Result<Self> {
        let executable = find()?;
        let profile = dir.join(format!(
            "browser-{}",
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
                "--disable-backgrounding-occluded-windows",
            ])
            .arg(format!("--window-size={},{}", VIEWPORT.0, VIEWPORT.1))
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
        {
            let mut page = connection.page.lock().unwrap();
            page.main_frame = target.into();
            page.session = session.into();
            page.owners.insert(session.into(), session.into());
        }
        connection
            .send(
                None,
                "Target.setDiscoverTargets",
                json!({ "discover": true }),
            )
            .await?;
        connection
            .send(
                None,
                "Browser.setDownloadBehavior",
                json!({ "behavior": "allowAndName", "downloadPath": profile.join("downloads"), "eventsEnabled": true }),
            )
            .await?;
        let browser = Self {
            session: session.into(),
            connection,
            child,
            profile,
            frames: Vec::new(),
            behind: Vec::new(),
            painted: String::new(),
            unread: false,
        };
        browser.connection.size(&browser.session).await?;
        browser.connection.prepare(&browser.session, false).await?;
        browser.focus().await?;
        browser
            .connection
            .send(
                None,
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            )
            .await?;
        Ok(browser)
    }

    async fn focus(&self) -> Result<()> {
        self.call(
            "Emulation.setFocusEmulationEnabled",
            json!({ "enabled": true }),
        )
        .await?;
        Ok(())
    }

    pub(super) fn picker(&self) -> Option<Picker> {
        self.connection.page.lock().unwrap().picker.take()
    }

    pub(super) async fn pick(&self, picker: &Picker, paths: &[String]) -> Result<Vec<String>> {
        let mut budget = MAX_PICKED_BYTES;
        let given = fitting(paths, picker.directory, picker.multiple)?;
        let mut trees = given
            .iter()
            .map(|path| tree(Path::new(path), &mut budget))
            .collect::<Result<Vec<_>>>()?;
        let tree = if picker.directory {
            trees.pop().unwrap_or_default()
        } else {
            json!({ "files": trees })
        };
        self.answer_picker(picker, &tree, picker.multiple).await?;
        self.give("file picker", picker.directory, picker.multiple, &given);
        Ok(given)
    }

    fn give(&self, by: &'static str, folder: bool, many: bool, given: &[String]) {
        let takes = match (folder, many) {
            (true, _) => "a folder",
            (false, true) => "several files",
            (false, false) => "one file",
        };
        let entry = (by, takes, file_names(given));
        let mut page = self.connection.page.lock().unwrap();
        if !page.given.contains(&entry) {
            page.given.push(entry);
        }
    }

    pub(super) async fn refuse(&self, picker: &Picker) -> Result<()> {
        self.answer_picker(picker, &Value::Null, false).await
    }

    async fn answer_picker(&self, picker: &Picker, tree: &Value, many: bool) -> Result<()> {
        let before = self.revision().await;
        let started = Instant::now();
        let answered = self
            .connection
            .send(
                picker.session.as_deref(),
                "Runtime.evaluate",
                json!({ "expression": format!("({PICKED})({}, {tree}, {many})", picker.id), "contextId": picker.context }),
            )
            .await?;
        if let Some(thrown) = answered.get("exceptionDetails") {
            bail!("the page did not take the picked files: {}", thrown["text"]);
        }
        self.settle(before, started).await;
        Ok(())
    }

    pub(super) async fn inject(&self, source: String) -> Result<()> {
        let script = json!({ "source": source });
        self.call("Page.addScriptToEvaluateOnNewDocument", script.clone())
            .await?;
        let behind = self.behind.iter().map(|(session, _)| session.clone());
        for session in self.frame_sessions().into_iter().chain(behind) {
            self.connection
                .send(
                    Some(&session),
                    "Page.addScriptToEvaluateOnNewDocument",
                    script.clone(),
                )
                .await
                .ok();
        }
        self.connection.page.lock().unwrap().injected.push(source);
        Ok(())
    }

    async fn intercept(&self) -> Result<()> {
        self.connection.intercept(&self.session).await?;
        for frame in self.frame_sessions() {
            self.connection.intercept(&frame).await.ok();
        }
        Ok(())
    }

    fn frame_sessions(&self) -> Vec<String> {
        self.connection
            .page
            .lock()
            .unwrap()
            .isolated
            .values()
            .cloned()
            .collect()
    }

    pub(super) async fn install_clock(&self, frozen: bool) -> Result<()> {
        let freeze = if frozen {
            let at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
            format!("globalThis.__iu2uClock.freeze({at});")
        } else {
            String::new()
        };
        self.inject(format!("{CLOCK};{freeze}")).await
    }

    pub(super) async fn run_clock(&self, by: Duration) -> Result<()> {
        let (before, started) = (self.revision().await, Instant::now());
        let ran = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": format!("globalThis.__iu2uClock?.run({}).then(() => true)", by.as_millis()), "awaitPromise": true, "returnByValue": true }),
            )
            .await?;
        anyhow::ensure!(
            ran["result"]["value"] == true,
            "the page has no clock to run: put clock install before its open"
        );
        self.settle(before, started).await;
        Ok(())
    }

    pub(super) async fn scroll_point(&self) -> Result<Option<(f64, f64, bool)>> {
        let point = self.value(SCROLL_POINT).await?;
        Ok(point[0]
            .as_f64()
            .zip(point[1].as_f64())
            .map(|(x, y)| (x, y, point[2] == true)))
    }

    pub(super) async fn screenshot(&self) -> Result<String> {
        let started = Instant::now();
        while started.elapsed() < IMAGES_WITHIN
            && self
                .value(IMAGES_SHOWN)
                .await
                .is_ok_and(|shown| shown == false)
            && self.connection.page.lock().unwrap().held.is_empty()
        {
            tokio::time::sleep(surface::POLL).await;
        }
        let mut tried = 0;
        loop {
            tried += 1;
            match tokio::time::timeout(SHOT_WITHIN, self.capture()).await {
                Ok(Ok(shot)) => return Ok(shot),
                Ok(Err(error)) if tried == SHOT_TRIES => return Err(error),
                Err(_) if tried == SHOT_TRIES => bail!(
                    "the page did not hold still for a screenshot within {}s",
                    SHOT_WITHIN.as_secs()
                ),
                _ => {}
            }
        }
    }

    async fn view(&self) -> Result<[f64; 5]> {
        let view = self
            .value("[innerWidth, innerHeight, devicePixelRatio, scrollX, scrollY]")
            .await?;
        Ok(std::array::from_fn(|index| {
            view[index].as_f64().unwrap_or_default()
        }))
    }

    pub(super) async fn grid_shot(&self, grid: Grid) -> Result<String> {
        let [width, height, ratio, scroll_x, scroll_y] = self.view().await?;
        let ratio = ratio.max(1.0);
        let area = grid.area;
        let (left, top) = (area.left * width, area.top * height);
        let (wide, high) = (area.width * width, area.height * height);
        let count = grid.columns * grid.rows;
        let labels: Vec<String> = (0..count)
            .map(|cell| {
                decide::forward(if grid.reversed {
                    count - 1 - cell
                } else {
                    cell
                })
            })
            .collect();
        let scale = (SCREENSHOT_SIDE / (wide.max(high) * ratio)).min(MAX_ZOOM);
        let shown = json!({ "left": left, "top": top, "width": wide, "height": high, "columns": grid.columns, "rows": grid.rows, "labels": labels, "scale": scale * ratio });
        anyhow::ensure!(
            self.value(&format!("({GRID})({shown})")).await? == json!(true),
            "the page would not let act draw the grid to find a spot"
        );
        let shot = self
            .call(
                "Page.captureScreenshot",
                json!({ "format": "png", "clip": { "x": scroll_x + left, "y": scroll_y + top, "width": wide, "height": high, "scale": scale } }),
            )
            .await;
        self.value(GRID_GONE).await.ok();
        shot?["data"]
            .as_str()
            .map(str::to_owned)
            .context("the browser returned no screenshot")
    }

    async fn capture(&self) -> Result<String> {
        let [width, height, ratio, scroll_x, scroll_y] = self.view().await?;
        let scale = (SCREENSHOT_SIDE / (width.max(height) * ratio.max(1.0))).min(1.0);
        let shot = self
            .call(
                "Page.captureScreenshot",
                json!({ "format": "png", "clip": { "x": scroll_x, "y": scroll_y, "width": width, "height": height, "scale": scale } }),
            )
            .await?;
        shot["data"]
            .as_str()
            .map(str::to_owned)
            .context("the browser took no screenshot")
    }

    pub(super) fn forget_given(&self) {
        self.connection.page.lock().unwrap().given.clear();
    }

    pub(super) fn chooser(&self) -> Option<Chooser> {
        self.connection.page.lock().unwrap().chooser.take()
    }

    pub(super) async fn choose(&self, chooser: Chooser, paths: &[String]) -> Result<Vec<String>> {
        let session = chooser.session.as_deref();
        let described = self
            .connection
            .send(
                session,
                "DOM.describeNode",
                json!({ "backendNodeId": chooser.node }),
            )
            .await?;
        let attributes: Vec<&str> = described["node"]["attributes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let has = |name: &str| attributes.chunks(2).any(|pair| pair[0] == name);
        let (folder, many) = (has("webkitdirectory"), has("multiple"));
        let given = fitting(paths, folder, many)?;
        let before = self.revision().await;
        let started = Instant::now();
        self.connection
            .send(
                session,
                "DOM.setFileInputFiles",
                json!({ "files": given, "backendNodeId": chooser.node }),
            )
            .await?;
        self.settle(before, started).await;
        self.give("file chooser", folder, many, &given);
        Ok(given)
    }

    pub(super) async fn follow(&mut self) -> Result<()> {
        let (opened, closed) = {
            let mut page = self.connection.page.lock().unwrap();
            (page.opened.take(), std::mem::take(&mut page.closed))
        };
        if closed && let Some((session, target)) = self.behind.pop() {
            self.switch(session, target).await?;
        }
        let Some(target) = opened else {
            return Ok(());
        };
        let started = Instant::now();
        let session = loop {
            if let Some(session) = self
                .connection
                .page
                .lock()
                .unwrap()
                .prepared
                .remove(&target)
            {
                break session;
            }
            if self
                .connection
                .send(None, "Target.getTargetInfo", json!({ "targetId": target }))
                .await
                .is_err()
            {
                return Ok(());
            }
            anyhow::ensure!(
                started.elapsed() < REPLY_WITHIN,
                "the new tab did not start"
            );
            tokio::time::sleep(surface::POLL).await;
        };
        let main_frame = self.connection.page.lock().unwrap().main_frame.clone();
        self.behind.push((self.session.clone(), main_frame));
        if self.switch(session, target).await.is_err()
            && let Some((session, target)) = self.behind.pop()
        {
            self.switch(session, target).await?;
        }
        Ok(())
    }

    async fn switch(&mut self, session: String, target: String) -> Result<()> {
        self.session = session;
        {
            let mut page = self.connection.page.lock().unwrap();
            page.main_frame.clone_from(&target);
            page.session.clone_from(&self.session);
            page.requests.clear();
            page.addresses.clear();
            page.failures.clear();
            page.loading = false;
        }
        self.intercept().await?;
        self.focus().await?;
        self.connection
            .send(None, "Target.activateTarget", json!({ "targetId": target }))
            .await
            .ok();
        self.settle(None, Instant::now()).await;
        Ok(())
    }

    pub(super) async fn open(&mut self, url: &str) -> Result<()> {
        {
            let mut page = self.connection.page.lock().unwrap();
            page.dialogs.clear();
            page.given.clear();
            page.downloads.clear();
            page.failures.clear();
            page.addresses.clear();
        }
        let started = Instant::now();
        let opened = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(error) = opened["errorText"].as_str() {
            bail!("cannot open {url}: {error}");
        }
        self.settle(None, started).await;
        Ok(())
    }

    pub(super) async fn route(
        &mut self,
        pattern: String,
        once: bool,
        handling: Handling,
    ) -> Result<()> {
        {
            let mut page = self.connection.page.lock().unwrap();
            if !once {
                page.routes.retain(|route| {
                    route.pattern != pattern || matches!(route.handling, Handling::Hold)
                });
            }
            page.routes.push(Routed {
                pattern,
                once,
                handling,
                hits: 0,
            });
        }
        self.intercept().await
    }

    pub(super) async fn release(&mut self, pattern: &str) -> Result<usize> {
        let started = Instant::now();
        let held = {
            let mut page = self.connection.page.lock().unwrap();
            let before = page.routes.len();
            page.routes.retain(|route| {
                !(route.pattern == pattern && matches!(route.handling, Handling::Hold))
            });
            if page.routes.len() == before {
                bail!("no route holds {pattern}");
            }
            let (go, stay) = std::mem::take(&mut page.held)
                .into_iter()
                .partition::<Vec<_>, _>(|(held, _, _, _)| held == pattern);
            page.held = stay;
            for (_, _, network, _) in &go {
                page.held_network.remove(network);
                page.requests.insert(network.clone(), started);
            }
            go
        };
        self.intercept().await?;
        for (_, request, _, session) in &held {
            self.connection
                .send(
                    session.as_deref(),
                    "Fetch.continueRequest",
                    json!({ "requestId": request }),
                )
                .await
                .ok();
        }
        self.settle(None, started).await;
        Ok(held.len())
    }

    pub(super) async fn go(&mut self, delta: i64) -> Result<()> {
        let history = self.call("Page.getNavigationHistory", json!({})).await?;
        let entry = history["currentIndex"]
            .as_i64()
            .map(|index| index + delta)
            .and_then(|index| history["entries"].get(usize::try_from(index).ok()?))
            .with_context(|| {
                format!(
                    "there is no page to go {} to",
                    if delta < 0 { "back" } else { "forward" }
                )
            })?;
        let started = Instant::now();
        self.call(
            "Page.navigateToHistoryEntry",
            json!({ "entryId": entry["id"] }),
        )
        .await?;
        self.settle(None, started).await;
        Ok(())
    }

    pub(super) async fn reload(&mut self) -> Result<()> {
        let started = Instant::now();
        self.call("Page.reload", json!({})).await?;
        self.settle(None, started).await;
        Ok(())
    }

    pub(super) async fn url(&self) -> Result<String> {
        let history = self.call("Page.getNavigationHistory", json!({})).await?;
        history["currentIndex"]
            .as_u64()
            .and_then(|index| history["entries"].get(usize::try_from(index).ok()?))
            .and_then(|entry| entry["url"].as_str())
            .map(str::to_owned)
            .context("the browser did not say which page is open")
    }

    pub(super) async fn observe(&mut self) -> Result<Vec<Node>> {
        self.follow().await?;
        match self.read().await {
            Err(_) if self.connection.page.lock().unwrap().closed => {
                self.follow().await?;
                self.read().await
            }
            read => read,
        }
    }

    async fn read(&mut self) -> Result<Vec<Node>> {
        self.frames = vec![Frame {
            session: self.session.clone(),
            owner: None,
            isolated: false,
        }];
        let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let (clickable, mut painted) = self.snapshot(&self.session).await?;
        let mut arena = arena(&tree, 0, &clickable)?;
        let tabs = self.value(NEW_TABS).await?;
        let tabs: HashSet<&str> = tabs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for node in &mut arena {
            if node.role == "link" && tabs.contains(node.url.as_str()) {
                node.states.push("opens a new tab");
            }
        }
        let (mut clickables, none) = (
            HashMap::from([(self.session.clone(), clickable)]),
            HashSet::new(),
        );
        let (mut next, mut unread) = (0, false);
        while next < arena.len() {
            let framed = arena[next].role == "iframe";
            if framed
                && self.frames.len() <= MAX_FRAMES
                && let Ok((tree, sandbox)) = self.frame(arena[next].frame, arena[next].handle).await
                && let Some(frame) = self.frames.last()
                && let Ok(()) = match frame.isolated {
                    true => self.snapshot(&frame.session).await.map(|(own, text)| {
                        painted.push_str(&text);
                        clickables.insert(frame.session.clone(), own);
                    }),
                    false => Ok(()),
                }
                && let Ok(inner) = self::arena(
                    &tree,
                    self.frames.len() - 1,
                    clickables.get(&frame.session).unwrap_or(&none),
                )
            {
                if let Some(sandbox) = sandbox {
                    arena[next].value = format!("sandbox {sandbox}").trim_end().to_owned();
                }
                let offset = arena.len();
                arena.extend(inner.into_iter().map(|mut node| {
                    node.children.iter_mut().for_each(|child| *child += offset);
                    node
                }));
                arena[next].children.push(offset);
            } else if framed {
                unread = true;
            }
            next += 1;
        }
        self.painted = painted;
        self.unread = unread;
        let mut guard = self.connection.page.lock().unwrap();
        let page = &mut *guard;
        let failures = page.failures.iter().map(|(url, error)| Node {
            role: "request".into(),
            name: format!("failed: {url}"),
            value: error.clone(),
            ..Node::default()
        });
        let routes = page
            .routes
            .iter()
            .filter(|route| route.hits > 0)
            .map(|route| {
                let mut value = match route.hits {
                    1 => "1 request".to_string(),
                    hits => format!("{hits} requests"),
                };
                let held = page
                    .held
                    .iter()
                    .filter(|(pattern, ..)| *pattern == route.pattern)
                    .count();
                if held > 0 {
                    value.push_str(&format!(", {held} held until release"));
                }
                Node {
                    role: "route".into(),
                    name: route.pattern.clone(),
                    value,
                    ..Node::default()
                }
            });
        let given = page.given.iter().map(|(role, takes, names)| Node {
            role: (*role).into(),
            name: format!("for {takes}"),
            value: names.clone(),
            ..Node::default()
        });
        let downloads = page.downloads.iter().map(|download| Node {
            role: "download".into(),
            name: download.name.clone(),
            value: format!("{} bytes", download.bytes),
            states: vec![download.state],
            ..Node::default()
        });
        let dialogs = page.dialogs.iter_mut().map(|(name, shown)| {
            *shown = true;
            Node {
                role: "dialog".into(),
                name: name.clone(),
                ..Node::default()
            }
        });
        for node in failures
            .chain(routes)
            .chain(given)
            .chain(downloads)
            .chain(dialogs)
        {
            let at = arena.len();
            arena.push(node);
            arena[0].children.push(at);
        }
        Ok(arena)
    }

    pub(super) fn paints(&self, secret: &str) -> bool {
        let secret: String = secret.split_whitespace().collect();
        !secret.is_empty() && (self.unread || self.painted.contains(&secret.to_lowercase()))
    }

    async fn snapshot(&self, session: &str) -> Result<(HashSet<usize>, String)> {
        let snapshot = self
            .connection
            .send(
                Some(session),
                "DOMSnapshot.captureSnapshot",
                json!({ "computedStyles": [] }),
            )
            .await?;
        let string = |at: &Value| {
            at.as_u64()
                .and_then(|at| snapshot["strings"].get(usize::try_from(at).ok()?))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let (mut clickable, mut painted) = (HashSet::new(), String::new());
        for document in snapshot["documents"].as_array().into_iter().flatten() {
            let nodes = &document["nodes"];
            for at in nodes["isClickable"]["index"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(handle) = at
                    .as_u64()
                    .and_then(|at| nodes["backendNodeId"].get(usize::try_from(at).ok()?))
                    .and_then(Value::as_u64)
                {
                    clickable.insert(handle as usize);
                }
            }
            let layout = &document["layout"];
            let laid: HashSet<&Value> = layout["nodeIndex"]
                .as_array()
                .into_iter()
                .flatten()
                .collect();
            let password = |at: &Value| {
                at.as_u64()
                    .and_then(|at| nodes["attributes"].get(usize::try_from(at).ok()?))
                    .and_then(Value::as_array)
                    .is_some_and(|pairs| {
                        pairs.chunks(2).any(|pair| {
                            pair.iter()
                                .map(|at| string(at).to_ascii_lowercase())
                                .eq(["type", "password"])
                        })
                    })
            };
            let values = ["inputValue", "textValue"].iter().flat_map(|field| {
                let rare = &nodes[field];
                rare["index"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .zip(rare["value"].as_array().into_iter().flatten())
            });
            let shown = values
                .filter(|(at, _)| laid.contains(at) && !password(at))
                .map(|(_, text)| text);
            for text in layout["text"].as_array().into_iter().flatten().chain(shown) {
                painted.extend(string(text).to_lowercase().split_whitespace());
            }
        }
        Ok((clickable, painted))
    }

    async fn frame(&mut self, parent: usize, owner: usize) -> Result<(Value, Option<String>)> {
        let session = self.frames[parent].session.clone();
        let described = self
            .connection
            .send(
                Some(&session),
                "DOM.describeNode",
                json!({ "backendNodeId": owner }),
            )
            .await?;
        let frame = described["node"]["frameId"]
            .as_str()
            .context("the iframe has no document")?
            .to_owned();
        let sandbox = described["node"]["attributes"]
            .as_array()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .chunks(2)
            .find(|pair| pair[0] == "sandbox")
            .and_then(|pair| pair.get(1)?.as_str())
            .map(str::to_owned);
        let isolated = self
            .connection
            .page
            .lock()
            .unwrap()
            .isolated
            .get(&frame)
            .cloned();
        let (tree, frame) = match isolated {
            Some(isolated) => (
                self.connection
                    .send(Some(&isolated), "Accessibility.getFullAXTree", json!({}))
                    .await?,
                Frame {
                    session: isolated,
                    owner: Some((parent, owner)),
                    isolated: true,
                },
            ),
            None => (
                self.connection
                    .send(
                        Some(&session),
                        "Accessibility.getFullAXTree",
                        json!({ "frameId": frame }),
                    )
                    .await?,
                Frame {
                    session,
                    owner: Some((parent, owner)),
                    isolated: false,
                },
            ),
        };
        self.frames.push(frame);
        Ok((tree, sandbox))
    }

    async fn place(&self, frame: usize) -> Result<((f64, f64), (f64, f64))> {
        let Some(Frame {
            owner: Some((parent, owner)),
            isolated,
            ..
        }) = self.frames.get(frame)
        else {
            return Ok(((0.0, 0.0), (0.0, 0.0)));
        };
        let (origin, _) = Box::pin(self.place(*parent)).await?;
        let model = self
            .call_in(
                *parent,
                "DOM.getBoxModel",
                json!({ "backendNodeId": owner }),
            )
            .await?;
        let content = &model["model"]["content"];
        let document = (
            content[0].as_f64().unwrap_or_default() + origin.0,
            content[1].as_f64().unwrap_or_default() + origin.1,
        );
        Ok((if *isolated { document } else { origin }, document))
    }

    async fn call_in(&self, frame: usize, method: &str, params: Value) -> Result<Value> {
        let session = self
            .frames
            .get(frame)
            .map_or(&self.session, |frame| &frame.session);
        self.connection.send(Some(session), method, params).await
    }

    pub(super) async fn act(&mut self, action: &Action) -> Result<()> {
        self.connection
            .page
            .lock()
            .unwrap()
            .dialogs
            .retain(|(_, shown)| !shown);
        let (mut before, mut started) = (self.revision().await, Instant::now());
        match action {
            Action::Press(element) => {
                let (x, y) = self.point(element).await?;
                (before, started) = (self.revision().await, Instant::now());
                self.click(x, y).await?;
            }
            Action::Spot(x, y) => {
                let [width, height, ..] = self.view().await?;
                self.click(x * width, y * height).await?;
            }
            Action::Hover(element) => {
                let (x, y) = self.point(element).await?;
                (before, started) = (self.revision().await, Instant::now());
                self.call(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": x, "y": y }),
                )
                .await?;
            }
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
            Action::Escape => self.key("Escape", 27, "").await?,
            Action::Scroll => {
                if let Some((x, y, _)) = self.scroll_point().await? {
                    self.call(
                        "Input.dispatchMouseEvent",
                        json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": SCROLL_PIXELS }),
                    )
                    .await?;
                }
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
        self.value(WATCH).await.ok()?.as_f64()
    }

    async fn value(&self, expression: &str) -> Result<Value> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true }),
            )
            .await?;
        Ok(result["result"]["value"].clone())
    }

    async fn settle(&self, before: Option<f64>, started: Instant) {
        surface::settle(before, started, async || {
            let (busy, loading) = {
                let page = self.connection.page.lock().unwrap();
                let loading = page.loading && page.held.is_empty();
                let busy = loading
                    || page
                        .requests
                        .values()
                        .any(|at| *at >= started && at.elapsed() < LONG_REQUEST);
                (busy, loading)
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
            .call_in(
                element.frame,
                "Accessibility.getPartialAXTree",
                json!({ "backendNodeId": element.handle, "fetchRelatives": false }),
            )
            .await
            .with_context(|| format!("{} is gone", element.describe()))?;
        let node = &current["nodes"][0];
        let (role, mut name) = (role_of(node), text(&node["name"]));
        if named_by_contents(&name, &role, &element.states) {
            name = self.contents(element).await?;
        }
        if role != element.role || name != element.raw_name {
            bail!("{} changed before it could be used", element.describe());
        }
        Ok(text(&node["value"]))
    }

    async fn contents(&self, element: &Element) -> Result<String> {
        let subtree = self
            .call_in(
                element.frame,
                "Accessibility.queryAXTree",
                json!({ "backendNodeId": element.handle }),
            )
            .await?;
        let arena = arena(&subtree, element.frame, &HashSet::new())?;
        let at = arena
            .iter()
            .position(|node| node.handle == element.handle)
            .with_context(|| format!("{} is gone", element.describe()))?;
        let mut names = Vec::new();
        contents(&arena, at, &mut names);
        Ok(names.join(" "))
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
            .call_in(
                element.frame,
                "DOM.resolveNode",
                json!({ "backendNodeId": element.handle }),
            )
            .await?;
        let object = resolved["object"]["objectId"]
            .as_str()
            .with_context(|| format!("{} is gone", element.describe()))?;
        let result = self
            .call_in(
                element.frame,
                "Runtime.callFunctionOn",
                json!({ "objectId": object, "functionDeclaration": function, "arguments": arguments, "returnByValue": true, "awaitPromise": true }),
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

    async fn click(&self, x: f64, y: f64) -> Result<()> {
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

    async fn point(&self, element: &Element) -> Result<(f64, f64)> {
        let (started, mut tries) = (Instant::now(), 0);
        loop {
            self.fresh(element).await?;
            match ALIGNMENTS[tries % ALIGNMENTS.len()] {
                None => {
                    self.call_in(
                        element.frame,
                        "DOM.scrollIntoViewIfNeeded",
                        json!({ "backendNodeId": element.handle }),
                    )
                    .await?;
                }
                Some(at) => {
                    self.on(element, ALIGN, json!([{ "value": at }])).await?;
                }
            }
            let measured = self
                .on(
                    element,
                    STABLE,
                    json!([
                        { "value": FRAMES_WITHIN.as_secs_f64() * 1000.0 },
                        { "value": STILL_PIXELS }
                    ]),
                )
                .await?;
            let still = measured == json!(true);
            let (origin, document) = self.place(element.frame).await?;
            let quads = self
                .call_in(
                    element.frame,
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
            let (x, y) = (
                (left + right) / 2.0 + origin.0,
                (top + bottom) / 2.0 + origin.1,
            );
            let hit = self
                .on(
                    element,
                    HIT,
                    json!([{ "value": x - document.0 }, { "value": y - document.1 }]),
                )
                .await?
                == json!(true);
            if still && hit {
                return Ok((x, y));
            }
            if started.elapsed() >= ACTIONABLE_WITHIN {
                if measured.is_null() {
                    bail!(
                        "the page drew no frames, so it is unknown whether {} holds still",
                        element.describe()
                    );
                }
                if !still {
                    bail!("{} did not hold still", element.describe());
                }
                bail!("{} is covered by another element", element.describe());
            }
            tries += 1;
            tokio::time::sleep(surface::POLL).await;
        }
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
    async fn size(&self, session: &str) -> Result<()> {
        self.send(
            Some(session),
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": VIEWPORT.0, "height": VIEWPORT.1, "deviceScaleFactor": 0, "mobile": false }),
        )
        .await
        .map(drop)
    }

    async fn prepare(&self, session: &str, resume: bool) -> Result<()> {
        let injected = self.page.lock().unwrap().injected.clone();
        let mut commands = vec![
            ("Page.enable", json!({})),
            ("Network.enable", json!({})),
            ("Accessibility.enable", json!({})),
            ("Runtime.enable", json!({})),
            (
                "Emulation.setEmulatedMedia",
                json!({ "features": [{ "name": "prefers-reduced-motion", "value": "reduce" }] }),
            ),
            (
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": WATCH }),
            ),
            (
                "Page.setInterceptFileChooserDialog",
                json!({ "enabled": true }),
            ),
            ("Runtime.addBinding", json!({ "name": "__iu2uPick" })),
            (
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            ),
        ];
        commands.extend(
            std::iter::once(PICKERS)
                .chain(injected.iter().map(String::as_str))
                .map(|source| {
                    (
                        "Page.addScriptToEvaluateOnNewDocument",
                        json!({ "source": source }),
                    )
                }),
        );
        commands.extend(self.interception());
        if resume {
            commands.push(("Runtime.runIfWaitingForDebugger", json!({})));
        }
        try_join_all(
            commands
                .into_iter()
                .map(|(method, params)| self.send(Some(session), method, params)),
        )
        .await?;
        Ok(())
    }

    fn interception(&self) -> [(&'static str, Value); 2] {
        let patterns: Vec<Value> = self
            .page
            .lock()
            .unwrap()
            .routes
            .iter()
            .map(|route| json!({ "urlPattern": cdp_pattern(&route.pattern) }))
            .collect();
        [
            (
                "Network.setCacheDisabled",
                json!({ "cacheDisabled": !patterns.is_empty() }),
            ),
            if patterns.is_empty() {
                ("Fetch.disable", json!({}))
            } else {
                ("Fetch.enable", json!({ "patterns": patterns }))
            },
        ]
    }

    async fn intercept(&self, session: &str) -> Result<()> {
        for (method, params) in self.interception() {
            self.send(Some(session), method, params).await?;
        }
        Ok(())
    }

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
        let shown = event["sessionId"]
            .as_str()
            .and_then(|session| page.owners.get(session))
            == Some(&page.session);
        if event["method"] == "Network.requestWillBeSent" && shown {
            page.addresses
                .insert(text(&params["requestId"]), text(&params["request"]["url"]));
        }
        match event["method"].as_str().unwrap_or_default() {
            "Network.requestWillBeSent"
                if shown
                    && BUSY_REQUESTS.contains(&params["type"].as_str().unwrap_or_default())
                    && !page
                        .held_network
                        .contains(params["requestId"].as_str().unwrap_or_default()) =>
            {
                page.requests
                    .insert(text(&params["requestId"]), Instant::now());
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                let request = params["requestId"].as_str().unwrap_or_default();
                page.requests.remove(request);
                if page.held_network.remove(request) {
                    page.held.retain(|(_, _, network, _)| network != request);
                }
                let address = page.addresses.remove(request);
                if let Some(mut address) = address
                    && event["method"] == "Network.loadingFailed"
                    && params["canceled"] != true
                    && page.failures.len() < MAX_FAILURES
                {
                    address.truncate(address.find(['?', '#']).unwrap_or(address.len()));
                    if let Err(at) = page
                        .failures
                        .binary_search_by(|(failed, _)| failed.cmp(&address))
                    {
                        page.failures
                            .insert(at, (address, text(&params["errorText"])));
                    }
                }
            }
            "Page.frameStartedLoading" if main_frame => {
                page.loading = true;
                page.failures.clear();
                let state = &mut *page;
                forget_held(&mut state.held, &mut state.held_network, |from| {
                    from.and_then(|from| state.owners.get(from)) == Some(&state.session)
                });
            }
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
            "Fetch.requestPaused" => {
                let url = text(&params["request"]["url"]);
                let request = text(&params["requestId"]);
                let network = text(&params["networkId"]);
                let asked = &params["request"];
                let preflight = asked["method"] == "OPTIONS"
                    && header(asked, "access-control-request-method").is_some();
                let chosen =
                    page.routes.iter_mut().rev().find(|route| {
                        matches(&route.pattern, &url) && !(route.once && route.hits > 0)
                    });
                let (method, arguments) = match chosen {
                    None => ("Fetch.continueRequest", json!({ "requestId": request })),
                    Some(_) if preflight => (
                        "Fetch.fulfillRequest",
                        json!({ "requestId": request, "responseCode": 204, "responseHeaders": cors(asked, &url) }),
                    ),
                    Some(route) => {
                        route.hits += 1;
                        match &route.handling {
                            Handling::To(target) => (
                                "Fetch.continueRequest",
                                json!({ "requestId": request, "url": format!("{target}{}", &url[route.pattern.len()..]) }),
                            ),
                            Handling::Answer {
                                status,
                                body,
                                content_type,
                            } => (
                                "Fetch.fulfillRequest",
                                json!({
                                    "requestId": request,
                                    "responseCode": status,
                                    "responseHeaders": std::iter::once(json!({ "name": "content-type", "value": content_type })).chain(cors(asked, &url)).collect::<Vec<_>>(),
                                    "body": STANDARD.encode(body),
                                }),
                            ),
                            Handling::Hold => {
                                let pattern = route.pattern.clone();
                                page.requests.remove(&network);
                                page.held_network.insert(network.clone());
                                let session = event["sessionId"].as_str().map(str::to_owned);
                                page.held.push((pattern, request, network, session));
                                return;
                            }
                        }
                    }
                };
                let connection = self.clone();
                let session = event["sessionId"].as_str().map(str::to_owned);
                tokio::spawn(async move {
                    connection
                        .send(session.as_deref(), method, arguments)
                        .await
                        .ok();
                });
            }
            "Page.fileChooserOpened" => {
                page.chooser = params["backendNodeId"].as_u64().map(|node| Chooser {
                    node,
                    session: event["sessionId"].as_str().map(str::to_owned),
                });
            }
            "Browser.downloadWillBegin" => page.downloads.push(Download {
                guid: text(&params["guid"]),
                name: text(&params["suggestedFilename"]),
                state: "downloading",
                bytes: 0,
            }),
            "Browser.downloadProgress" => {
                let guid = text(&params["guid"]);
                if let Some(download) = page
                    .downloads
                    .iter_mut()
                    .find(|download| download.guid == guid)
                {
                    download.bytes = params["receivedBytes"].as_u64().unwrap_or(download.bytes);
                    download.state = match params["state"].as_str() {
                        Some("completed") => "downloaded",
                        Some("canceled") => "canceled",
                        _ => "downloading",
                    };
                }
            }
            "Runtime.bindingCalled" if params["name"] == "__iu2uPick" => {
                if let Ok(asked) =
                    serde_json::from_str::<Value>(params["payload"].as_str().unwrap_or_default())
                {
                    page.picker = asked["id"].as_u64().map(|id| Picker {
                        id,
                        directory: asked["kind"] == "directory",
                        multiple: asked["multiple"] == true,
                        context: params["executionContextId"].as_u64().unwrap_or_default(),
                        session: event["sessionId"].as_str().map(str::to_owned),
                    });
                }
            }
            "Target.attachedToTarget" => {
                let session = text(&params["sessionId"]);
                let target = text(&params["targetInfo"]["targetId"]);
                let kind = text(&params["targetInfo"]["type"]);
                let waiting = params["waitingForDebugger"] == true;
                let owner = if kind == "page" {
                    Some(session.clone())
                } else {
                    event["sessionId"]
                        .as_str()
                        .and_then(|parent| page.owners.get(parent))
                        .cloned()
                };
                if let Some(owner) = owner {
                    page.owners.insert(session.clone(), owner);
                }
                if kind == "iframe" {
                    page.isolated.insert(target.clone(), session.clone());
                } else if !waiting {
                    return;
                }
                let connection = self.clone();
                tokio::spawn(async move {
                    if kind == "page" {
                        let (sized, prepared) = tokio::join!(
                            connection.size(&session),
                            connection.prepare(&session, waiting)
                        );
                        sized.and(prepared).ok();
                    } else if kind == "iframe" {
                        connection.prepare(&session, waiting).await.ok();
                    } else {
                        connection
                            .send(Some(&session), "Runtime.runIfWaitingForDebugger", json!({}))
                            .await
                            .ok();
                    }
                    if kind == "page" {
                        connection
                            .page
                            .lock()
                            .unwrap()
                            .prepared
                            .insert(target, session);
                    }
                });
            }
            "Target.detachedFromTarget" => {
                let session = text(&params["sessionId"]);
                page.isolated.retain(|_, attached| *attached != session);
                page.owners.remove(&session);
                let state = &mut *page;
                forget_held(&mut state.held, &mut state.held_network, |from| {
                    from == Some(session.as_str())
                });
                page.closed |= params["targetId"] == page.main_frame.as_str();
                if page.opened.as_deref() == params["targetId"].as_str() {
                    page.opened = None;
                }
            }
            "Target.targetInfoChanged" => {
                let info = &params["targetInfo"];
                let target = text(&info["targetId"]);
                if addressed(&info["url"]) && page.blank.remove(&target) {
                    page.opened = Some(target);
                }
            }
            "Target.targetCreated"
                if params["targetInfo"]["type"] == "page"
                    && params["targetInfo"]["targetId"] != page.main_frame.as_str() =>
            {
                let info = &params["targetInfo"];
                if info["openerId"] == page.main_frame.as_str() {
                    let target = text(&info["targetId"]);
                    if addressed(&info["url"]) {
                        page.opened = Some(target);
                        return;
                    }
                    page.blank.insert(target);
                }
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

fn forget_held(
    held: &mut Vec<(String, String, String, Option<String>)>,
    held_network: &mut HashSet<String>,
    gone: impl Fn(Option<&str>) -> bool,
) {
    held.retain(|(_, _, network, from)| {
        let gone = gone(from.as_deref());
        if gone {
            held_network.remove(network);
        }
        !gone
    });
}

fn fitting(paths: &[String], folder: bool, many: bool) -> Result<Vec<String>> {
    let fit: Vec<String> = paths
        .iter()
        .filter(|path| match folder {
            true => Path::new(path).is_dir(),
            false => Path::new(path).is_file(),
        })
        .take(if many && !folder { paths.len() } else { 1 })
        .cloned()
        .collect();
    anyhow::ensure!(
        !fit.is_empty(),
        "the page asked for {}, but the goal names only {}",
        if folder { "a folder" } else { "files" },
        if folder { "files" } else { "folders" }
    );
    Ok(fit)
}

pub(super) fn file_names(paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| {
            Path::new(path)
                .file_name()
                .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn tree(path: &Path, budget: &mut u64) -> Result<Value> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let metadata =
        std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if metadata.is_dir() {
        let mut children = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(path)?.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_file())
            {
                children.push(tree(&entry.path(), budget)?);
            }
        }
        return Ok(json!({ "name": name, "children": children }));
    }
    *budget = budget.checked_sub(metadata.len()).with_context(|| {
        format!(
            "the files given to the page pass {} MiB",
            MAX_PICKED_BYTES / 1024 / 1024
        )
    })?;
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let kind = mime_guess::from_path(path).first_raw().unwrap_or_default();
    Ok(json!({ "name": name, "type": kind, "data": STANDARD.encode(&bytes) }))
}

fn arena(tree: &Value, frame: usize, clickable: &HashSet<usize>) -> Result<Vec<Node>> {
    let nodes = tree["nodes"].as_array().context("no accessibility tree")?;
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(position, node)| Some((node["nodeId"].as_str()?, position)))
        .collect();
    let mut arena: Vec<Node> = nodes
        .iter()
        .map(|node| Node {
            frame,
            ..to_node(node, nodes, &index)
        })
        .collect();
    let wrappers = page_wrappers(&arena);
    let candidates: HashSet<usize> = (0..arena.len())
        .filter(|&at| {
            let node = &arena[at];
            let generic = text(&nodes[at]["role"]) == "generic"
                && !nodes[at]["ignored"].as_bool().unwrap_or(false);
            node.kind.is_none()
                && clickable.contains(&node.handle)
                && !wrappers.contains(&at)
                && (generic || !["", "text", "page", "iframe"].contains(&node.role.as_str()))
        })
        .collect();
    for at in 0..arena.len() {
        if candidates.contains(&at) && !holds_control(&arena, at, &candidates) {
            if arena[at].role.is_empty() {
                arena[at].role = "generic".into();
            }
            arena[at].kind = Some(Kind::Press);
            arena[at].states.push("clickable");
        }
        if named_by_contents(&arena[at].name, &arena[at].role, &arena[at].states) {
            let mut names = Vec::new();
            contents(&arena, at, &mut names);
            arena[at].name = names.join(" ");
        }
    }
    Ok(arena)
}

fn page_wrappers(arena: &[Node]) -> Vec<usize> {
    fn filled(arena: &[Node], at: usize) -> bool {
        arena.get(at).is_some_and(|node| {
            !node.name.is_empty()
                || !node.value.is_empty()
                || node.children.iter().any(|&child| filled(arena, child))
        })
    }
    let (mut chain, mut at) = (Vec::new(), 0);
    while chain.len() < arena.len() {
        let parts: Vec<usize> = arena.get(at).map_or_else(Vec::new, |node| {
            node.children
                .iter()
                .copied()
                .filter(|&child| filled(arena, child))
                .collect()
        });
        match parts[..] {
            [only] => {
                chain.push(only);
                at = only;
            }
            [] => return Vec::new(),
            _ => return chain,
        }
    }
    Vec::new()
}

fn holds_control(arena: &[Node], at: usize, candidates: &HashSet<usize>) -> bool {
    arena[at].children.iter().any(|&child| {
        arena.get(child).is_some_and(|node| {
            node.kind.is_some()
                || candidates.contains(&child)
                || holds_control(arena, child, candidates)
        })
    })
}

fn named_by_contents(name: &str, role: &str, states: &[&str]) -> bool {
    name.is_empty() && (role == "row" || states.contains(&"clickable"))
}

fn contents(arena: &[Node], at: usize, names: &mut Vec<String>) {
    for &child in &arena[at].children {
        match arena.get(child) {
            Some(node) if !node.name.is_empty() => names.push(node.name.clone()),
            Some(_) => contents(arena, child, names),
            None => {}
        }
    }
}

fn addressed(url: &Value) -> bool {
    url.as_str()
        .is_some_and(|url| !url.is_empty() && url != "about:blank")
}

fn cdp_pattern(pattern: &str) -> String {
    let escaped = pattern.replace('\\', "\\\\").replace('?', "\\?");
    if pattern.contains('*') {
        escaped
    } else {
        format!("{escaped}*")
    }
}

fn matches(pattern: &str, url: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return url.starts_with(first);
    }
    if !url.starts_with(first) || url.len() < first.len() + last.len() || !url.ends_with(last) {
        return false;
    }
    let mut rest = &url[first.len()..url.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

pub(super) fn content_type(path: &str) -> String {
    let kind = mime_guess::from_path(path).first_or_octet_stream();
    if kind.type_() == mime_guess::mime::TEXT {
        format!("{kind}; charset=utf-8")
    } else {
        kind.to_string()
    }
}

fn header<'a>(request: &'a Value, name: &str) -> Option<&'a str> {
    request["headers"]
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?
        .1
        .as_str()
}

fn cors(request: &Value, url: &str) -> Vec<Value> {
    let own = reqwest::Url::parse(url)
        .ok()
        .map(|url| url.origin().ascii_serialization());
    let Some(origin) = header(request, "origin").filter(|origin| own.as_deref() != Some(*origin))
    else {
        return Vec::new();
    };
    [
        ("access-control-allow-origin", Some(origin)),
        ("access-control-allow-credentials", Some("true")),
        ("vary", Some("Origin")),
        (
            "access-control-allow-methods",
            header(request, "access-control-request-method"),
        ),
        (
            "access-control-allow-headers",
            header(request, "access-control-request-headers"),
        ),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some(json!({ "name": name, "value": value? })))
    .collect()
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

pub(super) fn sweep(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|name| name.strip_prefix(SUITE_PREFIX))
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

pub(super) fn private_dir(path: &Path) -> std::io::Result<()> {
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
    let busy =
        property(node, "busy").is_some_and(|busy| busy == true || busy == "true" || busy == 1);
    let shown = !node["ignored"].as_bool().unwrap_or(false)
        && (!HIDDEN.contains(&role.as_str()) || (role == "generic" && busy));
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
    if busy {
        states.push("busy");
    }
    let value = text(&node["value"]);
    if kind == Some(Kind::Type) && value.is_empty() {
        states.push("empty");
    }
    let name = text(&node["name"]);
    let role = match role.as_str() {
        "StaticText" => "text".into(),
        "RootWebArea" => "page".into(),
        "Iframe" | "IframePresentational" => "iframe".into(),
        _ if shown => role,
        _ => String::new(),
    };
    let url = property(node, "url")
        .and_then(Value::as_str)
        .filter(|_| !role.is_empty())
        .unwrap_or_default()
        .to_owned();
    let description = if role.is_empty() {
        String::new()
    } else {
        text(&node["description"])
    };
    Node {
        name: if role.is_empty() { String::new() } else { name },
        role,
        value,
        url,
        description,
        states,
        kind: kind.filter(|_| shown),
        handle: node["backendDOMNodeId"].as_u64().unwrap_or_default() as usize,
        frame: 0,
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
    fn folders_of_runs_that_died_are_swept_and_live_ones_kept() {
        let dir = std::env::temp_dir().join(format!("iu2u-sweep-test-{}", std::process::id()));
        let dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait_with_output().unwrap();
        let suite = |pid: u32| format!("{SUITE_PREFIX}{pid}");
        for kept in [suite(dead_pid), suite(std::process::id()), "other".into()] {
            std::fs::create_dir_all(dir.join(kept).join("Default")).unwrap();
        }
        sweep(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        let mut expected = vec![suite(std::process::id()), "other".to_string()];
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
    fn route_addresses_match_by_their_start_or_their_stars() {
        assert!(matches(
            "https://login.example.com",
            "https://login.example.com/t/authorize?x=1"
        ));
        assert!(!matches(
            "https://login.example.com",
            "http://login.example.com/"
        ));
        assert!(matches("*/api/prototypes*", "http://h/api/prototypes?q=1"));
        assert!(matches("*/captures/*.png", "http://h/captures/a/v1.png"));
        assert!(!matches(
            "*/captures/*.png",
            "http://h/captures/a/v1.png?retry=1"
        ));
        assert!(!matches("*/api/a*b", "http://h/api/b"));
        assert_eq!(cdp_pattern("https://a.test/x?y"), "https://a.test/x\\?y*");
        assert_eq!(cdp_pattern("*/api/*"), "*/api/*");
    }

    #[test]
    fn a_linked_folder_is_given_as_the_folder_it_points_to() {
        let root = std::env::temp_dir().join(format!("iu2u-tree-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("current")).unwrap();
        std::os::unix::net::UnixListener::bind(root.join("real/s.sock")).unwrap();
        let given = tree(&root.join("current"), &mut 1024);
        std::fs::remove_dir_all(&root).ok();
        let given = given.unwrap();
        assert_eq!(given["children"][0]["name"], "a.txt");
        assert_eq!(given["children"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn files_take_the_type_their_extension_names() {
        for (path, kind) in [
            ("contract.pdf", "application/pdf"),
            ("app.wasm", "application/wasm"),
            ("index.html", "text/html; charset=utf-8"),
            ("data.json", "application/json"),
            ("site.zip", "application/zip"),
            ("README", "application/octet-stream"),
        ] {
            assert_eq!(content_type(path), kind, "{path}");
        }
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
