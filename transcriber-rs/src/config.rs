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
Available models (GA Realtime API — whisper-1 retired May 2026):
  gpt-4o-transcribe       GPT-4o transcription (default, high quality)
  gpt-4o-mini-transcribe  GPT-4o mini transcription (faster, lower cost)
  gpt-realtime-whisper    New Whisper successor in Realtime API

Examples:
  transcribe                          # Use default gpt-4o-transcribe
  transcribe --model gpt-realtime-whisper
  transcribe --allow-bye-thank-you    # Disable hallucination filtering
  transcribe --allow-non-ascii        # Allow non-ASCII characters"
)]
pub struct Config {
    /// Transcription model to use
    #[arg(short = 'm', long, default_value = "gpt-4o-transcribe",
           value_parser = ["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "gpt-realtime-whisper"])]
    pub model: String,

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
