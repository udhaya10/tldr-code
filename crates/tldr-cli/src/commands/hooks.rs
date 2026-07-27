//! Fail-open lifecycle hook bridge for Claude Code and compatible agents.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};

use crate::commands::daemon::{send_command, ContextPack, DaemonCommand, DaemonResponse};

const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Args)]
pub struct HookArgs {
    /// Project whose resident daemon should supply context.
    #[arg(long, default_value = ".")]
    pub project: PathBuf,

    /// Maximum context budget injected into the agent prompt.
    #[arg(long, default_value_t = 2_000)]
    pub max_tokens: usize,

    /// Hard daemon round-trip budget. Hook failures always fail open.
    #[arg(long, default_value_t = 350)]
    pub timeout_ms: u64,
}

impl HookArgs {
    pub fn run(&self) -> Result<()> {
        let mut raw = Vec::new();
        std::io::stdin()
            .take(MAX_HOOK_INPUT_BYTES)
            .read_to_end(&mut raw)
            .context("read hook input")?;
        let input: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
        let output = run_hook(&self.project, self.max_tokens, self.timeout_ms, &input);
        println!("{}", serde_json::to_string(&output)?);
        Ok(())
    }
}

fn run_hook(project: &Path, max_tokens: usize, timeout_ms: u64, input: &Value) -> Value {
    let command = lifecycle_command(project, max_tokens, input);
    let event = command_event(&command).to_string();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return json!({}),
    };

    let response = runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            send_command(project, &command),
        )
        .await
    });
    let Ok(Ok(DaemonResponse::Result(value))) = response else {
        return json!({});
    };
    let Ok(pack) = serde_json::from_value::<ContextPack>(value) else {
        return json!({});
    };
    if pack.content.is_empty() || !matches!(event.as_str(), "UserPromptSubmit" | "SessionStart") {
        return json!({});
    }

    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": pack.content
        }
    })
}

fn lifecycle_command(project: &Path, max_tokens: usize, input: &Value) -> DaemonCommand {
    let event = string_at(input, &["hook_event_name"]).unwrap_or("Unknown");
    let session = string_at(input, &["session_id"]).unwrap_or("unknown");
    let prompt = string_at(input, &["prompt"]).unwrap_or_default();
    let source = string_at(input, &["source"]).map(str::to_owned);
    let files = touched_file(input)
        .and_then(|path| normalize_project_path(project, path))
        .into_iter()
        .collect();
    let symbols = input
        .pointer("/tool_input/symbol")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .into_iter()
        .collect();

    DaemonCommand::Inject {
        session: session.to_owned(),
        event: event.to_owned(),
        prompt: prompt.to_owned(),
        source,
        files,
        symbols,
        max_tokens,
        input_tokens: number_at(input, &["/usage/input_tokens", "/input_tokens"]),
        output_tokens: number_at(input, &["/usage/output_tokens", "/output_tokens"]),
        cost_usd: float_at(input, &["/usage/cost_usd", "/cost_usd"]),
    }
}

fn command_event(command: &DaemonCommand) -> &str {
    match command {
        DaemonCommand::Inject { event, .. } => event,
        _ => "Unknown",
    }
}

fn touched_file(input: &Value) -> Option<&str> {
    [
        "/tool_input/file_path",
        "/tool_input/path",
        "/tool_input/notebook_path",
    ]
    .into_iter()
    .find_map(|pointer| input.pointer(pointer).and_then(Value::as_str))
}

fn normalize_project_path(project: &Path, raw: &str) -> Option<String> {
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        path.strip_prefix(project).ok()?
    } else {
        path
    };
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn string_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .or_else(|| value.pointer(key))
            .and_then(Value::as_str)
    })
}

fn number_at(value: &Value, pointers: &[&str]) -> u64 {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn float_at(value: &Value, pointers: &[&str]) -> f64 {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_f64))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_prompt_and_usage_into_inject_command() {
        let input = json!({
            "session_id": "abc",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "where is auth?",
            "usage": {"input_tokens": 12, "output_tokens": 3, "cost_usd": 0.01}
        });
        let DaemonCommand::Inject {
            session,
            event,
            prompt,
            input_tokens,
            output_tokens,
            cost_usd,
            ..
        } = lifecycle_command(Path::new("/repo"), 123, &input)
        else {
            panic!("expected inject");
        };
        assert_eq!(
            (session.as_str(), event.as_str()),
            ("abc", "UserPromptSubmit")
        );
        assert_eq!(prompt, "where is auth?");
        assert_eq!((input_tokens, output_tokens, cost_usd), (12, 3, 0.01));
    }

    #[test]
    fn maps_tool_file_relative_to_project() {
        let input = json!({
            "session_id": "abc",
            "hook_event_name": "PostToolUse",
            "tool_input": {"file_path": "/repo/src/lib.rs"}
        });
        let DaemonCommand::Inject { files, .. } =
            lifecycle_command(Path::new("/repo"), 123, &input)
        else {
            panic!("expected inject");
        };
        assert_eq!(files, vec!["src/lib.rs"]);
    }
}
