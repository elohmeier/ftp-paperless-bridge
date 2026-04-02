use async_trait::async_trait;
use log::info;
use reqwest::{Client, multipart};
use serde::Deserialize;

#[derive(Debug)]
pub enum PaperlessError {
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

#[derive(Deserialize, Debug)]
pub struct TaskStatus {
    pub status: String,
}

#[async_trait]
pub trait PaperlessApi: Send + Sync {
    async fn health_check(&self) -> Result<(), PaperlessError>;
    async fn upload(&self, path: &str) -> Result<String, PaperlessError>;
    async fn task_status(&self, task_id: &str) -> Result<TaskStatus, PaperlessError>;
}

#[derive(Clone)]
pub struct PaperlessClient {
    base_url: String,
    token: String,
    client: Client,
}

impl PaperlessClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client: Client::new(),
        }
    }
}

#[async_trait]
impl PaperlessApi for PaperlessClient {
    async fn health_check(&self) -> Result<(), PaperlessError> {
        self.client
            .get(format!("{}/api/ui_settings/", self.base_url))
            .header("Authorization", format!("Token {}", self.token))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

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
