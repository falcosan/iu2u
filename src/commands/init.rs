use std::io::BufRead;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;

use crate::config::{self, Config};
use crate::hooks::IRREVERSIBLE;
use crate::model::decision::{self, Record, bytes};
use crate::model::{Client, DEFAULT_CLAUDE_MODEL, Question, Questions, Reference, is_claude};
use crate::write_atomically;

const SWITCHES: [&str; 5] = ["guard", "proof", "trim", "compass", "prune"];
const LOCAL_BY_DEFAULT: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));
const ANSWER_WITHIN: Duration = Duration::from_secs(120);
const PROGRESS_EVERY: Duration = Duration::from_secs(5);
const LOADED_OVERHEAD: u64 = 1_500_000_000;
const FASTINO_TERMS: &str = "The Fastino Community License binds you: commercial use must display \"Built with Fastino GLiDE\", and an organisation with more than 10 million US dollars of yearly revenue needs an agreement with Fastino.";
const CPU_ONLY: &str = "On Linux the bundled runtime uses the CPU only: one yes-or-no question took 11.7 s on 6 CPUs with a smaller file than the default, so hooks will time out.";
const TEST_STATE: &str = "rm -rf ~/Documents";

pub async fn init(input: &mut dyn BufRead, yes: bool) -> Result<()> {
    let path = config::location()?;
    let config = Config::load()?;
    let (settings, local) = if yes {
        unattended(&config).await?
    } else {
        asked(input, &config).await?
    };
    let home = match &local {
        Some(_) => Some(decision::home()?),
        None => decision::home().ok().filter(|home| home.is_dir()),
    };
    eprintln!("\niu2u init will:");
    let written: Vec<String> = settings
        .iter()
        .map(|(name, value)| format!("{name} = {value}"))
        .collect();
    eprintln!("  write {}: {}", path.display(), written.join(", "));
    if let (Some(home), Some(record)) = (&home, &local) {
        summarize(home, record);
    }
    if !yes && !confirmed(input, "Go ahead?")? {
        eprintln!("Nothing was written or downloaded.");
        return Ok(());
    }
    let lock = home.as_deref().map(decision::installation).transpose()?;
    if let (Some(home), Some(record)) = (&home, &local) {
        install(home, record).await?;
    }
    let previous = match std::fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        read => Some(read.with_context(|| format!("cannot read {}", path.display()))?),
    };
    config::write(&settings)?;
    eprintln!("Wrote {}.", path.display());
    let mut overridden = false;
    for (name, value) in &settings {
        if let Some(set) = config::variable(name).filter(|set| set != value) {
            eprintln!(
                "warning: {}={set} in this shell overrides the {name} just written",
                config::variable_name(name)
            );
            overridden |= !SWITCHES.contains(name);
        }
    }
    let tested = if overridden {
        eprintln!(
            "Skipped the test question: it would ask the model the variable names, not the one just written. Unset the variable and run iu2u init again to test it."
        );
        Ok(())
    } else {
        if local.is_some() {
            decision::stop_all().await?;
        }
        test(input, yes, home.as_deref().zip(local.as_ref())).await
    };
    if tested
        .as_ref()
        .is_err_and(|error| answered_with(error, &[StatusCode::NOT_IMPLEMENTED]))
    {
        match &previous {
            Some(text) => write_atomically(&path, text),
            None => std::fs::remove_file(&path),
        }
        .with_context(|| format!("cannot put back {}", path.display()))?;
        eprintln!("Put back {} as it was.", path.display());
    }
    tested?;
    if let Some(home) = &home {
        tidy(input, yes, home, local.as_ref())?;
    }
    drop(lock);
    eprintln!(
        "\nNext, add the plugin to Claude Code:\n  claude plugin marketplace add falcosan/iu2u\n  claude plugin install iu2u@iu2u"
    );
    if local.is_none() && !is_claude(&settings[0].1) {
        eprintln!(
            "If your server needs an API key, set it in Claude Code with /plugin configure iu2u@iu2u, or export IU2U_API_KEY."
        );
    }
    Ok(())
}

type Plan = (Vec<(&'static str, String)>, Option<Record>);

async fn unattended(config: &Config) -> Result<Plan> {
    let model = match config.saved("model") {
        Some(model) => model,
        None if LOCAL_BY_DEFAULT => "decision".into(),
        None => bail!(
            "no model is configured: choose one with iu2u config set model <model>, then run iu2u init --yes again"
        ),
    };
    Ok(match Reference::parse(&model)? {
        Some(reference) => {
            let record = resolved(None, &reference).await?;
            (
                vec![("model", reference.stored(Some(&record.file)))],
                Some(record),
            )
        }
        None => {
            let setting = if is_claude(&model) {
                "effort"
            } else {
                "base_url"
            };
            let value = config.saved(setting).unwrap_or_default();
            (vec![("model", model), (setting, value)], None)
        }
    })
}

async fn asked(input: &mut dyn BufRead, config: &Config) -> Result<Plan> {
    let current = config.saved("model");
    let local = current
        .as_deref()
        .and_then(|model| Reference::parse(model).ok().flatten());
    let default = match (&current, &local) {
        (_, Some(Reference::BuiltIn)) => 1,
        (_, Some(_)) => 2,
        (Some(model), None) if is_claude(model) => 3,
        (Some(_), None) => 4,
        (None, None) if LOCAL_BY_DEFAULT => 1,
        (None, None) => 4,
    };
    let choice = loop {
        let choice = choose(
            input,
            "Which decision model should iu2u use?",
            &[
                "GLiDE-9B, run on this machine by iu2u (downloads 5.8 GB)".into(),
                "Another local model, from a Hugging Face link".into(),
                "Claude, through Claude Code (each request counts against your Claude plan)".into(),
                "A System One server you run or rent".into(),
            ],
            default,
        )?;
        match (choice, decision::supported()) {
            (1 | 2, Err(error)) => eprintln!("{error}"),
            _ => break choice,
        }
    };
    let mut settings = Vec::new();
    let mut record = None;
    match choice {
        1 | 2 => {
            let reference = if choice == 1 {
                Reference::BuiltIn
            } else {
                loop {
                    let link = answer(
                        input,
                        "Hugging Face link, or decision:<owner>/<repo>[:<file>]",
                        current.as_deref().filter(|_| default == 2),
                    )?;
                    match Reference::parse(&link) {
                        Ok(Some(reference)) => break reference,
                        Ok(None) => eprintln!("{link} is not a Hugging Face model link."),
                        Err(error) => eprintln!("{error}"),
                    }
                }
            };
            let found = resolved(Some(&mut *input), &reference).await?;
            settings.push(("model", reference.stored(Some(&found.file))));
            record = Some(found);
        }
        3 => {
            let model = current
                .as_deref()
                .and_then(|model| model.strip_prefix("claude:"))
                .unwrap_or(DEFAULT_CLAUDE_MODEL);
            let model = answer(input, "Claude model", Some(model))?;
            let effort = setting(
                input,
                "Effort: low, medium, high, xhigh or max",
                "effort",
                config.saved("effort").as_deref(),
            )?;
            settings.extend([("model", claude_model(&model)), ("effort", effort)]);
        }
        _ => {
            let url = setting(
                input,
                "Server URL",
                "base_url",
                config.saved("base_url").as_deref(),
            )?;
            let model = answer(
                input,
                "Model name on that server",
                current.as_deref().filter(|_| default == 4),
            )?;
            settings.extend([("model", model), ("base_url", url)]);
        }
    }
    let on: Vec<String> = SWITCHES
        .iter()
        .enumerate()
        .filter(|(_, name)| config.saved(name).as_deref() == Some("true"))
        .map(|(index, _)| (index + 1).to_string())
        .collect();
    let listed: Vec<String> = SWITCHES
        .iter()
        .enumerate()
        .map(|(index, name)| format!("{} {name}", index + 1))
        .collect();
    let on = on.join(" ");
    let picked = numbers(
        input,
        &format!(
            "Hooks to turn on, by number separated by spaces, or none ({})",
            listed.join(", ")
        ),
        if on.is_empty() { "none" } else { &on },
        SWITCHES.len(),
    )?;
    settings.extend(
        SWITCHES
            .iter()
            .enumerate()
            .map(|(index, name)| (*name, picked.contains(&(index + 1)).to_string())),
    );
    Ok((settings, record))
}

async fn resolved(input: Option<&mut dyn BufRead>, reference: &Reference) -> Result<Record> {
    decision::supported()?;
    let Reference::Linked {
        repository,
        revision,
        selector,
    } = reference
    else {
        return Ok(decision::built_in());
    };
    let home = decision::home()?;
    if let Some(record) = reference.installed(&home)
        && revision
            .as_ref()
            .is_none_or(|revision| *revision == record.revision)
    {
        return Ok(record);
    }
    let found =
        decision::repository(&decision::downloader()?, repository, revision.as_deref()).await?;
    let names = found.names();
    let file = match input {
        None => decision::pick(&names, selector.as_deref())?.to_owned(),
        Some(input) => {
            let default = decision::pick(&names, selector.as_deref())
                .ok()
                .and_then(|file| names.iter().position(|name| name == file))
                .map_or(1, |index| index + 1);
            let listed: Vec<String> = found
                .files
                .iter()
                .map(|(name, size, _)| format!("{name}, {}", bytes(*size)))
                .collect();
            if listed.is_empty() {
                bail!("{repository} has no .gguf file with a SHA-256");
            }
            names[choose(input, "Which file?", &listed, default)? - 1].clone()
        }
    };
    found
        .record(&file)
        .with_context(|| format!("{repository} has no file {file}"))
}

fn summarize(home: &Path, record: &Record) {
    if !decision::runtime_dir(home).join("llama-server").is_file()
        && let Some((url, size)) = decision::runtime_download()
    {
        eprintln!(
            "  download the llama.cpp runtime {}, {}, from {url}",
            decision::BUILD,
            bytes(size)
        );
    }
    let at = format!(
        "https://huggingface.co/{} at revision {}",
        record.repository, record.revision
    );
    if decision::model_path(home, &record.sha256).is_file() {
        eprintln!("  use {}, already installed, from {at}", record.file);
    } else {
        eprintln!(
            "  download {}, {}, from {at}, into {}",
            record.file,
            bytes(record.size),
            home.join("models").display()
        );
    }
    let memory = memory().map_or_else(|| "an unknown amount".into(), bytes);
    eprintln!(
        "  memory: about {} while the model is loaded; this machine has {memory}",
        bytes(record.size + LOADED_OVERHEAD)
    );
    eprintln!("  license: {}", record.license);
    if record.license.to_lowercase().contains("fastino") {
        eprintln!("  {FASTINO_TERMS}");
    }
    if cfg!(target_os = "linux") {
        eprintln!("  {CPU_ONLY}");
    }
}

async fn install(home: &Path, record: &Record) -> Result<()> {
    eprintln!("Installing the llama.cpp runtime {}…", decision::BUILD);
    decision::install_runtime(home, &mut progress()).await?;
    eprintln!("Installing {}…", record.file);
    decision::install_model(home, record, &mut progress()).await
}

fn progress() -> impl FnMut(u64, u64) {
    let (started, mut shown, mut first) = (Instant::now(), Instant::now(), None);
    move |have, total| {
        let first = *first.get_or_insert(have);
        if shown.elapsed() >= PROGRESS_EVERY || have == total {
            shown = Instant::now();
            let rate = (have - first) as f64 / started.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "  {} of {} ({}%), {}/s",
                bytes(have),
                bytes(total),
                have * 100 / total.max(1),
                bytes(rate as u64)
            );
        }
    }
}

async fn test(input: &mut dyn BufRead, yes: bool, local: Option<(&Path, &Record)>) -> Result<()> {
    let client = Client::from_env()?.with_timeout(ANSWER_WITHIN);
    let questions = Questions::from([("test".into(), Question::yesno(IRREVERSIBLE))]);
    let mut started = Instant::now();
    let mut answered = client.ask(TEST_STATE, &questions).await;
    if local.is_some() && answered.is_ok() {
        eprintln!(
            "The model started in {:.1} s.",
            started.elapsed().as_secs_f64()
        );
        started = Instant::now();
        answered = client.ask(TEST_STATE, &questions).await;
    }
    let took = started.elapsed();
    let answers = match (answered, local) {
        (Ok(answers), _) => answers,
        (Err(error), Some((home, record)))
            if answered_with(&error, &[StatusCode::NOT_IMPLEMENTED]) =>
        {
            decision::stop_all().await?;
            eprintln!(
                "{} is not served as a decision model: the runtime answered 501 to the test question.",
                record.file
            );
            if !yes && confirmed(input, &format!("Delete {}?", record.file))? {
                decision::delete_model(home, record)?;
                eprintln!("Deleted.");
            }
            return Err(error.context(format!("{} is not a decision model", record.file)));
        }
        (Err(error), _)
            if answered_with(&error, &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN])
                && config::api_key().is_none() =>
        {
            eprintln!(
                "Skipped the test question: the server asks for an API key, and this shell has none. Set it in Claude Code with /plugin configure iu2u@iu2u, or run IU2U_API_KEY=<key> iu2u init to test it here."
            );
            return Ok(());
        }
        (Err(error), _) => {
            return Err(error.context("the model could not answer the test question"));
        }
    };
    let yes_probability = answers
        .get("test")
        .and_then(|answer| answer.yes())
        .unwrap_or_default();
    println!(
        "The model answered in {:.1} s: `{TEST_STATE}` is hard to recover with probability {yes_probability:.2}.",
        took.as_secs_f64()
    );
    let timeout_ms: u128 = Config::load()?
        .get("timeout_ms")
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_default();
    if took.as_millis() > timeout_ms {
        eprintln!(
            "warning: that is longer than timeout_ms ({timeout_ms} ms), so hooks will time out"
        );
    }
    Ok(())
}

fn answered_with(error: &anyhow::Error, statuses: &[StatusCode]) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            .is_some_and(|status| statuses.contains(&status))
    })
}

fn tidy(input: &mut dyn BufRead, yes: bool, home: &Path, record: Option<&Record>) -> Result<()> {
    for removed in decision::remove_other_runtimes(home) {
        eprintln!("Removed the old runtime {removed}.");
    }
    let others: Vec<Record> = decision::installed_models(home)
        .into_iter()
        .filter(|other| Some(&other.sha256) != record.map(|record| &record.sha256))
        .collect();
    let unfinished = decision::unfinished_downloads(home);
    if others.is_empty() && unfinished.is_empty() {
        return Ok(());
    }
    eprintln!("Other installed models and unfinished downloads:");
    for (index, other) in others.iter().enumerate() {
        eprintln!(
            "  {}) {} {}, {}",
            index + 1,
            other.repository,
            other.file,
            bytes(other.size)
        );
    }
    for (index, (_, size)) in unfinished.iter().enumerate() {
        eprintln!(
            "  {}) an unfinished download, {}",
            others.len() + index + 1,
            bytes(*size)
        );
    }
    if yes {
        return Ok(());
    }
    let picked = numbers(
        input,
        "Delete any of them? By number separated by spaces, or none",
        "none",
        others.len() + unfinished.len(),
    )?;
    for index in picked.into_iter().map(|number| number - 1) {
        match others.get(index) {
            Some(other) => {
                decision::delete_model(home, other)?;
                eprintln!("Deleted {}.", other.file);
            }
            None => {
                let (path, _) = &unfinished[index - others.len()];
                std::fs::remove_file(path)
                    .with_context(|| format!("cannot delete {}", path.display()))?;
                eprintln!("Deleted the unfinished download.");
            }
        }
    }
    Ok(())
}

fn claude_model(answer: &str) -> String {
    match answer.strip_prefix("claude:").unwrap_or(answer) {
        "" | "claude" | DEFAULT_CLAUDE_MODEL => "claude".into(),
        model => format!("claude:{model}"),
    }
}

fn numbers(
    input: &mut dyn BufRead,
    question: &str,
    default: &str,
    count: usize,
) -> Result<Vec<usize>> {
    loop {
        let text = answer(input, question, Some(default))?;
        let numbers: Option<Vec<usize>> = text
            .split_whitespace()
            .filter(|word| *word != "none")
            .map(|word| word.parse().ok().filter(|n| (1..=count).contains(n)))
            .collect();
        match numbers {
            Some(mut numbers) => {
                numbers.sort_unstable();
                numbers.dedup();
                return Ok(numbers);
            }
            None => eprintln!("Answer with numbers from 1 to {count}, or none."),
        }
    }
}

fn setting(
    input: &mut dyn BufRead,
    question: &str,
    name: &str,
    default: Option<&str>,
) -> Result<String> {
    loop {
        let text = answer(input, question, default)?;
        match config::checked(name, &text) {
            Ok(()) => return Ok(text),
            Err(error) => eprintln!("{error}"),
        }
    }
}

fn line(input: &mut dyn BufRead) -> Result<String> {
    let mut text = String::new();
    if input.read_line(&mut text)? == 0 {
        bail!("no answer: the input ended; run iu2u init --yes to answer nothing");
    }
    Ok(text.trim().to_owned())
}

fn answer(input: &mut dyn BufRead, question: &str, default: Option<&str>) -> Result<String> {
    loop {
        match default {
            Some(default) => eprint!("{question} [{default}]: "),
            None => eprint!("{question}: "),
        }
        let text = line(input)?;
        match (text.is_empty(), default) {
            (false, _) => return Ok(text),
            (true, Some(default)) => return Ok(default.to_owned()),
            (true, None) => eprintln!("This needs an answer."),
        }
    }
}

fn choose(
    input: &mut dyn BufRead,
    question: &str,
    options: &[String],
    default: usize,
) -> Result<usize> {
    eprintln!("{question}");
    for (index, option) in options.iter().enumerate() {
        eprintln!("  {}) {option}", index + 1);
    }
    loop {
        let text = answer(input, "Number", Some(&default.to_string()))?;
        match text.parse() {
            Ok(number) if (1..=options.len()).contains(&number) => return Ok(number),
            _ => eprintln!("Answer with a number from 1 to {}.", options.len()),
        }
    }
}

fn confirmed(input: &mut dyn BufRead, question: &str) -> Result<bool> {
    Ok(matches!(
        answer(input, &format!("{question} y or n"), Some("n"))?
            .to_lowercase()
            .as_str(),
        "y" | "yes"
    ))
}

fn memory() -> Option<u64> {
    let (pages, size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    u64::try_from(pages)
        .ok()?
        .checked_mul(u64::try_from(size).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_model_is_stored_with_one_prefix() {
        for (answer, stored) in [
            ("haiku", "claude"),
            ("claude", "claude"),
            ("claude:", "claude"),
            ("sonnet", "claude:sonnet"),
            ("claude:sonnet", "claude:sonnet"),
        ] {
            assert_eq!(claude_model(answer), stored, "{answer}");
        }
    }
}
