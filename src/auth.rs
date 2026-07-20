use async_trait::async_trait;
use libunftp::auth::{AuthenticationError, Authenticator, Credentials, UserDetail};
use log::{info, warn};

use crate::health::PaperlessHealth;

#[derive(Debug)]
pub struct User;

impl UserDetail for User {}

impl std::fmt::Display for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "User")
    }
}

#[derive(Debug)]
pub struct UsernamePasswordAuthenticator {
    username: String,
    password: String,
    paperless_health: PaperlessHealth,
}

impl UsernamePasswordAuthenticator {
    pub fn new(username: String, password: String, paperless_health: PaperlessHealth) -> Self {
        Self {
            username,
            password,
            paperless_health,
        }
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
        if let Err(error) = self.paperless_health.check() {
            warn!("Rejecting FTP login because Paperless is unavailable: {error}");
            return Err(AuthenticationError::new("Paperless is unavailable"));
        }
        info!("Successfully authenticated");
        Ok(User {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn rejects_valid_credentials_when_paperless_is_unavailable() {
        let health = PaperlessHealth::new_healthy(Duration::from_secs(60));
        health.mark_unhealthy("dns failure");
        let authenticator =
            UsernamePasswordAuthenticator::new("scanner".to_string(), "secret".to_string(), health);

        assert!(
            authenticator
                .authenticate("scanner", &"secret".into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn accepts_valid_credentials_when_paperless_is_healthy() {
        let authenticator = UsernamePasswordAuthenticator::new(
            "scanner".to_string(),
            "secret".to_string(),
            PaperlessHealth::new_healthy(Duration::from_secs(60)),
        );

        assert!(
            authenticator
                .authenticate("scanner", &"secret".into())
                .await
                .is_ok()
        );
    }
}
