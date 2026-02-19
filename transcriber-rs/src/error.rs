use thiserror::Error;

#[derive(Error, Debug)]
pub enum TranscriberError {
    #[error("No OPENAI_API_KEY found in environment")]
    MissingApiKey,

    #[error("Audio device error: {0}")]
    AudioDevice(String),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Filter config error: {0}")]
    FilterConfig(String),

    #[error("Channel send error")]
    ChannelSend,
}
