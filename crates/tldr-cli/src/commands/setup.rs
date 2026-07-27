//! Idempotent agent-harness wiring for tldr MCP and lifecycle hooks.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut, Item, Table};

use crate::output::OutputFormat;

const TLDR_HOOK_COMMAND: &str = "tldr hook --project .";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SetupAgent {
    Claude,
    Cursor,
    Codex,
    All,
}

#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    /// Agent integration to configure. Omit to configure detected agents.
    #[arg(value_enum)]
    pub agent: Option<SetupAgent>,

    /// Project root whose agent configuration should be updated.
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,

    /// Remove tldr-owned integration entries.
    #[arg(long)]
    pub remove: bool,
}

#[derive(Debug, Serialize)]
struct SetupOutput {
    status: &'static str,
    action: &'static str,
    project: PathBuf,
    agents: Vec<&'static str>,
    files: Vec<PathBuf>,
    tldr: &'static str,
    tldr_mcp: &'static str,
    next: &'static str,
}

impl SetupArgs {
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let project = self
            .project
            .canonicalize()
            .unwrap_or_else(|_| self.project.clone());
        let mut files = Vec::new();
        let mut agents = Vec::new();
        for agent in selected_agents(self.agent, &project) {
            match agent {
                SetupAgent::Claude => configure_claude(&project, self.remove, &mut files)?,
                SetupAgent::Cursor => configure_cursor(&project, self.remove, &mut files)?,
                SetupAgent::Codex => configure_codex(&project, self.remove, &mut files)?,
                SetupAgent::All => unreachable!(),
            }
            agents.push(agent_name(agent));
        }
        files.sort();
        files.dedup();
        let output = SetupOutput {
            status: "ok",
            action: if self.remove { "remove" } else { "setup" },
            project,
            agents,
            files,
            tldr: if which::which("tldr").is_ok() {
                "available"
            } else {
                "missing"
            },
            tldr_mcp: if which::which("tldr-mcp").is_ok() {
                "available"
            } else {
                "missing"
            },
            next: "run `tldr init` once for project daemon lifecycle",
        };
        if quiet {
            return Ok(());
        }
        match format {
            OutputFormat::Compact => println!("{}", serde_json::to_string(&output)?),
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
            _ => {
                println!(
                    "{} tldr integration for {}",
                    output.action,
                    output.agents.join(", ")
                );
                for file in &output.files {
                    println!("  {}", file.display());
                }
                println!("{}", output.next);
            }
        }
        Ok(())
    }
}

fn selected_agents(agent: Option<SetupAgent>, project: &Path) -> Vec<SetupAgent> {
    match agent {
        Some(SetupAgent::All) => vec![SetupAgent::Claude, SetupAgent::Cursor, SetupAgent::Codex],
        Some(one) => vec![one],
        None => {
            let detected = [
                (SetupAgent::Claude, ".claude", "claude"),
                (SetupAgent::Cursor, ".cursor", "cursor"),
                (SetupAgent::Codex, ".codex", "codex"),
            ]
            .into_iter()
            .filter_map(|(agent, directory, binary)| {
                (project.join(directory).exists() || which::which(binary).is_ok()).then_some(agent)
            })
            .collect::<Vec<_>>();
            if detected.is_empty() {
                vec![SetupAgent::Claude, SetupAgent::Cursor, SetupAgent::Codex]
            } else {
                detected
            }
        }
    }
}

fn agent_name(agent: SetupAgent) -> &'static str {
    match agent {
        SetupAgent::Claude => "claude",
        SetupAgent::Cursor => "cursor",
        SetupAgent::Codex => "codex",
        SetupAgent::All => "all",
    }
}

fn configure_claude(project: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    update_mcp_json(&project.join(".mcp.json"), remove, changed)?;
    update_hooks_json(
        &project.join(".claude/settings.local.json"),
        remove,
        changed,
    )
}

fn configure_cursor(project: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    update_mcp_json(&project.join(".cursor/mcp.json"), remove, changed)
}

fn configure_codex(project: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    update_hooks_json(&project.join(".codex/hooks.json"), remove, changed)?;
    update_codex_toml(&project.join(".codex/config.toml"), remove, changed)
}

fn update_mcp_json(path: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    let mut root = read_json_object(path)?;
    let servers = object_mut(&mut root, "mcpServers")?;
    if remove {
        servers.remove("tldr");
    } else {
        servers.insert(
            "tldr".to_string(),
            json!({"command": "tldr-mcp", "args": []}),
        );
    }
    write_json_if_changed(path, Value::Object(root), changed)
}

fn update_hooks_json(path: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    let mut root = read_json_object(path)?;
    let hooks = object_mut(&mut root, "hooks")?;
    for (event, matcher) in [
        ("UserPromptSubmit", None),
        ("SessionStart", Some("startup|resume|compact|fork")),
        ("PostToolUse", Some("Read|Edit|Write")),
        ("SessionEnd", None),
    ] {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let array = entries
            .as_array_mut()
            .context("hook event must be an array")?;
        array.retain(|entry| !contains_tldr_hook(entry));
        if !remove {
            let mut group = Map::new();
            if let Some(matcher) = matcher {
                group.insert("matcher".to_string(), Value::String(matcher.to_string()));
            }
            group.insert(
                "hooks".to_string(),
                json!([{"type": "command", "command": TLDR_HOOK_COMMAND, "timeout": 1}]),
            );
            array.push(Value::Object(group));
        }
    }
    write_json_if_changed(path, Value::Object(root), changed)
}

fn contains_tldr_hook(value: &Value) -> bool {
    value
        .pointer("/hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.starts_with("tldr hook "))
            })
        })
}

fn update_codex_toml(path: &Path, remove: bool, changed: &mut Vec<PathBuf>) -> Result<()> {
    let before = fs::read_to_string(path).unwrap_or_default();
    let mut document = before
        .parse::<DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    if remove {
        if let Some(servers) = document.get_mut("mcp_servers").and_then(Item::as_table_mut) {
            servers.remove("tldr");
        }
    } else {
        if !document.contains_key("mcp_servers") {
            document["mcp_servers"] = Item::Table(Table::new());
        }
        document["mcp_servers"]["tldr"]["command"] = value("tldr-mcp");
    }
    let after = document.to_string();
    if after != before {
        write_text(path, &after)?;
        changed.push(path.to_path_buf());
    }
    Ok(())
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("parse {}", path.display()))?;
    value
        .as_object()
        .cloned()
        .with_context(|| format!("{} must contain a JSON object", path.display()))
}

fn object_mut<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    root.entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("configuration key `{key}` must be a JSON object"))
}

fn write_json_if_changed(path: &Path, value: Value, changed: &mut Vec<PathBuf>) -> Result<()> {
    let after = format!("{}\n", serde_json::to_string_pretty(&value)?);
    let before = fs::read_to_string(path).unwrap_or_default();
    if after != before {
        write_text(path, &after)?;
        changed.push(path.to_path_buf());
    }
    Ok(())
}

fn write_text(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_setup_is_idempotent_and_preserves_existing_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join(".claude/settings.local.json");
        write_text(
            &settings,
            r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"other"}]}]}}"#,
        )
        .unwrap();
        let mut changed = Vec::new();
        configure_claude(temp.path(), false, &mut changed).unwrap();
        let once = fs::read_to_string(&settings).unwrap();
        configure_claude(temp.path(), false, &mut changed).unwrap();
        assert_eq!(once, fs::read_to_string(&settings).unwrap());
        assert!(once.contains("\"command\": \"other\""));
        assert!(once.contains("tldr hook --project ."));
    }

    #[test]
    fn codex_setup_and_remove_only_own_entries() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".codex/config.toml");
        write_text(&config, "[mcp_servers.other]\ncommand = \"other\"\n").unwrap();
        let mut changed = Vec::new();
        configure_codex(temp.path(), false, &mut changed).unwrap();
        configure_codex(temp.path(), true, &mut changed).unwrap();
        let result = fs::read_to_string(config).unwrap();
        assert!(result.contains("mcp_servers.other"));
        assert!(!result.contains("mcp_servers.tldr"));
    }
}
