mod auth;
mod paperless;
pub mod spool;
mod storage;

use std::env;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::Result;
use libunftp::options::ActivePassiveMode;
use log::{error, info, warn};

use auth::UsernamePasswordAuthenticator;
use paperless::{PaperlessApi, PaperlessClient, PaperlessError};
use storage::PaperlessStorage;

const STARTUP_HEALTH_CHECK_MAX_ATTEMPTS: u32 = 5;
const STARTUP_HEALTH_CHECK_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const STARTUP_HEALTH_CHECK_MAX_BACKOFF: Duration = Duration::from_secs(16);

fn parse_port_range(src: &str) -> Result<RangeInclusive<u16>, String> {
    let parts: Vec<_> = src.split("-").collect();

    if parts.len() != 2 {
        return Err("Wrong format for port range, should be in the format 2222-3333".to_string());
    }

    let range_start: u16 = parts[0]
        .parse()
        .map_err(|_| "First number of port range can't be parsed")?;
    let range_end: u16 = parts[1]
        .parse()
        .map_err(|_| "Second number of port range can't be parsed")?;

    Ok(range_start..=range_end)
}

fn validate_listen_addr(addr: &str) -> Result<String, String> {
    if addr.parse::<std::net::SocketAddr>().is_ok() {
        Ok(addr.to_string())
    } else {
        Err(format!(
            "Invalid listen address '{}'. Must be in format IP:PORT (e.g., 0.0.0.0:2121 or [::]:2121)",
            addr
        ))
    }
}

async fn validate_paperless_connection_with_retry(
    paperless_client: &dyn PaperlessApi,
) -> Result<(), PaperlessError> {
    let mut attempt = 1;
    let mut backoff = STARTUP_HEALTH_CHECK_INITIAL_BACKOFF;

    loop {
        match paperless_client.health_check().await {
            Ok(()) => return Ok(()),
            Err(err) if attempt < STARTUP_HEALTH_CHECK_MAX_ATTEMPTS => {
                warn!(
                    "Paperless API health check attempt {attempt}/{} failed: {err}. Retrying in {}s",
                    STARTUP_HEALTH_CHECK_MAX_ATTEMPTS,
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
                backoff = (backoff * 2).min(STARTUP_HEALTH_CHECK_MAX_BACKOFF);
            }
            Err(err) => return Err(err),
        }
    }
}

/// The FTP server part enables both active mode and passive mode at the same time for better
/// flexibility.
#[derive(Parser)]
#[command(name = "ftp-paperless-bridge", author, about, version)]
pub struct CliArgs {
    /// Be verbose
    #[arg(short, long, env = "FTP_PAPERLESS_BRIDGE_VERBOSE")]
    pub verbose: bool,

    /// Listen address (must include both IP and port)
    ///
    /// Examples: 0.0.0.0:2121, 127.0.0.1:2121, [::]:2121
    #[arg(short, long, env = "FTP_PAPERLESS_BRIDGE_LISTEN", value_parser = validate_listen_addr)]
    pub listen: String,

    /// Passive mode port range
    ///
    /// e.g. 2122-2124
    #[arg(long, env = "FTP_PAPERLESS_BRIDGE_PASSIVE_MODE_PORTS", value_parser = parse_port_range)]
    pub passive_mode_ports: RangeInclusive<u16>,

    /// FTP username
    #[arg(short, long, env = "FTP_PAPERLESS_BRIDGE_USERNAME")]
    pub username: String,

    /// FTP password
    #[arg(short, long, env = "FTP_PAPERLESS_BRIDGE_PASSWORD")]
    pub password: String,

    /// URL to your paperless instance
    ///
    /// e.g. https://paperless.example.com
    #[arg(long, env = "FTP_PAPERLESS_BRIDGE_PAPERLESS_URL")]
    pub paperless_url: String,

    /// Paperless API token
    #[arg(long, env = "FTP_PAPERLESS_BRIDGE_PAPERLESS_API_TOKEN")]
    pub paperless_api_token: String,

    /// Spool directory for failed uploads (enables spool-to-disk)
    ///
    /// When set, files that fail to upload after retries are saved here
    /// and retried periodically in the background.
    #[arg(long, env = "FTP_PAPERLESS_BRIDGE_SPOOL_DIR")]
    pub spool_dir: Option<PathBuf>,
}

#[tokio::main]
pub async fn main() -> Result<()> {
    color_eyre::install()?;

    let args = CliArgs::parse();

    unsafe {
        if args.verbose {
            env::set_var("RUST_LOG", "debug");
        } else {
            env::set_var("RUST_LOG", "info");
        }
    }
    env_logger::init();

    let paperless_client = Arc::new(PaperlessClient::new(
        &args.paperless_url,
        &args.paperless_api_token,
    ));

    // Validate API connection at startup
    info!("Validating Paperless API connection...");
    if let Err(e) = validate_paperless_connection_with_retry(paperless_client.as_ref()).await {
        error!("Failed to connect to Paperless API: {e}");
        return Err(color_eyre::eyre::eyre!("Failed to connect to Paperless API: {e}"));
    }
    info!("Paperless API connection validated");

    let authenticator = Arc::new(UsernamePasswordAuthenticator::new(
        args.username,
        args.password,
    ));

    let spool_dir = args.spool_dir.clone();

    // Start background spool drain if spool_dir is configured
    if let Some(ref dir) = spool_dir {
        std::fs::create_dir_all(dir)?;
        info!("Spool directory: {}", dir.display());
        let spool_client = Arc::clone(&paperless_client) as Arc<dyn PaperlessApi>;
        let spool_path = dir.clone();
        tokio::spawn(spool::spool_drain_loop(
            spool_path,
            spool_client,
            Duration::from_secs(60),
        ));
    }

    let paperless_storage = Box::new(move || {
        let client = Arc::clone(&paperless_client) as Arc<dyn PaperlessApi>;
        if let Some(ref dir) = spool_dir {
            PaperlessStorage::new_with_spool(client, dir.clone())
        } else {
            PaperlessStorage::new(client)
        }
    });

    info!(
        "Starting FTP server at {} with passive port range {}-{}",
        args.listen,
        args.passive_mode_ports.start(),
        args.passive_mode_ports.end()
    );
    let ftp_server = libunftp::ServerBuilder::with_authenticator(paperless_storage, authenticator)
        .greeting("ftp-paperless-bridge")
        .active_passive_mode(ActivePassiveMode::ActiveAndPassive)
        .passive_ports(args.passive_mode_ports)
        .build()?;

    let server_handle = tokio::spawn(async move {
        if let Err(e) = ftp_server.listen(args.listen).await {
            error!("FTP server error: {}", e);
        }
    });

    tokio::select! {
        _ = server_handle => {
            info!("FTP server stopped");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT (Ctrl+C), shutting down gracefully...");
        }
        _ = async {
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler");
            sigterm.recv().await
        } => {
            info!("Received SIGTERM, shutting down gracefully...");
        }
    }

    info!("Shutdown complete");
    Ok(())
}
