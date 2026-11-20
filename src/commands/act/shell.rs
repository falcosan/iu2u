use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use super::script::Start;

const READY_EVERY: Duration = Duration::from_millis(250);
const STOP_WITHIN: Duration = Duration::from_secs(5);
const TAIL_LINES: usize = 20;
pub(super) const KEPT_BYTES: usize = 1024 * 1024;

pub(super) struct Service {
    pub(super) name: String,
    child: Child,
    log: PathBuf,
    group: Group,
    shown_from: u64,
}

pub(super) struct Ran {
    pub(super) code: Option<i32>,
    pub(super) lines: Vec<String>,
    pub(super) stdout: String,
    pub(super) cut: bool,
}

impl Ran {
    pub(super) fn exit(&self) -> String {
        self.code
            .map_or_else(|| "exit on a signal".into(), |code| format!("exit {code}"))
    }

    pub(super) fn shown(self) -> Vec<String> {
        let exit = self.exit();
        let mut lines = self.lines;
        if self.cut {
            lines.insert(
                0,
                format!("(the command printed more; only its last {KEPT_BYTES} bytes are kept)"),
            );
        }
        lines.push(exit);
        lines
    }
}

fn shell(command: &str, env: &[(String, String)]) -> Command {
    let mut shell = Command::new("sh");
    shell
        .arg("-c")
        .arg(command)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    shell
}

pub(super) async fn run(command: &str, env: &[(String, String)], within: Duration) -> Result<Ran> {
    let mut child = shell(command, env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start sh")?;
    let group = Group(child.id());
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let Ok(status) = tokio::time::timeout(within, child.wait()).await else {
        bail!(
            "the command did not finish within {}s",
            within.as_secs_f64()
        );
    };
    let status = status.context("the command broke off")?;
    drop(group);
    let Ok((Ok((stdout, cut)), Ok((stderr, _)))) =
        tokio::time::timeout(STOP_WITHIN, async { (stdout.await, stderr.await) }).await
    else {
        bail!(
            "the command ended, but a process it started outside its group still holds its output"
        );
    };
    let lines = [&stdout, &stderr]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(Ran {
        code: status.code(),
        lines,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        cut,
    })
}

fn read_all(
    pipe: Option<impl AsyncRead + Unpin + Send + 'static>,
) -> tokio::task::JoinHandle<(Vec<u8>, bool)> {
    tokio::spawn(async move {
        let (mut bytes, mut chunk, mut cut) = (Vec::new(), vec![0; 64 * 1024], false);
        if let Some(mut pipe) = pipe {
            while let Ok(read @ 1..) = pipe.read(&mut chunk).await {
                bytes.extend_from_slice(&chunk[..read]);
                if bytes.len() > 2 * KEPT_BYTES {
                    bytes.drain(..bytes.len() - KEPT_BYTES);
                    cut = true;
                }
            }
        }
        if bytes.len() > KEPT_BYTES {
            bytes.drain(..bytes.len() - KEPT_BYTES);
            cut = true;
        }
        if cut {
            bytes = from_a_line(bytes);
        }
        (bytes, cut)
    })
}

fn from_a_line(mut bytes: Vec<u8>) -> Vec<u8> {
    let start = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(0, |at| at + 1);
    bytes.drain(..start);
    bytes
}

pub(super) async fn start(
    client: &reqwest::Client,
    start: &Start,
    env: &[(String, String)],
    dir: &Path,
    within: Duration,
) -> Result<Service> {
    let answers = async |at: &str| client.get(at).timeout(READY_EVERY * 4).send().await;
    if let Some(at) = &start.at {
        anyhow::ensure!(
            reqwest::Url::parse(at).is_ok_and(|url| matches!(url.scheme(), "http" | "https")),
            "{at} is not an http or https address"
        );
        if answers(at).await.is_ok() {
            bail!(
                "something already answers at {at}; stop it before {} starts",
                start.name
            );
        }
    }
    let log = dir.join(format!("{}.log", start.name));
    let file =
        std::fs::File::create(&log).with_context(|| format!("cannot create {}", log.display()))?;
    let mut child = shell(&start.command, env)
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()
        .context("cannot start sh")?;
    let group = Group(child.id());
    if let Some(at) = &start.at {
        let started = Instant::now();
        loop {
            if let Ok(response) = answers(at).await
                && (response.status().is_success()
                    || response.status().is_redirection()
                    || (400..=403).contains(&response.status().as_u16()))
            {
                break;
            }
            let ended = child.try_wait()?;
            if ended.is_some() || started.elapsed() >= within {
                let tail = tail(&log);
                let said = if tail.is_empty() {
                    String::new()
                } else {
                    format!("; {} said:\n{tail}", start.name)
                };
                match ended {
                    Some(status) => bail!(
                        "{} stopped ({status}) before {at} answered{said}",
                        start.name
                    ),
                    None => bail!("{at} did not answer within {}s{said}", within.as_secs_f64()),
                }
            }
            tokio::time::sleep(READY_EVERY).await;
        }
    }
    Ok(Service {
        name: start.name.clone(),
        child,
        log,
        group,
        shown_from: 0,
    })
}

impl Service {
    pub(super) fn begin_script(&mut self) {
        self.shown_from = std::fs::metadata(&self.log).map_or(0, |log| log.len());
    }
}

pub(super) fn logs(service: &Service) -> Vec<String> {
    let (log, cut) = read_tail(&service.log, service.shown_from);
    let note =
        cut.then(|| format!("(the log began earlier; only its last {KEPT_BYTES} bytes are kept)"));
    note.into_iter()
        .chain(String::from_utf8_lossy(&log).lines().map(str::to_owned))
        .collect()
}

fn read_tail(path: &Path, shown_from: u64) -> (Vec<u8>, bool) {
    let (mut bytes, mut cut) = (Vec::new(), false);
    if let Ok(mut file) = std::fs::File::open(path)
        && let Ok(length) = file.metadata().map(|meta| meta.len())
    {
        let kept = u64::try_from(KEPT_BYTES).unwrap_or(u64::MAX);
        let from = shown_from.max(length.saturating_sub(kept));
        cut = from > shown_from;
        if file.seek(SeekFrom::Start(from)).is_ok() {
            file.take(kept).read_to_end(&mut bytes).ok();
        }
    }
    if cut {
        bytes = from_a_line(bytes);
    }
    (bytes, cut)
}

pub(super) async fn stop(mut service: Service) {
    let Some(group) = service.group.0 else {
        return;
    };
    signal(group, libc::SIGTERM);
    let started = Instant::now();
    while started.elapsed() < STOP_WITHIN && running(group) {
        service.child.try_wait().ok();
        tokio::time::sleep(READY_EVERY).await;
    }
    signal(group, libc::SIGKILL);
    service.group.0 = None;
    service.child.wait().await.ok();
}

fn running(group: u32) -> bool {
    libc::pid_t::try_from(group).is_ok_and(|group| unsafe { libc::kill(-group, 0) } == 0)
}

struct Group(Option<u32>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            signal(group, libc::SIGKILL);
        }
    }
}

fn signal(group: u32, signal: libc::c_int) {
    if let Ok(group) = libc::pid_t::try_from(group) {
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

pub(crate) fn tail(log: &Path) -> String {
    let (bytes, _) = read_tail(log, 0);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}
