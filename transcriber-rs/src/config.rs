use clap::Parser;

/// Real-time audio transcription with OpenAI API.
///
/// Captures audio from your microphone, transcribes it via OpenAI's Realtime API,
/// and types the result into the focused window.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "transcribe",
    about = "Real-time audio transcription with OpenAI API",
    after_help = "\
Available models (GA Realtime API):
  gpt-realtime-whisper    Whisper streaming with local VAD + manual commits (default)
  gpt-4o-transcribe       GPT-4o transcription (server VAD, no commits needed)
  gpt-4o-mini-transcribe  GPT-4o mini transcription (cheapest)

Local VAD endpoints whisper utterances since the server rejects turn_detection
for that model. delay only applies to whisper; ignored for other models.

Examples:
  transcribe                              # gpt-realtime-whisper, delay=high
  transcribe --delay medium               # Snappier feel, may drop sentence-initial words
  transcribe --model gpt-4o-transcribe    # Old behavior (server VAD)
  transcribe --allow-bye-thank-you        # Disable hallucination filtering"
)]
pub struct Config {
    /// Transcription model to use
    #[arg(short = 'm', long, default_value = "gpt-realtime-whisper",
           value_parser = ["gpt-realtime-whisper", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"])]
    pub model: String,

    /// Latency/accuracy tradeoff for gpt-realtime-whisper (higher = more accurate, more lag).
    /// "high" default: prioritizes not dropping sentence-initial words over snappy feel.
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
