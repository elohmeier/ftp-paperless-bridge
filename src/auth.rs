use async_trait::async_trait;
use libunftp::auth::{AuthenticationError, Authenticator, Credentials, UserDetail};
use log::{info, warn};

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
}

impl UsernamePasswordAuthenticator {
    pub fn new(username: String, password: String) -> Self {
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
