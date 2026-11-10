use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ITEM_CHARS, context};
use crate::clip;
use crate::model::{Answer, Client, Question, Questions};

const PICK_AT: f64 = 0.6;
const MAX_SKILLS: usize = 254;

pub(super) async fn run(input: &Value, client: &Client) -> Result<String> {
    let prompt = input["prompt"].as_str().context("no prompt")?;
    let mut options = BTreeMap::new();
    let roots = [
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(std::env::home_dir()?.join(".claude"))),
        std::env::var_os("CLAUDE_PROJECT_DIR")
            .map(|project| PathBuf::from(project).join(".claude")),
        input["cwd"]
            .as_str()
            .map(|cwd| Path::new(cwd).join(".claude")),
    ];
    for root in roots.into_iter().flatten() {
        let Ok(entries) = std::fs::read_dir(root.join("skills")) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Some((name, description)) = skill_summary(&entry.path().join("SKILL.md")) {
                options.insert(name, clip(&description, ITEM_CHARS));
            }
        }
    }
    if options.is_empty() {
        return Ok(String::new());
    }
    anyhow::ensure!(
        options.len() <= MAX_SKILLS,
        "more than {MAX_SKILLS} skills to choose from"
    );
    options.insert("none".into(), "No listed skill fits this request".into());
    let question = Questions::from([(
        "skill".into(),
        Question::Pick {
            text: "Which skill should handle this request?".into(),
            options,
        },
    )]);
    let answers = client.ask(prompt, &question).await?;
    let Answer::Pick {
        pick, confidence, ..
    } = &answers["skill"]
    else {
        return Ok(String::new());
    };
    if pick == "none" || confidence.unwrap_or(0.0) < PICK_AT {
        return Ok(String::new());
    }
    Ok(context(
        &"UserPromptSubmit".into(),
        &format!("iu2u compass: the skill \"{pick}\" likely fits this request."),
    ))
}

fn skill_summary(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let frontmatter = text.strip_prefix("---")?.split("\n---").next()?;
    let field = |key: &str| {
        let mut lines = frontmatter
            .lines()
            .skip_while(|line| !line.starts_with(key));
        let first = lines.next()?.strip_prefix(key)?.trim();
        let first = if first.starts_with(['>', '|']) {
            ""
        } else {
            first
        };
        let continued = lines
            .take_while(|line| line.starts_with([' ', '\t']))
            .map(str::trim);
        let joined = std::iter::once(first)
            .chain(continued)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let value = joined.trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_string())
    };
    let name =
        field("name:").or_else(|| Some(path.parent()?.file_name()?.to_str()?.to_string()))?;
    Some((name, field("description:")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_summary_reads_frontmatter_and_falls_back_to_the_folder_name() {
        let dir = std::env::temp_dir().join("iu2u-skill-summary/fallback-name");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\ndescription: \"Does things\"\n---\n",
        )
        .unwrap();
        assert_eq!(
            skill_summary(&dir.join("SKILL.md")),
            Some(("fallback-name".into(), "Does things".into()))
        );
    }

    #[test]
    fn skill_summary_reads_block_and_single_quoted_values() {
        let dir = std::env::temp_dir().join("iu2u-skill-summary/block");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: 'deploy'\ndescription: >-\n  Ship the app\n  to staging\nmetadata: x\n---\n",
        )
        .unwrap();
        assert_eq!(
            skill_summary(&dir.join("SKILL.md")),
            Some(("deploy".into(), "Ship the app to staging".into()))
        );
    }

    #[tokio::test]
    async fn too_many_skills_are_reported_instead_of_ignored() {
        let project = std::env::temp_dir().join("iu2u-skill-summary/many");
        for index in 0..=MAX_SKILLS {
            let skill = project.join(format!(".claude/skills/skill-{index}"));
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                "---\ndescription: Does things\n---\n",
            )
            .unwrap();
        }
        let client = Client::new(
            "http://127.0.0.1:9",
            None,
            "model-x".into(),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let error = run(
            &serde_json::json!({ "prompt": "ship it", "cwd": project }),
            &client,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("more than 254 skills"));
    }
}
