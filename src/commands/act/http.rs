use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use reqwest::multipart::{Form, Part as FilePart};
use reqwest::{Client, Method};
use serde_json::Value;

use super::script::{Body, Part, Request};

const ANSWER_WITHIN: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 1 << 20;

pub(super) fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .timeout(ANSWER_WITHIN)
        .build()
        .context("cannot start the HTTP client")
}

pub(super) async fn send(
    client: &Client,
    request: &Request,
) -> Result<(Vec<String>, Option<Value>)> {
    let method = Method::from_bytes(request.method.as_bytes())?;
    let mut builder = client.request(method, &request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let typed = request
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
    match &request.body {
        Some(Body::Json(json)) => {
            let Part::Text(text) = json else {
                bail!("the json file is read when the request is expanded");
            };
            if !typed {
                builder =
                    builder.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            }
            builder = builder.body(text.clone());
        }
        Some(Body::Form(fields))
            if fields.iter().all(|(_, part)| matches!(part, Part::Text(_))) =>
        {
            let body = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().filter_map(|(name, part)| match part {
                    Part::Text(text) => Some((name, text)),
                    Part::File(..) => None,
                }))
                .finish();
            if !typed {
                builder = builder.header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
            builder = builder.body(body);
        }
        Some(Body::Form(fields)) => {
            anyhow::ensure!(
                !typed,
                "a form with a file sets its own content-type; leave out the content-type header"
            );
            let mut form = Form::new();
            for (name, part) in fields {
                form = match part {
                    Part::Text(text) => form.text(name.clone(), text.clone()),
                    Part::File(_, path) => form.part(name.clone(), file(path).await?),
                };
            }
            builder = builder.multipart(form);
        }
        None => {}
    }
    let mut response = builder.send().await.context("the request failed")?;
    let status = response.status();
    let mut lines = vec![
        format!(
            "HTTP {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default()
        )
        .trim_end()
        .to_string(),
    ];
    for (name, value) in response.headers() {
        lines.push(format!(
            "{name}: {}",
            String::from_utf8_lossy(value.as_bytes())
        ));
    }
    lines.push(String::new());
    let mut body = Vec::new();
    let mut cut = false;
    while let Some(chunk) = response.chunk().await.context("the response broke off")? {
        let room = MAX_BODY_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room {
            cut = true;
            break;
        }
    }
    if cut
        && let Err(error) = std::str::from_utf8(&body)
        && error.error_len().is_none()
    {
        body.truncate(error.valid_up_to());
    }
    let mut json = None;
    match String::from_utf8(body) {
        Ok(text) if !text.contains('\0') => {
            json = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok();
            match &json {
                Some(json) => lines.extend(
                    leaves(json)
                        .into_iter()
                        .map(|(path, leaf)| line(&path, leaf)),
                ),
                None => lines.extend(text.lines().map(str::to_owned)),
            }
        }
        Ok(text) => lines.push(format!("(binary body: {} bytes)", text.len())),
        Err(error) => lines.push(format!("(binary body: {} bytes)", error.as_bytes().len())),
    }
    if cut {
        lines.push(format!(
            "(the body continues; only its first {MAX_BODY_BYTES} bytes are shown)"
        ));
    }
    Ok((lines, json))
}

async fn file(path: &str) -> Result<FilePart> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("cannot read {path}"))?;
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(FilePart::bytes(bytes)
        .file_name(name)
        .mime_str(&super::browser::content_type(path))?)
}

pub(super) fn leaves(json: &Value) -> Vec<(String, &Value)> {
    let mut leaves = Vec::new();
    gather(json, String::new(), &mut leaves);
    leaves
}

pub(super) fn line(path: &str, leaf: &Value) -> String {
    if path.is_empty() {
        leaf.to_string()
    } else {
        format!("{path}: {leaf}")
    }
}

fn gather<'a>(value: &'a Value, path: String, leaves: &mut Vec<(String, &'a Value)>) {
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (name, field) in fields {
                let plain = name.starts_with(|c: char| c.is_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                let path = match (plain, path.is_empty()) {
                    (true, true) => name.clone(),
                    (true, false) => format!("{path}.{name}"),
                    (false, _) => format!("{path}[{}]", Value::from(name.as_str())),
                };
                gather(field, path, leaves);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (index, item) in items.iter().enumerate() {
                gather(item, format!("{path}[{index}]"), leaves);
            }
        }
        leaf => leaves.push((path, leaf)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shows_one_line_per_value_by_its_path() {
        let json = serde_json::json!({
            "people": [{"email": "a@b.c", "accounts": [], "joinedTo": null}],
            "two words": {"x-y": 1, "": true},
            "empty": {}
        });
        let lines: Vec<String> = leaves(&json)
            .into_iter()
            .map(|(path, leaf)| line(&path, leaf))
            .collect();
        assert_eq!(
            lines,
            [
                "empty: {}",
                "people[0].accounts: []",
                "people[0].email: \"a@b.c\"",
                "people[0].joinedTo: null",
                "[\"two words\"][\"\"]: true",
                "[\"two words\"][\"x-y\"]: 1",
            ]
        );
        assert_eq!(
            leaves(&serde_json::json!("plain"))
                .into_iter()
                .map(|(path, leaf)| line(&path, leaf))
                .collect::<Vec<_>>(),
            ["\"plain\""]
        );
    }
}
