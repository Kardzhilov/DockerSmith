//! Command-line interface definition and headless (non-TUI) entry points.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::docker::DockerClient;
use crate::util::format_bytes;

/// DockerSmith — manage Docker images, containers, updates, and disk usage from your terminal.
#[derive(Debug, Parser)]
#[command(name = "dockersmith", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check running containers for available image updates (headless), then exit.
    Check {
        /// Named host from your config to check (defaults to the local socket).
        #[arg(long)]
        host: Option<String>,
    },
    /// Print reclaimable disk usage (like `docker system df`) and exit.
    Space {
        /// Named host from your config to query (defaults to the local socket).
        #[arg(long)]
        host: Option<String>,
    },
    /// Apply an update to a container: pull its image and recreate it.
    Apply {
        /// Container name or id to update.
        container: String,
        /// Named host from your config (defaults to the local socket).
        #[arg(long)]
        host: Option<String>,
    },
    /// Verify Docker connectivity and environment, then exit.
    Doctor,
    /// Check for a newer DockerSmith release and update the binary in place.
    SelfUpdate,
}

/// Resolve a Docker client for an optional named host.
async fn connect(cfg: &Config, host: Option<String>) -> Result<DockerClient> {
    match host {
        Some(name) => {
            let h = cfg
                .hosts
                .iter()
                .find(|h| h.name == name)
                .with_context(|| format!("no host named '{name}' in config"))?;
            DockerClient::connect(&h.endpoint).await
        }
        None => DockerClient::connect_local().await,
    }
}

/// `dockersmith check`
pub async fn run_check(cfg: Config, host: Option<String>) -> Result<()> {
    let client = connect(&cfg, host).await?;
    let containers = client.list_containers(true).await?;

    println!("Checking {} container(s) for updates...\n", containers.len());

    let mut updates = 0usize;
    for c in &containers {
        let info = client.check_update_detailed(&c.image).await;
        use crate::docker::model::UpdateStatus;
        match info.status {
            UpdateStatus::UpdateAvailable => {
                updates += 1;
                let detail = info
                    .transition()
                    .map(|t| format!("  {t}"))
                    .unwrap_or_default();
                println!("  UPDATE   {:<24} {}{detail}", c.display_name(), c.image);
            }
            UpdateStatus::UpToDate => {
                let ver = info
                    .current_label()
                    .map(|v| format!("  ({v})"))
                    .unwrap_or_default();
                println!("  ok       {:<24} {}{ver}", c.display_name(), c.image);
            }
            UpdateStatus::LocalOnly => {
                println!("  local    {:<24} {}", c.display_name(), c.image);
            }
            UpdateStatus::Checking => {}
            UpdateStatus::Error(e) => {
                println!("  ?        {:<24} {}  ({e})", c.display_name(), c.image);
            }
        }
    }

    println!();
    if updates == 0 {
        println!("All containers are up to date.");
    } else {
        println!("{updates} update(s) available.");
    }
    Ok(())
}

/// `dockersmith space`
pub async fn run_space(cfg: Config, host: Option<String>) -> Result<()> {
    let client = connect(&cfg, host).await?;
    let usage = client.disk_usage().await?;

    println!("TYPE            TOTAL       RECLAIMABLE");
    println!(
        "Images          {:<11} {}",
        format_bytes(usage.images_total),
        format_bytes(usage.images_reclaimable)
    );
    println!(
        "Containers      {:<11} {}",
        format_bytes(usage.containers_total),
        format_bytes(usage.containers_reclaimable)
    );
    println!(
        "Volumes         {:<11} {}",
        format_bytes(usage.volumes_total),
        format_bytes(usage.volumes_reclaimable)
    );
    println!(
        "Build Cache     {:<11} {}",
        format_bytes(usage.build_cache_total),
        format_bytes(usage.build_cache_reclaimable)
    );
    println!();
    println!("Total reclaimable: {}", format_bytes(usage.total_reclaimable()));
    Ok(())
}

/// `dockersmith apply <container>`
pub async fn run_apply(cfg: Config, container: String, host: Option<String>) -> Result<()> {
    let client = connect(&cfg, host).await?;
    let containers = client.list_containers(true).await?;
    let target = containers
        .iter()
        .find(|c| c.name == container || c.id.starts_with(&container))
        .with_context(|| format!("no container named '{container}'"))?;

    println!("Updating {} ({})…", target.name, target.image);
    use crate::docker::{ApplyProgress, StageState};
    let result = client
        .apply_update(&target.id, &target.image, |p| match p {
            ApplyProgress::Stage(s, StageState::Start) => println!("  → {}", s.label()),
            ApplyProgress::Stage(s, StageState::Done) => {
                if s.label() != "Finished" {
                    println!("  ✔ {}", s.label());
                }
            }
            ApplyProgress::Stage(s, StageState::Failed(e)) => {
                eprintln!("  ✖ {} — {e}", s.label())
            }
            ApplyProgress::Log(l) => println!("      {l}"),
        })
        .await;
    match result {
        Ok(()) => {
            println!("Done.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// `dockersmith doctor`
pub async fn run_doctor(cfg: Config) -> Result<()> {
    println!("DockerSmith doctor\n");
    match DockerClient::connect_local().await {
        Ok(client) => match client.version().await {
            Ok(v) => println!("  [ok] local daemon reachable (API {v})"),
            Err(e) => println!("  [!!] connected but version query failed: {e}"),
        },
        Err(e) => println!("  [!!] cannot reach local daemon: {e}"),
    }

    println!("  config: {} host(s) defined", cfg.hosts.len());
    for h in &cfg.hosts {
        match DockerClient::connect(&h.endpoint).await {
            Ok(client) => match client.version().await {
                Ok(v) => println!("  [ok] host '{}' reachable (API {v})", h.name),
                Err(e) => println!("  [!!] host '{}' connect ok but version failed: {e}", h.name),
            },
            Err(e) => println!("  [!!] host '{}' unreachable: {e}", h.name),
        }
    }
    Ok(())
}
