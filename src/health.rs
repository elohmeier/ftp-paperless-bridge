use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use log::{info, warn};
use tokio::time::{MissedTickBehavior, interval};

use crate::paperless::PaperlessApi;

#[derive(Clone, Debug)]
pub struct PaperlessHealth {
    inner: Arc<RwLock<HealthSnapshot>>,
    max_age: Duration,
}

#[derive(Debug)]
struct HealthSnapshot {
    healthy: bool,
    checked_at: Instant,
    error: Option<String>,
}

#[derive(Debug)]
pub struct PaperlessUnavailable(String);

impl fmt::Display for PaperlessUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PaperlessUnavailable {}

impl PaperlessHealth {
    pub fn new_healthy(max_age: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HealthSnapshot {
                healthy: true,
                checked_at: Instant::now(),
                error: None,
            })),
            max_age,
        }
    }

    pub fn check(&self) -> Result<(), PaperlessUnavailable> {
        let snapshot = self.inner.read().expect("Paperless health lock poisoned");
        let age = snapshot.checked_at.elapsed();

        if age > self.max_age {
            return Err(PaperlessUnavailable(format!(
                "Paperless health status is stale (last checked {}s ago)",
                age.as_secs()
            )));
        }

        if snapshot.healthy {
            Ok(())
        } else {
            Err(PaperlessUnavailable(
                snapshot
                    .error
                    .clone()
                    .unwrap_or_else(|| "Paperless is unavailable".to_string()),
            ))
        }
    }

    pub fn mark_healthy(&self) -> bool {
        let mut snapshot = self.inner.write().expect("Paperless health lock poisoned");
        let changed = !snapshot.healthy;
        snapshot.healthy = true;
        snapshot.checked_at = Instant::now();
        snapshot.error = None;
        changed
    }

    pub fn mark_unhealthy(&self, error: impl fmt::Display) -> bool {
        let mut snapshot = self.inner.write().expect("Paperless health lock poisoned");
        let changed = snapshot.healthy;
        snapshot.healthy = false;
        snapshot.checked_at = Instant::now();
        snapshot.error = Some(error.to_string());
        changed
    }
}

pub async fn monitor_paperless_health(
    client: Arc<dyn PaperlessApi>,
    health: PaperlessHealth,
    check_interval: Duration,
) {
    let mut ticker = interval(check_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        match client.health_check().await {
            Ok(()) => {
                if health.mark_healthy() {
                    info!("Paperless API is available again; FTP logins are enabled");
                }
            }
            Err(error) => {
                if health.mark_unhealthy(&error) {
                    warn!("Paperless API became unavailable; FTP logins are disabled: {error}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unhealthy_status_blocks_access_until_recovery() {
        let health = PaperlessHealth::new_healthy(Duration::from_secs(60));
        assert!(health.check().is_ok());

        health.mark_unhealthy("dns failure");
        assert_eq!(health.check().unwrap_err().to_string(), "dns failure");

        health.mark_healthy();
        assert!(health.check().is_ok());
    }

    #[test]
    fn stale_status_fails_closed() {
        let health = PaperlessHealth::new_healthy(Duration::ZERO);
        assert!(health.check().is_err());
    }
}
