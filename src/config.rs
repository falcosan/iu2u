use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Value};

use crate::write_atomically;

pub(crate) const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const API_KEY_VARIABLES: [&str; 2] = ["CLAUDE_PLUGIN_OPTION_API_KEY", "IU2U_API_KEY"];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Url,
    Effort,
    Milliseconds,
    Switch,
}

struct Key {
    name: &'static str,
    kind: Kind,
    default: Option<&'static str>,
}

impl Key {
    const fn new(name: &'static str, kind: Kind, default: Option<&'static str>) -> Self {
        Self {
            name,
            kind,
            default,
        }
    }
}

const KEYS: [Key; 11] = [
    Key::new("model", Kind::Text, None),
    Key::new("effort", Kind::Effort, Some("medium")),
    Key::new("base_url", Kind::Url, Some("http://127.0.0.1:11435")),
    Key::new("timeout_ms", Kind::Milliseconds, Some("10000")),
    Key::new("guard", Kind::Switch, Some("true")),
    Key::new("proof", Kind::Switch, Some("true")),
    Key::new("trim", Kind::Switch, Some("false")),
    Key::new("compass", Kind::Switch, Some("false")),
    Key::new("prune", Kind::Switch, Some("false")),
    Key::new("vision", Kind::Switch, Some("false")),
    Key::new("browser", Kind::Text, None),
];

#[derive(Default)]
pub struct Config {
    path: Option<PathBuf>,
    document: DocumentMut,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = path();
        let text = match &path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot read {}", path.display()));
                }
            },
            None => String::new(),
        };
        let config = Self {
            document: text
                .parse()
                .with_context(|| format!("{} is not valid TOML", shown(path.as_deref())))?,
            path,
        };
        let file = || shown(config.path.as_deref());
        for (name, item) in config.document.iter() {
            let key = KEYS.iter().find(|key| key.name == name).with_context(|| {
                format!("{}: unknown key `{name}`; the keys are {}", file(), names())
            })?;
            item.as_value()
                .and_then(|value| text_of(key.kind, value))
                .with_context(|| format!("{}: `{name}` must be {}", file(), expected(key.kind)))?;
        }
        Ok(config)
    }

    pub fn get(&self, name: &str) -> Option<String> {
        variable(name)
            .or_else(|| self.in_file(name))
            .or_else(|| default(name).map(str::to_owned))
    }

    pub(crate) fn origin(&self, name: &str) -> String {
        let file = shown(self.path.as_deref());
        match (variable(name), self.in_file(name)) {
            (Some(_), Some(hidden)) => {
                format!("{}, overriding {hidden} in {file}", variable_name(name))
            }
            (Some(_), None) => variable_name(name),
            (None, Some(_)) => file,
            (None, None) => "default".into(),
        }
    }

    #[cfg(unix)]
    pub(crate) fn saved(&self, name: &str) -> Option<String> {
        self.in_file(name)
            .or_else(|| default(name).map(str::to_owned))
    }

    fn in_file(&self, name: &str) -> Option<String> {
        let key = KEYS.iter().find(|key| key.name == name)?;
        text_of(key.kind, self.document.get(name)?.as_value()?).filter(|text| !text.is_empty())
    }

    fn write(&self) -> Result<()> {
        let path = self.path.as_deref().context(NO_LOCATION)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        write_atomically(path, self.document.to_string().as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

const NO_LOCATION: &str = "there is no config file: set XDG_CONFIG_HOME or HOME";
const HOMES: &[&str] = if cfg!(windows) {
    &["HOME", "USERPROFILE"]
} else {
    &["HOME"]
};

pub fn path() -> Option<PathBuf> {
    base("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("config.toml"))
}

#[cfg(unix)]
pub(crate) fn data_dir() -> Option<PathBuf> {
    base("XDG_DATA_HOME", ".local/share")
}

fn base(variable: &str, under_home: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            HOMES
                .iter()
                .find_map(|name| std::env::var_os(name).filter(|home| !home.is_empty()))
                .map(|home| Path::new(&home).join(under_home))
        })
        .map(|dir| dir.join("iu2u"))
}

fn shown(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_default()
}

pub(crate) fn default(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|key| key.name == name)?.default
}

pub(crate) fn variable_name(name: &str) -> String {
    format!("IU2U_{}", name.to_uppercase())
}

pub(crate) fn variable(name: &str) -> Option<String> {
    std::env::var(variable_name(name))
        .ok()
        .filter(|value| !value.is_empty())
}

pub(crate) fn api_key() -> Option<(String, &'static str)> {
    API_KEY_VARIABLES
        .iter()
        .find_map(|name| Some((std::env::var(name).ok()?, *name)))
        .filter(|(key, _)| !key.is_empty())
}

fn names() -> String {
    KEYS.map(|key| key.name).join(", ")
}

fn expected(kind: Kind) -> String {
    match kind {
        Kind::Text => "a string".into(),
        Kind::Url => "an http or https URL".into(),
        Kind::Effort => format!("one of {}", EFFORTS.join(", ")),
        Kind::Milliseconds => "a positive whole number of milliseconds".into(),
        Kind::Switch => "true or false".into(),
    }
}

fn text_of(kind: Kind, value: &Value) -> Option<String> {
    match (kind, value) {
        (Kind::Switch, Value::Boolean(on)) => Some(on.value().to_string()),
        (Kind::Milliseconds, Value::Integer(ms)) if *ms.value() > 0 => Some(ms.value().to_string()),
        (Kind::Text, Value::String(text)) => Some(text.value().clone()),
        (Kind::Url, Value::String(url))
            if reqwest::Url::parse(url.value())
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https")) =>
        {
            Some(url.value().clone())
        }
        (Kind::Effort, Value::String(effort)) if EFFORTS.contains(&effort.value().as_str()) => {
            Some(effort.value().clone())
        }
        _ => None,
    }
}

fn key(name: &str) -> Result<&'static Key> {
    if name == "api_key" {
        bail!(
            "the API key is not kept in the config file: set it with /plugin configure iu2u@iu2u in Claude Code, or with IU2U_API_KEY"
        );
    }
    KEYS.iter()
        .find(|key| key.name == name)
        .with_context(|| format!("unknown key `{name}`; the keys are {}", names()))
}

pub fn list() -> Result<String> {
    let config = Config::load()?;
    let rows: Vec<(&str, String, String)> = KEYS
        .iter()
        .map(|key| {
            let value = config.get(key.name).unwrap_or_else(|| "(none)".into());
            (key.name, value, config.origin(key.name))
        })
        .chain(std::iter::once(match api_key() {
            Some((_, variable)) => ("api_key", "set".into(), variable.into()),
            None => ("api_key", "not set".into(), String::new()),
        }))
        .collect();
    let width = rows
        .iter()
        .map(|(_, value, _)| value.len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (name, value, origin) in rows {
        writeln!(text, "{name:<10} {value:<width$}  {origin}").ok();
    }
    Ok(text)
}

pub fn get(name: &str) -> Result<Option<String>> {
    key(name)?;
    Ok(Config::load()?.get(name))
}

pub fn set(name: &str, text: &str) -> Result<()> {
    write(&[(name, text.to_owned())])?;
    if name == "model"
        && let Some(local) = crate::model::Reference::parse(text)?
        && let Some(hint) = not_ready(text, &local)
    {
        eprintln!("{hint}");
    }
    Ok(())
}

#[cfg(unix)]
fn not_ready(text: &str, local: &crate::model::Reference) -> Option<String> {
    if let Err(error) = crate::model::decision::supported() {
        return Some(error.to_string());
    }
    crate::model::decision::home()
        .ok()
        .and_then(|home| local.installed(&home))
        .is_none()
        .then(|| format!("{text} is not installed yet: run iu2u init to download it"))
}

#[cfg(not(unix))]
fn not_ready(_: &str, _: &crate::model::Reference) -> Option<String> {
    Some(crate::model::UNSUPPORTED.to_owned())
}

#[cfg(unix)]
pub(crate) fn checked(name: &str, text: &str) -> Result<()> {
    value(name, text).map(drop)
}

fn value(name: &str, text: &str) -> Result<Value> {
    let key = key(name)?;
    let stored = match name {
        "model" => crate::model::Reference::parse(text)?.map(|local| local.stored(None)),
        _ => None,
    };
    let text = stored.as_deref().unwrap_or(text);
    match key.kind {
        Kind::Switch => text.parse::<bool>().ok().map(Value::from),
        Kind::Milliseconds => text.parse::<i64>().ok().map(Value::from),
        Kind::Text | Kind::Url | Kind::Effort => Some(Value::from(text)),
    }
    .filter(|value| text_of(key.kind, value).is_some())
    .with_context(|| format!("`{name}` must be {}", expected(key.kind)))
}

pub(crate) fn write(settings: &[(&str, String)]) -> Result<()> {
    let values = settings
        .iter()
        .map(|(name, text)| Ok((*name, value(name, text)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut config = Config::load()?;
    for (name, mut value) in values {
        if let Some(old) = config.document.get(name).and_then(Item::as_value) {
            *value.decor_mut() = old.decor().clone();
        }
        config.document[name] = Item::Value(value);
    }
    config.write()
}

pub fn unset(name: &str) -> Result<()> {
    key(name)?;
    let mut config = Config::load()?;
    config.path.as_ref().context(NO_LOCATION)?;
    if config.document.remove(name).is_some() {
        config.write()?;
    }
    Ok(())
}

pub fn location() -> Result<PathBuf> {
    path().context(NO_LOCATION)
}
