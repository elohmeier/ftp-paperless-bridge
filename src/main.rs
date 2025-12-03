use std::env;
use std::fmt::Debug;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_tempfile::TempFile;
use async_trait::async_trait;
use clap::Parser;
use color_eyre::eyre::Result;
use libunftp::options::ActivePassiveMode;
use libunftp::{
    auth::{AuthenticationError, Authenticator, Credentials, UserDetail},
    storage::{
        Error as StorageError, ErrorKind::LocalError, Fileinfo, Metadata, Result as StorageResult,
        StorageBackend,
    },
};
use log::{debug, error, info, warn};
use reqwest::{Client, multipart};
use serde::Deserialize;
use tokio::io::AsyncSeekExt;
use tokio::time::{Instant, sleep};

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
    // Try to parse as SocketAddr to validate format
    if addr.parse::<std::net::SocketAddr>().is_ok() {
        Ok(addr.to_string())
    } else {
        Err(format!(
            "Invalid listen address '{}'. Must be in format IP:PORT (e.g., 0.0.0.0:2121 or [::]:2121)",
            addr
        ))
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
}

#[derive(Debug)]
enum PaperlessError {
    Reqwest(reqwest::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for PaperlessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaperlessError::Reqwest(e) => write!(f, "{e}"),
            PaperlessError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PaperlessError {}

impl From<reqwest::Error> for PaperlessError {
    fn from(e: reqwest::Error) -> Self {
        PaperlessError::Reqwest(e)
    }
}

impl From<std::io::Error> for PaperlessError {
    fn from(e: std::io::Error) -> Self {
        PaperlessError::Io(e)
    }
}

#[derive(Clone)]
struct PaperlessClient {
    base_url: String,
    token: String,
    client: Client,
}

#[derive(Deserialize, Debug)]
struct TaskStatus {
    pub status: String,
}

impl PaperlessClient {
    fn new(base_url: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client: Client::new(),
        }
    }

    /// Validate API token by hitting /api/ui_settings/
    async fn health_check(&self) -> Result<(), PaperlessError> {
        self.client
            .get(format!("{}/api/ui_settings/", self.base_url))
            .header("Authorization", format!("Token {}", self.token))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Upload document, returns task UUID
    async fn upload(&self, path: &str) -> Result<String, PaperlessError> {
        info!("Uploading {path:?}");
        let form = multipart::Form::new().file("document", path).await?;

        let resp = self
            .client
            .post(format!("{}/api/documents/post_document/", self.base_url))
            .header("Authorization", format!("Token {}", self.token))
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;

        let uuid = resp.text().await?;
        Ok(uuid.trim_matches('"').to_string())
    }

    /// Poll task status
    async fn task_status(&self, task_id: &str) -> Result<TaskStatus, PaperlessError> {
        let resp: Vec<TaskStatus> = self
            .client
            .get(format!("{}/api/tasks/?task_id={}", self.base_url, task_id))
            .header("Authorization", format!("Token {}", self.token))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.into_iter().next().unwrap_or(TaskStatus {
            status: "PENDING".to_string(),
        }))
    }
}

struct PaperlessStorage {
    paperless_client: Arc<PaperlessClient>,
}

impl std::fmt::Debug for PaperlessStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "opaque")
    }
}

impl PaperlessStorage {
    pub fn new(paperless_client: Arc<PaperlessClient>) -> Self {
        Self { paperless_client }
    }
}

#[derive(Debug)]
struct Meta;

impl Metadata for Meta {
    fn len(&self) -> u64 {
        0
    }

    fn is_dir(&self) -> bool {
        true
    }

    fn is_file(&self) -> bool {
        false
    }

    fn is_symlink(&self) -> bool {
        false
    }

    fn modified(&self) -> StorageResult<std::time::SystemTime> {
        Ok(std::time::SystemTime::now())
    }

    fn gid(&self) -> u32 {
        0
    }

    fn uid(&self) -> u32 {
        0
    }
}

#[async_trait]
impl StorageBackend<User> for PaperlessStorage {
    type Metadata = Meta;

    async fn metadata<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        path: P,
    ) -> StorageResult<Self::Metadata> {
        debug!("METADATA called for path: {:?}", path.as_ref());
        // Return a basic metadata implementation for the root directory
        Ok(Meta)
    }

    async fn list<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        path: P,
    ) -> StorageResult<Vec<Fileinfo<PathBuf, Self::Metadata>>>
    where
        <Self as StorageBackend<User>>::Metadata: Metadata,
    {
        debug!("LIST called for path: {:?}", path.as_ref());
        // Return an empty directory listing since this is an upload-only bridge
        Ok(vec![])
    }

    async fn get<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        _path: P,
        _start_pos: u64,
    ) -> StorageResult<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>> {
        unimplemented!()
    }

    async fn put<
        P: AsRef<Path> + Send + Debug,
        R: tokio::io::AsyncRead + Send + Sync + Unpin + 'static,
    >(
        &self,
        _user: &User,
        input: R,
        path: P,
        start_pos: u64,
    ) -> StorageResult<u64> {
        info!("Received upload request");

        // First we'll write the provided file to a temporary location.
        let mut tempfile =
            if let Some(file_name) = path.as_ref().file_name().map(|x| x.to_string_lossy()) {
                TempFile::new_with_name(file_name).await.unwrap()
            } else {
                TempFile::new().await.unwrap()
            };
        let path = tempfile.file_path().to_str().unwrap().to_owned();
        debug!("Saving upload to {path}");

        tempfile.set_len(start_pos).await.unwrap();
        tempfile
            .seek(std::io::SeekFrom::Start(start_pos))
            .await
            .unwrap();

        let mut reader = tokio::io::BufReader::with_capacity(4096, input);
        let mut writer = tokio::io::BufWriter::with_capacity(4096, tempfile);
        let bytes_copied = tokio::io::copy(&mut reader, &mut writer).await?;

        // Now we'll upload the file.
        //
        // The upload returns immediately and gives us a task UUID that we'll have to poll.
        let task_id = match self.paperless_client.upload(&path).await {
            Ok(id) => id,
            Err(e) => {
                error!("Upload failed: {e}");
                return Err(StorageError::new(LocalError, e));
            }
        };

        let now = Instant::now();
        loop {
            sleep(Duration::from_secs(1)).await;

            let status = match self.paperless_client.task_status(&task_id).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to get task status: {e}");
                    if now.elapsed() > Duration::from_secs(10) {
                        error!("Timeout getting upload status: {e}");
                        return Err(StorageError::new(LocalError, e));
                    }
                    continue;
                }
            };

            debug!("Task status: {status:?}");

            match status.status.as_str() {
                "SUCCESS" => {
                    info!("File uploaded successfully");
                    break;
                }
                "FAILURE" | "REVOKED" => {
                    error!("Upload failed: {}", status.status);
                    return Err(StorageError::new(
                        LocalError,
                        std::io::Error::other("Upload task failed"),
                    ));
                }
                _ => {} // PENDING, STARTED - continue polling
            }

            if now.elapsed() > Duration::from_secs(10) {
                error!("Timeout waiting for upload");
                return Err(StorageError::new(
                    LocalError,
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "Upload timeout"),
                ));
            }
        }

        Ok(bytes_copied)
    }

    async fn del<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        _path: P,
    ) -> StorageResult<()> {
        unimplemented!()
    }

    async fn mkd<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        _path: P,
    ) -> StorageResult<()> {
        unimplemented!()
    }

    async fn rename<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        _from: P,
        _to: P,
    ) -> StorageResult<()> {
        unimplemented!()
    }

    async fn rmd<P: AsRef<Path> + Send + Debug>(
        &self,
        _user: &User,
        _path: P,
    ) -> StorageResult<()> {
        unimplemented!()
    }

    async fn cwd<P: AsRef<Path> + Send + Debug>(&self, _user: &User, path: P) -> StorageResult<()> {
        debug!("CWD called for path: {:?}", path.as_ref());
        // Always succeed for directory changes since we don't have a real filesystem
        Ok(())
    }
}

#[derive(Debug)]
struct UsernamePasswordAuthenticator {
    username: String,
    password: String,
}

impl UsernamePasswordAuthenticator {
    fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

#[async_trait]

impl Authenticator<User> for UsernamePasswordAuthenticator {
    async fn authenticate(
        &self,
        username: &str,
        creds: &Credentials,
    ) -> Result<User, AuthenticationError> {
        if let Some(ref password) = creds.password
            && *password != self.password
        {
            warn!("Provided password doesn't match");
            return Err(AuthenticationError::BadPassword);
        }
        if username != self.username {
            warn!("Provided username doesn't match");
            return Err(AuthenticationError::BadUser);
        }
        info!("Successfully authenticated");
        Ok(User {})
    }
}

#[derive(Debug)]
struct User;

impl UserDetail for User {}

impl std::fmt::Display for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "User")
    }
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
    if let Err(e) = paperless_client.health_check().await {
        error!("Failed to connect to Paperless API: {e}");
        return Err(e.into());
    }
    info!("Paperless API connection validated");

    let authenticator = Arc::new(UsernamePasswordAuthenticator::new(
        args.username,
        args.password,
    ));

    let paperless_storage = Box::new(move || PaperlessStorage::new(Arc::clone(&paperless_client)));

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

    // Set up graceful shutdown handling
    let server_handle = tokio::spawn(async move {
        if let Err(e) = ftp_server.listen(args.listen).await {
            error!("FTP server error: {}", e);
        }
    });

    // Wait for shutdown signal
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
