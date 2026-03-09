use thiserror::Error;

#[derive(Error, Debug)]
pub enum TranscriberError {
    #[error("No OPENAI_API_KEY found in environment")]
    MissingApiKey,

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}
