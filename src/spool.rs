use std::path::{Path, PathBuf};
use std::time::Duration;

use log::{debug, error, info, warn};
use tokio::time::sleep;

use crate::paperless::{PaperlessApi, PaperlessError};

/// Move a file into the spool directory, preserving the original filename.
pub async fn spool_file(source: &Path, spool_dir: &Path) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(spool_dir)?;

    let file_name = source
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"));
    let dest = spool_dir.join(file_name);

    // If a file with the same name exists, add a suffix
    let dest = if dest.exists() {
        let stem = dest
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = dest
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        spool_dir.join(format!("{stem}_{timestamp}{ext}"))
    } else {
        dest
    };

    tokio::fs::copy(source, &dest).await?;
    info!("Spooled file to {}", dest.display());
    Ok(dest)
}

/// Try to upload a single file, returning Ok if it succeeds.
async fn try_upload_file(
    path: &Path,
    client: &dyn PaperlessApi,
) -> Result<(), PaperlessError> {
    let path_str = path
        .to_str()
        .ok_or_else(|| PaperlessError::Io(std::io::Error::other("invalid path")))?;

    client.upload(path_str).await?;
    info!("Spooled file uploaded successfully: {}", path.display());
    Ok(())
}

/// Drain the spool directory by uploading all files. Successfully uploaded files are removed.
pub async fn drain_spool(
    spool_dir: &Path,
    client: &dyn PaperlessApi,
) -> Result<(), std::io::Error> {
    let entries: Vec<_> = std::fs::read_dir(spool_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    for entry in entries {
        let path = entry.path();
        debug!("Attempting to upload spooled file: {}", path.display());

        match try_upload_file(&path, client).await {
            Ok(()) => {
                std::fs::remove_file(&path)?;
                info!("Removed spooled file after successful upload: {}", path.display());
            }
            Err(e) => {
                warn!(
                    "Failed to upload spooled file {}: {e}, will retry later",
                    path.display()
                );
            }
        }
    }

    Ok(())
}

/// Background task that periodically drains the spool directory.
pub async fn spool_drain_loop(
    spool_dir: PathBuf,
    client: std::sync::Arc<dyn PaperlessApi>,
    interval: Duration,
) {
    loop {
        sleep(interval).await;

        let files_exist = std::fs::read_dir(&spool_dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);

        if files_exist {
            info!("Checking spool directory for pending uploads...");
            if let Err(e) = drain_spool(&spool_dir, client.as_ref()).await {
                error!("Error draining spool: {e}");
            }
        }
    }
}
