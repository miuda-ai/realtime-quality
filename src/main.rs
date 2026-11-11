use std::{io::IsTerminal, path::PathBuf, process::ExitCode};
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
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let result: Result<()> = async {
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
        tracing::info!(address = %listener.local_addr()?, "quality service listening");
        service.serve(listener, async { let _ = tokio::signal::ctrl_c().await; }).await
    }.await;
    match result {
        Ok(()) => {
            tracing::info!("quality service stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "quality service failed");
            ExitCode::FAILURE
        }
    }
}
