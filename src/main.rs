use std::path::PathBuf;
use anyhow::{Context, Result};
use clap::Parser;
use realtime_quality::{QualityService, config::Config};

#[derive(Parser)]
#[command(version, about = "Realtime ASR keyword and beep quality service")]
struct Cli {
    /// Service TOML configuration. Omit to run the default beep-only service.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config: Config = match cli.config {
        Some(path) => toml::from_str(&std::fs::read_to_string(&path)
            .with_context(|| format!("read configuration {}", path.display()))?)
            .with_context(|| format!("parse configuration {}", path.display()))?,
        None => Config::default(),
    };
    let address = config.listen.clone().unwrap_or_else(|| "127.0.0.1:8095".into());
    let service = QualityService::prepare(config)?;
    let listener = tokio::net::TcpListener::bind(&address).await?;
    eprintln!("realtime-quality listening on {}", listener.local_addr()?);
    service.serve(listener, async { let _ = tokio::signal::ctrl_c().await; }).await
}
