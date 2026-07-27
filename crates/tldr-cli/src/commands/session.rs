//! Queryable live agent-session token, cost, and context-injection ledger.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::daemon::{
    send_command, AllSessionsSummary, DaemonCommand, DaemonResponse, SessionStats,
};
use crate::output::OutputFormat;

#[derive(Debug, Clone, Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SessionCommand {
    /// Show provider usage, cost, and tldr-injected context for a live session.
    Stats(SessionStatsArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SessionStatsArgs {
    /// Project root whose daemon owns the session.
    #[arg(long, short = 'p', default_value = ".")]
    pub project: PathBuf,

    /// Host session identifier. Omit for the project-wide aggregate.
    #[arg(long, short = 's')]
    pub session: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionStatsOutput {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    all_sessions: Option<AllSessionsSummary>,
    caveat: &'static str,
}

impl SessionArgs {
    pub fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        match &self.command {
            SessionCommand::Stats(args) => args.run(format, quiet),
        }
    }
}

impl SessionStatsArgs {
    fn run(&self, format: OutputFormat, quiet: bool) -> Result<()> {
        let project = self
            .project
            .canonicalize()
            .unwrap_or_else(|_| self.project.clone());
        let runtime = tokio::runtime::Runtime::new()?;
        let response = runtime.block_on(send_command(
            &project,
            &DaemonCommand::Status {
                session: self.session.clone(),
            },
        ))?;
        let DaemonResponse::FullStatus {
            session_stats,
            all_sessions,
            ..
        } = response
        else {
            bail!("daemon returned an unexpected status response");
        };
        let output = SessionStatsOutput {
            source: "agent_hooks",
            session: session_stats,
            all_sessions,
            caveat: "provider token/cost fields are exact only when the agent host includes usage telemetry; tldr injected_tokens are measured locally",
        };
        if quiet {
            return Ok(());
        }
        match format {
            OutputFormat::Compact => println!("{}", serde_json::to_string(&output)?),
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
            _ => print_text(&output),
        }
        Ok(())
    }
}

fn print_text(output: &SessionStatsOutput) {
    println!("TLDR agent session usage");
    if let Some(session) = &output.session {
        println!("Session: {}", session.session_id);
        println!(
            "Provider input/output: {}/{}",
            session.input_tokens, session.output_tokens
        );
        println!("TLDR context injected: {} tokens", session.injected_tokens);
        println!("Provider-reported cost: ${:.6}", session.cost_usd);
    } else if let Some(all) = &output.all_sessions {
        println!("Sessions: {}", all.active_sessions);
        println!(
            "Provider input/output: {}/{}",
            all.total_input_tokens, all.total_output_tokens
        );
        println!(
            "TLDR context injected: {} tokens",
            all.total_injected_tokens
        );
        println!("Provider-reported cost: ${:.6}", all.total_cost_usd);
    }
    println!("Note: {}", output.caveat);
}
