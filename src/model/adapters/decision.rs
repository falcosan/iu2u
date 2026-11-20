use std::ffi::CString;
use std::fs::{File, TryLockError};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};

use super::{Reference, UNSUPPORTED};
use crate::commands::tail;
use crate::config::{self, Config};
use crate::write_atomically;

pub(crate) const BUILD: &str = "b12359";
const RELEASES: &str = "https://github.com/ggml-org/llama.cpp/releases/download";
const HUGGING_FACE: &str = "https://huggingface.co";
const ARGS: [&str; 8] = [
    "-c",
    "32768",
    "--no-webui",
    "--offline",
    "--log-verbosity",
    "1",
    "--sleep-idle-seconds",
    "300",
];
const READY_WITHIN: Duration = Duration::from_secs(120);
const POLL_EVERY: Duration = Duration::from_millis(100);
const RESPAWN_AFTER: Duration = Duration::from_secs(1);
const PROPS_EVERY: Duration = Duration::from_secs(1);
const STOP_WITHIN: Duration = Duration::from_secs(5);
const ASK_SERVER_WITHIN: Duration = Duration::from_secs(2);
const DISK_MARGIN: u64 = 1 << 30;
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
pub(crate) const LOCAL_URL: &str = "http://localhost/v1/systemone";
const MISSING_LIBRARIES: [(&str, &str); 4] = [
    ("libgomp.so.1", "the GNU OpenMP runtime, package libgomp1"),
    ("libssl.so.3", "OpenSSL 3, package libssl3t64 or libssl3"),
    ("libcrypto.so.3", "OpenSSL 3, package libssl3t64 or libssl3"),
    (
        "GLIBC_",
        "glibc 2.35 or later on x86-64, 2.38 or later on arm64",
    ),
];

struct Archive {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

impl Archive {
    fn url(&self) -> String {
        format!("{RELEASES}/{BUILD}/{}", self.name)
    }
}

const ARCHIVE: Option<Archive> = if cfg!(not(local_model)) {
    None
} else if cfg!(target_os = "macos") {
    Some(Archive {
        name: "llama-b12359-bin-macos-arm64.tar.gz",
        size: 16_907_811,
        sha256: "82b00abc740160e1b9980e6cc79c117345fa34af058c80e6c0a69aa8ddccde75",
    })
} else if cfg!(target_arch = "x86_64") {
    Some(Archive {
        name: "llama-b12359-bin-ubuntu-x64.tar.gz",
        size: 23_311_084,
        sha256: "0bc1f091e8916102b6d8f2cf944233a620e90a0f856c72d0ce41150c65c2e72e",
    })
} else {
    Some(Archive {
        name: "llama-b12359-bin-ubuntu-arm64.tar.gz",
        size: 18_949_862,
        sha256: "dc7aa17a67884619b7223870aca9fd6addbc01717713b04a714add1c31137359",
    })
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) file: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

pub(crate) fn built_in() -> Record {
    Record {
        repository: "bartowski/fastino_GLiDE-9B-GGUF".into(),
        revision: "88e7a44e35b3bf38ee716042d61deaa923c92359".into(),
        file: "fastino_GLiDE-9B-Q4_K_M.gguf".into(),
        size: 5_841_055_424,
        sha256: "371adada0c5b4d0193aa3212b831fdf04cf8f5889a51a9fc1012a9ecc08eb4be".into(),
    }
}

impl Reference {
    pub(crate) fn installed(&self, home: &Path) -> Option<Record> {
        match self {
            Self::BuiltIn => {
                Some(built_in()).filter(|record| model_path(home, &record.sha256).is_file())
            }
            Self::Linked {
                repository,
                selector,
                ..
            } => {
                let dir = home.join("models").join(repository);
                let names: Vec<String> = std::fs::read_dir(&dir)
                    .ok()?
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().into_string().ok()?;
                        name.strip_suffix(".json").map(str::to_owned)
                    })
                    .collect();
                let file = pick(&names, Some(selector.as_deref().unwrap_or("Q4_K_M"))).ok()?;
                read_record(home, &dir.join(format!("{file}.json")))
            }
        }
    }
}

pub(crate) fn pick<'a>(files: &'a [String], selector: Option<&str>) -> Result<&'a str> {
    let ggufs: Vec<&'a String> = files
        .iter()
        .filter(|file| file.to_lowercase().ends_with(".gguf"))
        .collect();
    let matching = |selector: &str| -> Vec<&'a String> {
        let (selector, suffix) = (
            selector.to_lowercase(),
            format!("{}.gguf", selector.to_lowercase()),
        );
        ggufs
            .iter()
            .copied()
            .filter(|file| {
                let file = file.to_lowercase();
                file == selector
                    || file
                        .strip_suffix(&suffix)
                        .is_some_and(|stem| stem.is_empty() || stem.ends_with(['-', '.', '_']))
            })
            .collect()
    };
    let chosen = match selector {
        Some(selector) => matching(selector),
        None => match matching("Q4_K_M").as_slice() {
            [] => ggufs.first().copied().into_iter().collect(),
            some => some.to_vec(),
        },
    };
    match chosen.as_slice() {
        [file] => Ok(file.as_str()),
        _ => bail!(
            "{} the .gguf files are: {}",
            match selector {
                Some(selector) if chosen.is_empty() => format!("no file matches {selector};"),
                Some(selector) => format!("more than one file matches {selector};"),
                None if chosen.is_empty() => "the repository has no .gguf file;".into(),
                None => "more than one Q4_K_M file;".into(),
            },
            if ggufs.is_empty() {
                "none".into()
            } else {
                ggufs
                    .iter()
                    .map(|file| file.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    }
}

pub(crate) fn supported() -> Result<()> {
    ensure!(ARCHIVE.is_some(), UNSUPPORTED);
    Ok(())
}

pub(crate) fn home() -> Result<PathBuf> {
    config::data_dir()
        .context("the local decision model needs a data directory: set XDG_DATA_HOME or HOME")
}

pub(crate) fn model_path(home: &Path, sha256: &str) -> PathBuf {
    home.join("models").join(sha256)
}

pub(crate) fn runtime_dir(home: &Path) -> PathBuf {
    home.join("runtime").join(BUILD)
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug)]
pub(crate) struct Server {
    home: PathBuf,
    sha256: String,
    identity: String,
    socket: PathBuf,
    lock: PathBuf,
    log: PathBuf,
    http: reqwest::Client,
    attempts: Arc<AtomicU64>,
    outcome: Arc<tokio::sync::Mutex<std::result::Result<(), String>>>,
}

impl Server {
    pub(crate) fn new(home: &Path, sha256: &str) -> Result<Self> {
        supported()?;
        ensure!(is_sha256(sha256), "{sha256} is not a SHA-256");
        let identity = hex(&Sha256::digest(format!(
            "{BUILD}\n{sha256}\n{}",
            ARGS.join(" ")
        )))[..16]
            .to_owned();
        let run = home.join("run");
        let socket = run.join(format!("{identity}.sock"));
        ensure!(
            std::os::unix::net::SocketAddr::from_pathname(&socket).is_ok(),
            "the socket path {} is longer than this system allows; set XDG_DATA_HOME to a shorter absolute path",
            socket.display()
        );
        Ok(Self {
            http: reqwest::Client::builder()
                .unix_socket(socket.as_path())
                .build()?,
            home: home.into(),
            sha256: sha256.into(),
            lock: run.join(format!("{identity}.lock")),
            log: run.join(format!("{identity}.log")),
            socket,
            attempts: Arc::default(),
            outcome: Arc::new(tokio::sync::Mutex::new(Ok(()))),
            identity,
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub(crate) async fn asked(&self, path: &str) -> Option<(StatusCode, Value)> {
        let response = self
            .http
            .get(format!("http://localhost{path}"))
            .timeout(ASK_SERVER_WITHIN)
            .send()
            .await
            .ok()?;
        let status = response.status();
        Some((status, response.json().await.unwrap_or(Value::Null)))
    }

    pub(crate) fn supervised(&self) -> Result<bool> {
        run_dir(&self.home)?;
        Ok(locked(&self.lock)?.is_none())
    }

    pub(crate) fn spawn(&self) -> Result<()> {
        let program = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe()?
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(["serve", &self.sha256, &self.identity])
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
            .spawn()
            .context("cannot start the local decision model")?;
        Ok(())
    }

    async fn started(&self) -> Result<()> {
        let seen = self.attempts.load(Ordering::SeqCst);
        let mut outcome = self.outcome.lock().await;
        if self.attempts.load(Ordering::SeqCst) == seen {
            *outcome = self.ready().await.map_err(|error| format!("{error:#}"));
            self.attempts.fetch_add(1, Ordering::SeqCst);
        }
        outcome
            .clone()
            .map_err(|error| std::io::Error::new(ErrorKind::NotFound, error).into())
    }

    async fn ready(&self) -> Result<()> {
        let started = Instant::now();
        let mut spawned: Option<SystemTime> = None;
        loop {
            if let Some((StatusCode::OK, _)) = self.asked("/health").await {
                return Ok(());
            }
            if !self.supervised()? {
                if let Some(at) = spawned
                    && std::fs::metadata(&self.log)
                        .and_then(|log| log.modified())
                        .is_ok_and(|modified| modified >= at)
                {
                    return Err(self.failed("stopped before it was ready"));
                }
                if spawned.is_none_or(|at| at.elapsed().unwrap_or_default() >= RESPAWN_AFTER) {
                    spawned = Some(SystemTime::now());
                    self.spawn()?;
                }
            }
            if started.elapsed() >= READY_WITHIN {
                return Err(
                    self.failed(&format!("was not ready within {}s", READY_WITHIN.as_secs()))
                );
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
    }

    fn failed(&self, what: &str) -> anyhow::Error {
        let tail = tail(&self.log);
        let needs = MISSING_LIBRARIES
            .iter()
            .find(|(library, _)| tail.contains(library))
            .map(|(_, needs)| format!("; the runtime needs {needs}"))
            .unwrap_or_default();
        let said = if tail.is_empty() {
            String::new()
        } else {
            format!(", and its log says:\n{tail}")
        };
        std::io::Error::new(
            ErrorKind::NotFound,
            format!(
                "the local decision model {what}{needs}; see {}{said}",
                self.log.display()
            ),
        )
        .into()
    }

    pub(crate) async fn send<F, R>(&self, send: F) -> Result<reqwest::Response>
    where
        F: Fn() -> R,
        R: Future<Output = reqwest::Result<reqwest::Response>>,
    {
        match send().await {
            Ok(response) if response.status() != StatusCode::SERVICE_UNAVAILABLE => Ok(response),
            Err(error) if !lost(&error) => Err(error.into()),
            _ => {
                self.started().await?;
                Ok(send().await?)
            }
        }
    }
}

fn lost(error: &reqwest::Error) -> bool {
    error.is_connect() || (error.is_request() && !error.is_timeout())
}

pub(crate) fn run_dir(home: &Path) -> Result<PathBuf> {
    let run = home.join("run");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .with_context(|| format!("cannot create {}", run.display()))?;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot protect {}", run.display()))?;
    Ok(run)
}

pub(crate) fn from_model(model: &str) -> Result<Option<Server>> {
    let Some(reference) = Reference::parse(model)? else {
        return Ok(None);
    };
    supported()?;
    let home = home()?;
    let record = reference.installed(&home).with_context(|| {
        format!("the local decision model {model} is not installed: run iu2u init")
    })?;
    Ok(Some(Server::new(&home, &record.sha256)?))
}

pub(crate) async fn stop(socket: &Path) -> bool {
    let Some(pid) = peer(socket).await else {
        return false;
    };
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let (started, mut killed) = (Instant::now(), false);
    while peer(socket).await == Some(pid) {
        if !killed && started.elapsed() >= STOP_WITHIN {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            killed = true;
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
    true
}

async fn peer(socket: &Path) -> Option<i32> {
    let stream = UnixStream::connect(socket).await.ok()?;
    stream.peer_cred().ok()?.pid().filter(|pid| *pid > 0)
}

pub async fn serve(sha256: &str, identity: &str) -> Result<()> {
    ensure!(
        identity.len() == 16 && identity.bytes().all(|b| b.is_ascii_hexdigit()),
        "{identity} is not a server identity"
    );
    let home = home()?;
    let server = Server::new(&home, sha256)?;
    let run = run_dir(&home)?;
    if server.identity != identity {
        std::fs::write(
            run.join(format!("{identity}.log")),
            "iu2u: the program that asked for this model started before iu2u was replaced by a version with another llama.cpp build; restart that program\n",
        )?;
        return Ok(());
    }
    let Some(_lock) = locked(&server.lock)? else {
        return Ok(());
    };
    let mut log = File::create(&server.log)?;
    if let Err(error) = supervise(&server, &log).await {
        use std::io::Write;
        writeln!(log, "iu2u: {error:#}").ok();
    }
    std::fs::remove_file(&server.socket).ok();
    Ok(())
}

async fn supervise(server: &Server, log: &File) -> Result<()> {
    let runtime = install_runtime(&server.home, &mut |_, _| {}).await?;
    stop(&server.socket).await;
    std::fs::remove_file(&server.socket).ok();
    let mut command = tokio::process::Command::new(runtime.join("llama-server"));
    command
        .arg("-m")
        .arg(model_path(&server.home, &server.sha256))
        .arg("--host")
        .arg(&server.socket)
        .args(ARGS)
        .current_dir(server.socket.parent().unwrap_or(&server.home))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("LLAMA_") || name.to_string_lossy().starts_with("HF_")
        {
            command.env_remove(&name);
        }
    }
    let mut child = command.spawn().context("cannot run llama-server")?;
    let (mut terminate, mut interrupt, mut hangup) = (
        signal(SignalKind::terminate())?,
        signal(SignalKind::interrupt())?,
        signal(SignalKind::hangup())?,
    );
    loop {
        tokio::select! {
            _ = child.wait() => break,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = hangup.recv() => break,
            () = tokio::time::sleep(PROPS_EVERY) => {
                if let Some((StatusCode::OK, props)) = server.asked("/props").await
                    && props["is_sleeping"] == true
                {
                    break;
                }
            }
        }
    }
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        if tokio::time::timeout(STOP_WITHIN, child.wait())
            .await
            .is_err()
        {
            child.kill().await.ok();
        }
    }
    Ok(())
}

pub(crate) fn downloader() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()?)
}

fn authorized(request: reqwest::RequestBuilder, url: &str) -> reqwest::RequestBuilder {
    match std::env::var("HF_TOKEN") {
        Ok(token)
            if !token.is_empty()
                && reqwest::Url::parse(url)
                    .is_ok_and(|url| url.host_str() == Some("huggingface.co")) =>
        {
            request.bearer_auth(token)
        }
        _ => request,
    }
}

fn gated(response: &reqwest::Response, url: &str) -> Result<()> {
    if response
        .headers()
        .get("x-error-code")
        .is_some_and(|code| code == "GatedRepo")
    {
        let page = url.split("/resolve/").next().unwrap_or(url);
        bail!(
            "{page} asks you to accept its terms: accept them on that page, then set HF_TOKEN to an access token of that account"
        );
    }
    Ok(())
}

pub(crate) async fn download(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    partial: &Path,
    pin: (u64, &str),
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let (size, sha256) = pin;
    let mut hasher = Sha256::new();
    let mut have = 0;
    if let Ok(mut file) = File::open(partial) {
        let mut buffer = vec![0; 1 << 20];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            have += read as u64;
        }
    }
    if have > size {
        std::fs::remove_file(partial)?;
        (hasher, have) = (Sha256::new(), 0);
    }
    if have < size {
        let dir = partial.parent().unwrap_or(Path::new("."));
        let available = available(dir)?;
        let needed = u128::from(size - have + DISK_MARGIN);
        ensure!(
            needed <= available,
            "{url} needs {needed} bytes free in {} with a 1 GiB margin, and {available} are available",
            dir.display()
        );
        let mut request = authorized(http.get(url), url);
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut response = request
            .send()
            .await
            .with_context(|| format!("cannot download {url}"))?;
        gated(&response, url)?;
        response = response
            .error_for_status()
            .with_context(|| format!("cannot download {url}"))?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            (hasher, have) = (Sha256::new(), 0);
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(have > 0)
            .write(true)
            .truncate(have == 0)
            .open(partial)
            .await
            .with_context(|| format!("cannot write {}", partial.display()))?;
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("the download of {url} stopped"))?
        {
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            have += chunk.len() as u64;
            ensure!(have <= size, "{url} is longer than its pin of {size} bytes");
            progress(have, size);
        }
        file.sync_all().await?;
    }
    let got = hex(&hasher.finalize());
    if have != size || got != sha256 {
        std::fs::remove_file(partial).ok();
        bail!(
            "{url} does not match its pin and was deleted: expected {size} bytes with SHA-256 {sha256}, got {have} bytes with SHA-256 {got}"
        );
    }
    std::fs::rename(partial, target)
        .with_context(|| format!("cannot move the download to {}", target.display()))
}

fn available(dir: &Path) -> Result<u128> {
    let path = CString::new(dir.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot read the free space of {}", dir.display()));
    }
    let stat = unsafe { stat.assume_init() };
    Ok(u128::from(stat.f_bavail) * u128::from(stat.f_frsize))
}

pub(crate) struct Repository {
    pub(crate) name: String,
    pub(crate) revision: String,
    pub(crate) files: Vec<(String, u64, String)>,
}

impl Repository {
    pub(crate) fn names(&self) -> Vec<String> {
        self.files.iter().map(|(name, ..)| name.clone()).collect()
    }

    pub(crate) fn record(&self, file: &str) -> Option<Record> {
        let (name, size, sha256) = self.files.iter().find(|(name, ..)| name == file)?;
        Some(Record {
            repository: self.name.clone(),
            revision: self.revision.clone(),
            file: name.clone(),
            size: *size,
            sha256: sha256.clone(),
        })
    }

    fn from_api(name: &str, info: &Value) -> Result<Self> {
        let revision = info["sha"]
            .as_str()
            .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .context("it gives no revision")?;
        let files = info["siblings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let name = file["rfilename"].as_str()?;
                let size = file["size"].as_u64()?;
                let sha256 = file["lfs"]["sha256"]
                    .as_str()
                    .filter(|sha| is_sha256(sha))?;
                (!name.contains('/') && name.to_lowercase().ends_with(".gguf"))
                    .then(|| (name.to_owned(), size, sha256.to_owned()))
            })
            .collect();
        Ok(Self {
            name: name.into(),
            revision: revision.into(),
            files,
        })
    }
}

pub(crate) async fn repository(
    http: &reqwest::Client,
    name: &str,
    revision: Option<&str>,
) -> Result<Repository> {
    let url = format!(
        "{HUGGING_FACE}/api/models/{name}/revision/{}?blobs=true",
        utf8_percent_encode(revision.unwrap_or("main"), SEGMENT)
    );
    let response = authorized(http.get(&url), &url)
        .send()
        .await
        .with_context(|| format!("cannot reach {HUGGING_FACE}"))?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
    ) {
        bail!(
            "{HUGGING_FACE}/{name} has no such revision, or is private: check the link, or set HF_TOKEN"
        );
    }
    let info: Value = response
        .error_for_status()
        .with_context(|| format!("cannot read {url}"))?
        .json()
        .await?;
    Repository::from_api(name, &info).with_context(|| format!("cannot read {url}"))
}

pub(crate) async fn install_model(
    home: &Path,
    record: &Record,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let models = home.join("models");
    std::fs::create_dir_all(&models)
        .with_context(|| format!("cannot create {}", models.display()))?;
    let target = model_path(home, &record.sha256);
    if !target.is_file() {
        let url = format!(
            "{HUGGING_FACE}/{}/resolve/{}/{}",
            record.repository,
            record.revision,
            utf8_percent_encode(&record.file, SEGMENT)
        );
        let partial = models.join(format!("{}.partial", record.sha256));
        download(
            &downloader()?,
            &url,
            &target,
            &partial,
            (record.size, &record.sha256),
            progress,
        )
        .await?;
    }
    if let Some(old) = installed_models(home).into_iter().find(|old| {
        (&old.repository, &old.file) == (&record.repository, &record.file)
            && old.sha256 != record.sha256
    }) {
        delete_model(home, &old)?;
    }
    let path = record_path(home, record);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomically(&path, serde_json::to_string_pretty(record)?.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
}

pub(crate) fn record_path(home: &Path, record: &Record) -> PathBuf {
    home.join("models")
        .join(&record.repository)
        .join(format!("{}.json", record.file))
}

pub(crate) fn bytes(count: u64) -> String {
    match count {
        count if count >= 1_000_000_000 => format!("{:.1} GB", count as f64 / 1e9),
        count if count >= 1_000_000 => format!("{:.0} MB", count as f64 / 1e6),
        count => format!("{count} bytes"),
    }
}

pub async fn status() -> Result<String> {
    let config = Config::load()?;
    let Some(model) = config.get("model") else {
        return Ok("model      (none): run iu2u init\n".into());
    };
    let mut lines = vec![format!(
        "model      {model}, from {}",
        config.origin("model")
    )];
    if let Some(reference) = Reference::parse(&model)? {
        let home = home()?;
        let runtime = runtime_dir(&home);
        lines.push(if runtime.join("llama-server").is_file() {
            let size = std::fs::read_dir(&runtime)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len())
                .sum();
            format!("runtime    {}, {}", runtime.display(), bytes(size))
        } else {
            format!(
                "runtime    not installed; it installs itself into {} on first use",
                runtime.display()
            )
        });
        match reference.installed(&home) {
            None => lines.push("file       not installed: run iu2u init".into()),
            Some(record) => {
                let server = Server::new(&home, &record.sha256)?;
                let state = match server.asked("/props").await {
                    Some((StatusCode::OK, props)) if props["is_sleeping"] == true => {
                        "running, model asleep"
                    }
                    Some((StatusCode::OK, _)) => "running, model loaded",
                    Some(_) => "starting",
                    None => "not running",
                };
                let file = model_path(&home, &record.sha256);
                let size = std::fs::metadata(&file).map_or(0, |metadata| metadata.len());
                lines.extend([
                    format!("file       {}, {}", file.display(), bytes(size)),
                    format!("server     {state}, at {}", server.socket.display()),
                    format!("log        {}", server.log.display()),
                ]);
            }
        }
    }
    Ok(lines.join("\n") + "\n")
}

pub async fn stop_all() -> Result<usize> {
    let mut stopped = 0;
    for entry in std::fs::read_dir(home()?.join("run"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.path().extension().is_some_and(|ext| ext == "sock") && stop(&entry.path()).await {
            stopped += 1;
        }
    }
    Ok(stopped)
}

pub(crate) fn runtime_download() -> Option<(String, u64)> {
    ARCHIVE.map(|archive| (archive.url(), archive.size))
}

pub(crate) fn installed_models(home: &Path) -> Vec<Record> {
    let models = home.join("models");
    let mut records: Vec<Record> = std::fs::read_dir(&models)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|owner| {
            std::fs::read_dir(owner.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .flat_map(|repo| {
            std::fs::read_dir(repo.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .filter_map(|record| read_record(home, &record.path()))
        .collect();
    records.sort_by(|a, b| (&a.repository, &a.file).cmp(&(&b.repository, &b.file)));
    records
}

fn read_record(home: &Path, path: &Path) -> Option<Record> {
    serde_json::from_str::<Record>(&std::fs::read_to_string(path).ok()?)
        .ok()
        .filter(|record| is_sha256(&record.sha256) && model_path(home, &record.sha256).is_file())
}

pub(crate) fn unfinished_downloads(home: &Path) -> Vec<(PathBuf, u64)> {
    let mut found: Vec<(PathBuf, u64)> = std::fs::read_dir(home.join("models"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "partial"))
        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.len())))
        .collect();
    found.sort();
    found
}

pub(crate) fn delete_model(home: &Path, record: &Record) -> Result<()> {
    std::fs::remove_file(record_path(home, record)).ok();
    if installed_models(home)
        .iter()
        .all(|other| other.sha256 != record.sha256)
    {
        let path = model_path(home, &record.sha256);
        std::fs::remove_file(&path).with_context(|| format!("cannot delete {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn remove_other_runtimes(home: &Path) -> Vec<String> {
    std::fs::read_dir(home.join("runtime"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if abandoned(&name) {
                std::fs::remove_dir_all(entry.path())
                    .or_else(|_| std::fs::remove_file(entry.path()))
                    .ok();
                return None;
            }
            let old = name != BUILD
                && name.strip_prefix('b').is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
                });
            (old && std::fs::remove_dir_all(entry.path()).is_ok()).then_some(name)
        })
        .collect()
}

fn abandoned(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|rest| rest.split('.').next())
        .and_then(|pid| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
        .is_some_and(|pid| {
            let signalled = unsafe { libc::kill(pid, 0) };
            signalled == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        })
}

pub(crate) fn installation(home: &Path) -> Result<File> {
    std::fs::create_dir_all(home)?;
    locked(&home.join("install.lock"))?
        .context("another iu2u init is installing; wait for it to finish")
}

fn locked(path: &Path) -> Result<Option<File>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) async fn install_runtime(
    home: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf> {
    let installed = runtime_dir(home);
    if installed.join("llama-server").is_file() {
        return Ok(installed);
    }
    let archive = ARCHIVE.context(UNSUPPORTED)?;
    let runtime = home.join("runtime");
    std::fs::create_dir_all(&runtime)
        .with_context(|| format!("cannot create {}", runtime.display()))?;
    let pid = std::process::id();
    let (file, partial, extracted) = (
        runtime.join(format!(".{pid}.tar.gz")),
        runtime.join(format!(".{pid}.partial")),
        runtime.join(format!(".{pid}.tmp")),
    );
    let installing = async {
        download(
            &downloader()?,
            &archive.url(),
            &file,
            &partial,
            (archive.size, archive.sha256),
            progress,
        )
        .await?;
        std::fs::create_dir_all(&extracted)?;
        let tar = tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(&file)
            .arg("-C")
            .arg(&extracted)
            .output()
            .await
            .context("cannot run tar")?;
        ensure!(
            tar.status.success(),
            "tar could not extract {}: {}",
            archive.name,
            String::from_utf8_lossy(&tar.stderr).trim()
        );
        if let Err(error) = std::fs::rename(extracted.join(format!("llama-{BUILD}")), &installed)
            && !installed.join("llama-server").is_file()
        {
            return Err(error)
                .with_context(|| format!("cannot move the runtime to {}", installed.display()));
        }
        anyhow::Ok(())
    };
    let installed_now = installing.await;
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&partial).ok();
    std::fs::remove_dir_all(&extracted).ok();
    installed_now?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn names(files: &[&str]) -> Vec<String> {
        files.iter().map(|file| (*file).to_owned()).collect()
    }

    #[test]
    fn a_selector_picks_one_gguf_file_and_the_default_is_q4_k_m() {
        let files = names(&[
            "README.md",
            "m-IQ2_M.gguf",
            "m-Q4_K_M.gguf",
            "m-q8_0.gguf",
            "m-Q8_0-x.gguf",
        ]);
        assert_eq!(pick(&files, None).unwrap(), "m-Q4_K_M.gguf");
        assert_eq!(pick(&files, Some("Q8_0")).unwrap(), "m-q8_0.gguf");
        assert_eq!(pick(&files, Some("m-iq2_m.GGUF")).unwrap(), "m-IQ2_M.gguf");
        assert_eq!(pick(&names(&["b.gguf", "a.gguf"]), None).unwrap(), "b.gguf");
        let precise = names(&["m-bf16.gguf", "m-f16.gguf", "m-IQ2_M.gguf"]);
        assert_eq!(pick(&precise, Some("f16")).unwrap(), "m-f16.gguf");
        assert!(pick(&precise, Some("Q2_M")).is_err());
        let error = pick(&files, Some("Q5")).unwrap_err().to_string();
        assert!(
            error.contains("no file matches Q5")
                && error.contains("m-IQ2_M.gguf, m-Q4_K_M.gguf, m-q8_0.gguf, m-Q8_0-x.gguf"),
            "{error}"
        );
        assert!(pick(&names(&["a-Q4_K_M.gguf", "b-Q4_K_M.gguf"]), None).is_err());
        assert!(pick(&names(&["README.md"]), None).is_err());
    }

    #[cfg(local_model)]
    #[test]
    fn the_identity_keeps_the_socket_short_and_differs_per_model() {
        let home = Path::new("/home/someone/.local/share/iu2u");
        let (one, two) = (
            Server::new(home, &"a".repeat(64)).unwrap(),
            Server::new(home, &"b".repeat(64)).unwrap(),
        );
        let name = one
            .socket
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name.len(), "0123456789abcdef.sock".len());
        assert_ne!(one.socket, two.socket);
        assert!(Server::new(home, "../x").is_err());
        let long = PathBuf::from(format!("/{}", "x".repeat(120)));
        let error = Server::new(&long, &"a".repeat(64)).unwrap_err().to_string();
        assert!(error.contains("XDG_DATA_HOME"), "{error}");
    }

    #[test]
    fn a_hugging_face_answer_gives_the_revision_and_the_gguf_files() {
        let sha256 = "a".repeat(64);
        let info = json!({
            "sha": "88e7a44e35b3bf38ee716042d61deaa923c92359",
            "siblings": [
                { "rfilename": "README.md", "size": 9 },
                { "rfilename": "m-Q4_K_M.gguf", "size": 7, "lfs": { "sha256": sha256 } },
                { "rfilename": "Q8_0/m-Q8_0-00001-of-00002.gguf", "size": 7, "lfs": { "sha256": sha256 } },
                { "rfilename": "m-small.gguf", "size": 7 }
            ]
        });
        let repository = Repository::from_api("o/r", &info).unwrap();
        assert_eq!(
            repository.revision,
            "88e7a44e35b3bf38ee716042d61deaa923c92359"
        );
        assert_eq!(repository.files, [("m-Q4_K_M.gguf".to_owned(), 7, sha256)]);
        assert!(Repository::from_api("o/r", &json!({ "sha": "main" })).is_err());
    }

    #[tokio::test]
    async fn downloads_refuse_plain_http() {
        let error = downloader()
            .unwrap()
            .get("http://127.0.0.1:9/archive.tar.gz")
            .send()
            .await
            .unwrap_err();
        assert!(error.is_builder(), "{error}");
    }

    async fn served(body: &'static [u8]) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("range", "bytes=4-"))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(&body[4..]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        server
    }

    fn record(repository: &str, file: &str) -> Record {
        Record {
            repository: repository.into(),
            revision: "a".repeat(40),
            file: file.into(),
            size: 7,
            sha256: "a".repeat(64),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("iu2u-unit")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const BODY: &[u8] = b"model weights";

    #[tokio::test]
    async fn a_download_resumes_from_its_partial_file_and_lands_when_its_hash_matches() {
        let server = served(BODY).await;
        let dir = scratch("resume");
        let (target, partial) = (dir.join("model"), dir.join("model.partial"));
        std::fs::write(&partial, &BODY[..4]).unwrap();
        let pin = (BODY.len() as u64, hex(&Sha256::digest(BODY)));
        let mut seen = Vec::new();
        download(
            &reqwest::Client::new(),
            &server.uri(),
            &target,
            &partial,
            (pin.0, &pin.1),
            &mut |have, total| seen.push((have, total)),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), BODY);
        assert!(!partial.exists());
        assert_eq!(seen.last(), Some(&(pin.0, pin.0)));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].headers["range"], "bytes=4-");
    }

    #[tokio::test]
    async fn a_download_with_one_changed_byte_is_refused_with_both_hashes() {
        let server = served(b"model weighTs").await;
        let dir = scratch("mismatch");
        let (target, partial) = (dir.join("model"), dir.join("model.partial"));
        let expected = hex(&Sha256::digest(BODY));
        let error = download(
            &reqwest::Client::new(),
            &server.uri(),
            &target,
            &partial,
            (BODY.len() as u64, &expected),
            &mut |_, _| {},
        )
        .await
        .unwrap_err()
        .to_string();
        let got = hex(&Sha256::digest(b"model weighTs"));
        assert!(error.contains(&expected) && error.contains(&got), "{error}");
        assert!(!target.exists() && !partial.exists());
    }

    #[tokio::test]
    async fn a_gated_repository_names_its_page_and_hf_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).insert_header("x-error-code", "GatedRepo"))
            .mount(&server)
            .await;
        let dir = scratch("gated");
        let url = format!("{}/o/r/resolve/main/m.gguf", server.uri());
        let error = download(
            &reqwest::Client::new(),
            &url,
            &dir.join("model"),
            &dir.join("model.partial"),
            (1, &"0".repeat(64)),
            &mut |_, _| {},
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(&format!("{}/o/r asks you to accept", server.uri()))
                && error.contains("HF_TOKEN"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_new_revision_of_an_installed_file_replaces_the_old_file() {
        let home = scratch("revision");
        let old = record("o/r", "m.gguf");
        let new = Record {
            revision: "b".repeat(40),
            sha256: "b".repeat(64),
            ..old.clone()
        };
        std::fs::create_dir_all(home.join("models")).unwrap();
        for record in [&old, &new] {
            std::fs::write(model_path(&home, &record.sha256), "weights").unwrap();
            install_model(&home, record, &mut |_, _| {}).await.unwrap();
        }
        assert_eq!(installed_models(&home), std::slice::from_ref(&new));
        assert!(!model_path(&home, &old.sha256).exists());
    }

    #[test]
    fn old_runtimes_and_abandoned_installations_go_and_running_ones_stay() {
        let home = scratch("runtimes");
        let runtime = home.join("runtime");
        let running = format!(".{}.tmp", std::process::id());
        for dir in ["b4000", BUILD, &running] {
            std::fs::create_dir_all(runtime.join(dir)).unwrap();
        }
        std::fs::write(runtime.join(".999999999.partial"), "part").unwrap();
        assert_eq!(remove_other_runtimes(&home), ["b4000"]);
        assert!(!runtime.join(".999999999.partial").exists());
        assert!(runtime.join(BUILD).exists() && runtime.join(running).exists());
    }

    #[tokio::test]
    async fn a_reference_without_a_file_is_installed_only_with_its_q4_k_m_file() {
        let home = scratch("unnamed");
        let other = record("o/r", "m-IQ2_M.gguf");
        std::fs::create_dir_all(home.join("models")).unwrap();
        std::fs::write(model_path(&home, &other.sha256), "weights").unwrap();
        install_model(&home, &other, &mut |_, _| {}).await.unwrap();
        let installed = |model| Reference::parse(model).unwrap().unwrap().installed(&home);
        assert_eq!(installed("decision:o/r"), None);
        assert_eq!(installed("decision:o/r:IQ2_M"), Some(other));
    }

    #[tokio::test]
    async fn a_model_file_shared_by_two_records_goes_with_the_last_one() {
        let home = scratch("shared");
        let first = record("o/one", "m.gguf");
        let second = Record {
            repository: "o/two".into(),
            ..first.clone()
        };
        std::fs::create_dir_all(home.join("models")).unwrap();
        std::fs::write(model_path(&home, &first.sha256), "weights").unwrap();
        for record in [&first, &second] {
            install_model(&home, record, &mut |_, _| {}).await.unwrap();
        }
        delete_model(&home, &first).unwrap();
        assert!(model_path(&home, &first.sha256).exists());
        delete_model(&home, &second).unwrap();
        assert!(!model_path(&home, &first.sha256).exists());
    }

    #[cfg(local_model)]
    #[tokio::test]
    #[ignore = "downloads the pinned llama.cpp runtime"]
    async fn the_pinned_runtime_installs_and_runs() {
        let home = scratch("runtime");
        let mut reported = (0, 0);
        let runtime = install_runtime(&home, &mut |have, total| reported = (have, total))
            .await
            .unwrap();
        assert!(reported.0 > 0 && reported.0 == reported.1, "{reported:?}");
        let version = std::process::Command::new(runtime.join("llama-server"))
            .arg("--version")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&version.stderr).into_owned()
            + &String::from_utf8_lossy(&version.stdout);
        assert!(
            version.status.success() && printed.contains("12359"),
            "{printed}"
        );
        std::fs::remove_dir_all(&home).ok();
    }
}
