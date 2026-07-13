//! DockerSmith — a full-screen terminal UI suite for Docker.
//!
//! Entry point: parses CLI args, sets up the terminal, and runs the async app.

mod cli;
mod config;
mod docker;
mod md;
mod notify;
mod registry;
mod selfupdate;
mod theme;
mod tui;
mod util;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load (or create) the user config early so both TUI and headless paths share it.
    let cfg = config::Config::load()?;

    match cli.command {
        Some(Command::Check { host }) => {
            cli::run_check(cfg, host).await?;
        }
        Some(Command::Space { host }) => {
            cli::run_space(cfg, host).await?;
        }
        Some(Command::Apply { container, host }) => {
            cli::run_apply(cfg, container, host).await?;
        }
        Some(Command::Doctor) => {
            cli::run_doctor(cfg).await?;
        }
        Some(Command::SelfUpdate) => {
            selfupdate::run(cfg.github_token.as_deref()).await?;
        }
        None => {
            // Default: launch the full-screen TUI.
            tui::run(cfg).await?;
        }
    }

    Ok(())
}
