/// Channel message types for the actor architecture.
///
/// Each task owns its state and communicates via typed mpsc channels.
/// This replaces the Python version's shared mutable state + threading.Lock.

/// Raw PCM audio from cpal callback → Audio Router.
pub struct AudioChunk {
    /// Milliseconds since session start
    pub timestamp_ms: u64,
    /// Raw i16 PCM samples (1024 frames, 24kHz, mono)
    pub data: Vec<i16>,
}

/// Commands from Audio Router → WebSocket task.
pub enum WsCommand {
    /// Send base64-encoded audio to the Realtime API
    SendAudio { audio_b64: String },
    /// Tell the server to finalize the current input buffer and run transcription.
    /// Triggered by local VAD when end-of-utterance is detected. Required because
    /// gpt-realtime-whisper rejects server-side VAD, so the client must endpoint.
    Commit,
}

/// Events from WebSocket task → Audio Router.
pub enum AudioEvent {
    /// VAD detected speech start
    SpeechStarted {
        item_id: String,
        audio_start_ms: u64,
    },
    /// VAD detected speech stop
    SpeechStopped {
        item_id: String,
        audio_end_ms: u64,
    },
    /// Realtime API delivered transcription — cancel pending fallback
    ItemCompleted {
        item_id: String,
    },
    /// New WebSocket session — API timestamps reset to 0
    SessionReset,
}

/// Events from WebSocket / Audio Router → Transcript Manager.
#[derive(Debug)]
pub enum TranscriptEvent {
    /// A new conversation item was created (for ordering)
    ItemCreated { item_id: String },
    /// Completed transcription from the Realtime API
    RealtimeCompleted {
        item_id: String,
        transcript: String,
        /// Speech duration in milliseconds (from VAD: audio_end_ms - audio_start_ms)
        duration_ms: Option<u64>,
    },
    /// Partial transcription delta from the Realtime API
    RealtimeDelta { delta: String },
    /// Completed transcription from Whisper fallback
    FallbackCompleted {
        item_id: String,
        transcript: String,
        /// Speech duration in milliseconds (from VAD: audio_end_ms - audio_start_ms)
        duration_ms: Option<u64>,
    },
    /// New WebSocket session — clear ordering/completion state
    SessionReset,
}

/// Commands from Transcript Manager → Typer task.
pub struct TypeCommand {
    /// The filtered text to type
    pub text: String,
}

/// Events from any task → Metrics task.
pub enum MetricsEvent {
    ConnectionAttempt,
    ConnectionSuccess,
    SessionExpiration,
    ReconnectionAttempt,
    AudioChunkSent,
    RealtimeTranscription,
    Timeout,
    FallbackSuccess,
    FallbackFailure { duration_ms: u64 },
    FallbackRace,
    ShortSegmentSkipped,
    DuplicateFiltered,
    ContentFiltered,
    WebSocketError,
    ApiError,
    PingRtt { millis: u64 },
    TranscriptionRtt { millis: u64 },
}

