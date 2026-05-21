use clap::Parser;

/// Real-time audio transcription with OpenAI API.
///
/// Captures audio from your microphone, transcribes it via OpenAI's Realtime API,
/// and types the result into the focused window.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "transcribe",
    about = "Real-time audio transcription with OpenAI's gpt-realtime-whisper",
    after_help = "\
Transcription uses gpt-realtime-whisper (the only supported model). Local
Silero VAD endpoints utterances since the server rejects turn_detection
for this model.

The --delay flag controls server-side latency/accuracy tradeoff:
  minimal | low | medium | high | xhigh

Higher delay = more audio context per inference = more accurate but laggy.
Default is `high` to avoid dropping sentence-initial words.

Examples:
  transcribe                          # delay=high
  transcribe --delay medium           # Snappier feel
  transcribe --allow-bye-thank-you    # Disable hallucination filtering"
)]
pub struct Config {
    /// Latency/accuracy tradeoff for gpt-realtime-whisper (higher = more accurate, more lag).
    #[arg(long, default_value = "high",
           value_parser = ["minimal", "low", "medium", "high", "xhigh"])]
    pub delay: String,

    /// Disable hallucination filtering (false positives, YouTube outros, etc.)
    #[arg(long)]
    pub allow_bye_thank_you: bool,

    /// Allow non-ASCII characters in transcription
    #[arg(long)]
    pub allow_non_ascii: bool,

    /// Don't filter out filler words (um, uh, hmm, etc.)
    #[arg(long)]
    pub allow_fillers: bool,

    /// Don't save transcriptions to conversations/ directory
    #[arg(long)]
    pub no_log: bool,
}

impl Config {
    /// Load the OpenAI API key from the environment.
    /// Returns None if OPENAI_API_KEY is not set.
    pub fn api_key() -> Option<String> {
        std::env::var("OPENAI_API_KEY").ok().filter(|k| !k.is_empty())
    }
}
