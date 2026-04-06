use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_tempfile::TempFile;
use async_trait::async_trait;
use libunftp::storage::{
    Error as StorageError, ErrorKind::LocalError, Fileinfo, Metadata, Result as StorageResult,
    StorageBackend,
};
use log::{debug, error, info, warn};
use tokio::io::AsyncSeekExt;
use tokio::time::sleep;

use crate::auth::User;
use crate::paperless::PaperlessApi;

const MAX_UPLOAD_RETRIES: usize = 5;
const INITIAL_RETRY_DELAY_MS: u64 = 500;

pub struct PaperlessStorage {
    paperless_client: Arc<dyn PaperlessApi>,
    spool_dir: Option<PathBuf>,
}

impl std::fmt::Debug for PaperlessStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "opaque")
    }
}

impl PaperlessStorage {
    pub fn new(paperless_client: Arc<dyn PaperlessApi>) -> Self {
        Self {
            paperless_client,
            spool_dir: None,
        }
    }

    pub fn new_with_spool(paperless_client: Arc<dyn PaperlessApi>, spool_dir: PathBuf) -> Self {
        Self {
            paperless_client,
            spool_dir: Some(spool_dir),
        }
    }

    async fn handle_upload_failure(
        &self,
        temp_path: &str,
        err: crate::paperless::PaperlessError,
        bytes_copied: u64,
    ) -> StorageResult<u64> {
        if let Some(ref spool_dir) = self.spool_dir {
            match crate::spool::spool_file(Path::new(temp_path), spool_dir).await {
                Ok(spool_path) => {
                    info!(
                        "File spooled for later retry: {}",
                        spool_path.display()
                    );
                    return Ok(bytes_copied);
                }
                Err(spool_err) => {
                    error!("Failed to spool file: {spool_err}");
                }
            }
        }
        Err(StorageError::new(LocalError, err))
    }
}

#[derive(Debug)]
pub struct Meta;

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

        // Save to temp file first
        let mut tempfile =
            if let Some(file_name) = path.as_ref().file_name().map(|x| x.to_string_lossy()) {
                TempFile::new_with_name(file_name).await.unwrap()
            } else {
                TempFile::new().await.unwrap()
            };
        let temp_path = tempfile.file_path().to_str().unwrap().to_owned();
        debug!("Saving upload to {temp_path}");

        tempfile.set_len(start_pos).await.unwrap();
        tempfile
            .seek(std::io::SeekFrom::Start(start_pos))
            .await
            .unwrap();

        let mut reader = tokio::io::BufReader::with_capacity(4096, input);
        let mut writer = tokio::io::BufWriter::with_capacity(4096, tempfile);
        let bytes_copied = tokio::io::copy(&mut reader, &mut writer).await?;
        // Flush to ensure all data is written before we might spool the file
        tokio::io::AsyncWriteExt::flush(&mut writer).await?;

        // Pre-upload health check
        if let Err(e) = self.paperless_client.health_check().await {
            warn!("Pre-upload health check failed: {e}");
            return self.handle_upload_failure(&temp_path, e, bytes_copied).await;
        }

        // Upload with retry
        let mut last_err = None;
        for attempt in 0..MAX_UPLOAD_RETRIES {
            match self.paperless_client.upload(&temp_path).await {
                Ok(_task_id) => {
                    info!("File uploaded successfully");
                    return Ok(bytes_copied);
                }
                Err(e) => {
                    warn!("Upload attempt {} failed: {e}", attempt + 1);
                    last_err = Some(e);
                }
            }

            if attempt + 1 < MAX_UPLOAD_RETRIES {
                let delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt as u32));
                debug!("Retrying in {}ms", delay.as_millis());
                sleep(delay).await;
            }
        }

        let err = last_err.unwrap();
        error!("Upload failed after {MAX_UPLOAD_RETRIES} attempts: {err}");
        self.handle_upload_failure(&temp_path, err, bytes_copied).await
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paperless::PaperlessError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock that fails N times then succeeds on upload, always succeeds on health_check
    struct RetryMockClient {
        fail_count: AtomicUsize,
        failures_remaining: AtomicUsize,
    }

    impl RetryMockClient {
        fn new(fail_n_times: usize) -> Self {
            Self {
                fail_count: AtomicUsize::new(0),
                failures_remaining: AtomicUsize::new(fail_n_times),
            }
        }
    }

    #[async_trait]
    impl PaperlessApi for RetryMockClient {
        async fn health_check(&self) -> Result<(), PaperlessError> {
            Ok(())
        }

        async fn upload(&self, _path: &str) -> Result<String, PaperlessError> {
            self.fail_count.fetch_add(1, Ordering::SeqCst);
            let remaining = self.failures_remaining.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                Err(PaperlessError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "dns error: Name does not resolve",
                )))
            } else {
                Ok("test-task-id".to_string())
            }
        }

    }

    /// Mock that always fails upload (for spool testing)
    struct AlwaysFailClient;

    #[async_trait]
    impl PaperlessApi for AlwaysFailClient {
        async fn health_check(&self) -> Result<(), PaperlessError> {
            Err(PaperlessError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "dns error: Name does not resolve",
            )))
        }

        async fn upload(&self, _path: &str) -> Result<String, PaperlessError> {
            Err(PaperlessError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "dns error: Name does not resolve",
            )))
        }

    }

    /// Mock that tracks health_check calls, fails health_check but would succeed upload
    struct HealthCheckTrackingClient {
        health_check_count: AtomicUsize,
        health_check_fails: bool,
    }

    impl HealthCheckTrackingClient {
        fn new(health_check_fails: bool) -> Self {
            Self {
                health_check_count: AtomicUsize::new(0),
                health_check_fails,
            }
        }
    }

    #[async_trait]
    impl PaperlessApi for HealthCheckTrackingClient {
        async fn health_check(&self) -> Result<(), PaperlessError> {
            self.health_check_count.fetch_add(1, Ordering::SeqCst);
            if self.health_check_fails {
                Err(PaperlessError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "dns error",
                )))
            } else {
                Ok(())
            }
        }

        async fn upload(&self, _path: &str) -> Result<String, PaperlessError> {
            Ok("test-task-id".to_string())
        }

    }

    fn make_input(data: &[u8]) -> impl tokio::io::AsyncRead + Send + Sync + Unpin + 'static {
        tokio::io::BufReader::new(std::io::Cursor::new(data.to_vec()))
    }

    // === Feature 1: Retry with backoff ===

    #[tokio::test]
    async fn test_upload_retries_on_transient_error_then_succeeds() {
        // Upload fails twice then succeeds on third attempt
        let client = Arc::new(RetryMockClient::new(2));
        let storage = PaperlessStorage::new(client.clone());

        let input = make_input(b"test pdf content");
        let result = storage
            .put(&User, input, Path::new("/test.pdf"), 0)
            .await;

        assert!(result.is_ok(), "Upload should succeed after retries");
        // Should have been called 3 times (2 failures + 1 success)
        assert_eq!(client.fail_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_upload_gives_up_after_max_retries() {
        // Upload always fails - should give up after max retries, not retry forever
        let client = Arc::new(RetryMockClient::new(100));
        let storage = PaperlessStorage::new(client.clone());

        let input = make_input(b"test pdf content");
        let result = storage
            .put(&User, input, Path::new("/test.pdf"), 0)
            .await;

        assert!(result.is_err(), "Upload should fail after max retries");
        let attempts = client.fail_count.load(Ordering::SeqCst);
        // Should have retried a bounded number of times (e.g. 3-5), not 100
        assert!(
            attempts <= 6,
            "Should not retry more than ~5 times, but attempted {attempts}"
        );
    }

    // === Feature 2: Pre-upload health check ===

    #[tokio::test]
    async fn test_health_check_called_before_upload() {
        // Health check passes - upload should proceed
        let client = Arc::new(HealthCheckTrackingClient::new(false));
        let storage = PaperlessStorage::new(client.clone());

        let input = make_input(b"test pdf content");
        let result = storage
            .put(&User, input, Path::new("/test.pdf"), 0)
            .await;

        assert!(result.is_ok());
        assert!(
            client.health_check_count.load(Ordering::SeqCst) >= 1,
            "health_check should be called before upload"
        );
    }

    #[tokio::test]
    async fn test_upload_rejected_when_health_check_fails() {
        // Health check fails - upload should be rejected early without attempting upload
        let client = Arc::new(HealthCheckTrackingClient::new(true));
        let storage = PaperlessStorage::new(client.clone());

        let input = make_input(b"test pdf content");
        let result = storage
            .put(&User, input, Path::new("/test.pdf"), 0)
            .await;

        // Should fail because health check failed (after retries)
        assert!(result.is_err(), "Upload should be rejected when health check fails");
    }

    // === Feature 3: Spool to disk on failure ===

    #[tokio::test]
    async fn test_file_spooled_to_disk_on_upload_failure() {
        let spool_dir = tempfile::tempdir().unwrap();
        let client = Arc::new(AlwaysFailClient);
        let storage = PaperlessStorage::new_with_spool(client, spool_dir.path().to_path_buf());

        let input = make_input(b"test pdf content");
        // put should succeed (from FTP client's perspective) because file is spooled
        let result = storage
            .put(&User, input, Path::new("/spool_test_1.pdf"), 0)
            .await;

        assert!(
            result.is_ok(),
            "put should succeed when file is spooled to disk, got: {:?}",
            result
        );

        // Verify the file was saved to the spool directory
        let spool_files: Vec<_> = std::fs::read_dir(spool_dir.path())
            .unwrap()
            .collect();
        assert_eq!(
            spool_files.len(),
            1,
            "Exactly one file should be spooled to disk"
        );
    }

    #[tokio::test]
    async fn test_spooled_file_retried_when_api_recovers() {
        let spool_dir = tempfile::tempdir().unwrap();

        // First, spool a file with a failing client
        let client = Arc::new(AlwaysFailClient);
        let storage = PaperlessStorage::new_with_spool(client, spool_dir.path().to_path_buf());

        let input = make_input(b"test pdf content");
        storage
            .put(&User, input, Path::new("/spool_test_2.pdf"), 0)
            .await
            .unwrap();

        // Verify file is in spool
        let spool_files: Vec<_> = std::fs::read_dir(spool_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(spool_files.len(), 1);

        // Now create a working client and run the spool drain
        let working_client: Arc<dyn PaperlessApi> = Arc::new(RetryMockClient::new(0));
        crate::spool::drain_spool(&spool_dir.path().to_path_buf(), working_client.as_ref())
            .await
            .unwrap();

        // Spool directory should be empty after successful drain
        let remaining: Vec<_> = std::fs::read_dir(spool_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(
            remaining.len(),
            0,
            "Spool directory should be empty after drain"
        );
    }
}
