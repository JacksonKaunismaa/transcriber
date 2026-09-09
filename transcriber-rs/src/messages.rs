/// Channel message types for the actor architecture.
///
/// Each task owns its state and communicates via typed mpsc channels.
/// This replaces the Python version's shared mutable state + threading.Lock.

/// Raw PCM audio from cpal callback → Audio Router.
pub struct AudioChunk {
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
    /// `utterance_duration_ms` is the length of audio being committed — the
    /// whole server buffer since the last commit (pre-speech prefix + speech +
    /// trailing silence). Used by the transcript task's wps-based hallucination
    /// filter; it must be the audio the transcript actually covers, not a
    /// speech-only estimate (which understates on soft onsets and dropped real
    /// speech — see audio_buffer.rs).
    Commit { utterance_duration_ms: Option<u64> },
}

/// Events from WebSocket task → Audio Router.
pub enum AudioEvent {
    /// New WebSocket session — API timestamps reset to 0
    SessionReset,
    /// User hit the audio-dump hotkey (SIGHUP). Write the rolling
    /// fallback buffer to a timestamped WAV file under conversations/audio_dumps/.
    DumpAudio,
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
        /// Speech duration in ms, measured by local VAD (excludes pre-speech
    /// prefix and post-speech silence — see WsCommand::Commit docs). Used
    /// by the wps-based hallucination filter.
    duration_ms: Option<u64>,
    },
    /// Partial transcription delta from the Realtime API
    RealtimeDelta { delta: String },
    /// Parked: completed transcription from whisper-1 REST fallback. See
    /// INCIDENTS.md for the monitoring plan that may revive this.
    #[allow(dead_code)]
    FallbackCompleted {
        item_id: String,
        transcript: String,
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
    /// Parked 2026-07-18: was emitted per audio chunk but never tracked.
    #[allow(dead_code)]
    AudioChunkSent,
    /// Gauge: audio duration waiting in the router's local send queue
    /// because the websocket can't keep up (network backpressure).
    SendBacklog { ms: u64 },
    /// Router send-queue overflow — audio dropped past the backlog cap.
    AudioDropped { chunks: u64, ms: u64, commits: u64 },
    /// cpal capture callback couldn't hand off a chunk (audio channel full).
    CaptureDropped { chunks: u64 },
    RealtimeTranscription,
    // Parked fallback variants — kept so the whisper-1 REST fallback can be
    // revived without re-defining the enum. See INCIDENTS.md.
    #[allow(dead_code)]
    Timeout,
    #[allow(dead_code)]
    FallbackSuccess,
    #[allow(dead_code)]
    FallbackFailure {
        duration_ms: u64,
    },
    #[allow(dead_code)]
    FallbackRace,
    #[allow(dead_code)]
    ShortSegmentSkipped,
    // Temporarily unused (2026-06-07) — fuzzy dedup disabled in transcript.rs.
    #[allow(dead_code)]
    DuplicateFiltered,
    ContentFiltered,
    WebSocketError,
    ApiError,
    PingRtt { millis: u64 },
    TranscriptionRtt { millis: u64 },
}

