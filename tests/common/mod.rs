#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use iu2u::model::Client;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method},
};

const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

pub fn client(model: &MockServer) -> Client {
    client_with_timeout(model, Duration::from_secs(5))
}

pub fn client_with_timeout(model: &MockServer, timeout: Duration) -> Client {
    Client::new(&model.uri(), Some("key".into()), "model-x".into(), timeout).unwrap()
}

pub fn reply(answers: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "model": "model-x", "answers": answers }))
}

pub async fn answer_when(model: &MockServer, state_contains: &str, answers: Value) {
    Mock::given(method("POST"))
        .and(body_string_contains(state_contains))
        .respond_with(reply(answers))
        .mount(model)
        .await;
}

pub async fn answer_always(model: &MockServer, answers: Value) {
    Mock::given(method("POST"))
        .respond_with(reply(answers))
        .mount(model)
        .await;
}

pub async fn fail_always(model: &MockServer, status: u16) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status))
        .mount(model)
        .await;
}

pub fn yes(probability: f64) -> Value {
    json!({ "type": "noul", "noul": probability })
}

pub fn temp_path(name: &str) -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = std::env::temp_dir().join("iu2u-tests");
        for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified.elapsed().unwrap_or_default() > STALE_AFTER);
            if stale {
                let path = entry.path();
                std::fs::remove_dir_all(&path)
                    .or_else(|_| std::fs::remove_file(&path))
                    .ok();
            }
        }
        let dir = root.join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        dir
    })
    .join(name)
}
