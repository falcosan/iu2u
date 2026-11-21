use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use super::script::Start;
use crate::tail::{KEPT_BYTES, from_a_line, read_tail, tail};

const READY_EVERY: Duration = Duration::from_millis(250);
const GONE_EVERY: Duration = Duration::from_millis(25);
const READ_CHUNK: usize = 1 << 16;
const STOP_WITHIN: Duration = Duration::from_secs(5);
const READY_REFUSALS: RangeInclusive<u16> = 400..=403;

pub(super) struct Service {
    pub(super) name: String,
    child: Child,
    log: PathBuf,
    group: Group,
    shown_from: u64,
}

pub(super) struct Ran {
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) cut: bool,
}

impl Ran {
    pub(super) fn exit(&self) -> String {
        self.code
            .map_or_else(|| "exit on a signal".into(), |code| format!("exit {code}"))
    }

    pub(super) fn shown(self) -> Vec<String> {
        let exit = self.exit();
        let mut lines: Vec<String> = self.stdout.lines().map(str::to_owned).collect();
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

pub(super) async fn run(
    command: &str,
    env: &[(String, String)],
    within: Duration,
) -> Result<Option<Ran>> {
    let mut child = shell(command, env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start sh")?;
    let group = Group(child.id());
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let Ok(status) = tokio::time::timeout(within, child.wait()).await else {
        return Ok(None);
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
    Ok(Some(Ran {
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        cut,
    }))
}

fn read_all(
    pipe: Option<impl AsyncRead + Unpin + Send + 'static>,
) -> tokio::task::JoinHandle<(Vec<u8>, bool)> {
    tokio::spawn(async move {
        let (mut bytes, mut chunk, mut cut) = (Vec::new(), vec![0; READ_CHUNK], false);
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

pub(super) async fn start(
    client: &reqwest::Client,
    start: &Start,
    env: &[(String, String)],
    dir: &Path,
    within: Duration,
) -> Result<Service> {
    if let Some(at) = &start.at {
        let url = reqwest::Url::parse(at)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .with_context(|| format!("{at} is not an http or https address"))?;
        let address = format!(
            "{}:{}",
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or_default()
        );
        if tokio::time::timeout(READY_EVERY, tokio::net::TcpStream::connect(address))
            .await
            .is_ok_and(|connected| connected.is_ok())
        {
            bail!(
                "something already listens at {at}; stop it before {} starts",
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
        let (mut answered, mut failed) = (None, None);
        loop {
            let asked = client
                .get(at)
                .timeout(within.saturating_sub(started.elapsed()).max(READY_EVERY))
                .send();
            let answer = tokio::select! {
                biased;
                answer = asked => Some(answer),
                _ = child.wait() => None,
            };
            match answer {
                Some(Ok(response)) => {
                    let status = response.status();
                    if status.is_success()
                        || status.is_redirection()
                        || READY_REFUSALS.contains(&status.as_u16())
                    {
                        break;
                    }
                    answered = Some(status);
                }
                Some(Err(error)) => failed = Some(anyhow::Error::new(error)),
                None => {}
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
                    None => match answered {
                        Some(status) => bail!(
                            "{at} answered only {status} within {}s{said}",
                            within.as_secs_f64()
                        ),
                        None => bail!(
                            "{at} did not answer within {}s{}{said}",
                            within.as_secs_f64(),
                            failed
                                .map(|error| format!(": {error:#}"))
                                .unwrap_or_default()
                        ),
                    },
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

pub(super) async fn stop(mut service: Service) {
    let Some(group) = service.group.0 else {
        return;
    };
    signal(group, libc::SIGTERM);
    end(&mut service.group, &mut service.child, STOP_WITHIN).await;
}

pub(super) async fn end(group: &mut Group, child: &mut Child, within: Duration) {
    let Some(id) = group.0 else {
        return;
    };
    let started = Instant::now();
    while started.elapsed() < within && running(id) {
        child.try_wait().ok();
        tokio::time::sleep(GONE_EVERY).await;
    }
    signal(id, libc::SIGKILL);
    group.0 = None;
    tokio::time::timeout(within, child.wait()).await.ok();
}

pub(super) async fn gone(groups: &[u32], within: Duration) {
    let started = Instant::now();
    while started.elapsed() < within && groups.iter().any(|group| running(*group)) {
        tokio::time::sleep(GONE_EVERY).await;
    }
}

fn running(group: u32) -> bool {
    libc::pid_t::try_from(group).is_ok_and(|group| unsafe { libc::kill(-group, 0) } == 0)
}

pub(super) struct Group(pub(super) Option<u32>);

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
