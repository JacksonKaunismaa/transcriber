use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::config::Config;
use crate::filters::Filters;
use crate::messages::{MetricsEvent, TranscriptEvent, TypeCommand};

/// State for the Transcript Manager task.
struct TranscriptState {
    /// Filters loaded from filters.yaml
    filters: Filters,
    /// Item IDs in creation order
    item_order: Vec<String>,
    /// Completed transcripts waiting for earlier items: (transcript, duration_ms)
    completed_transcripts: HashMap<String, (String, Option<u64>)>,
    /// Index of next item to output
    next_output_index: usize,
    /// Recent transcripts for fuzzy duplicate detection: (timestamp_secs, text)
    recent_transcripts: Vec<(f64, String)>,
    /// Partial transcript buffer (display only)
    transcript_buffer: String,
    /// Set of completed item IDs (for race prevention)
    completed_items: std::collections::HashSet<String>,
    /// Config flags
    allow_bye_thank_you: bool,
    allow_non_ascii: bool,
    allow_fillers: bool,
    /// Channels
    type_tx: mpsc::Sender<TypeCommand>,
    metrics_tx: mpsc::Sender<MetricsEvent>,
}

// Static regex patterns used in hallucination detection

/// Regex to detect non-ASCII characters.
fn non_ascii_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[^\x20-\x7E]").unwrap())
}

/// Regex to extract meaningful content (non-punctuation/whitespace).
fn meaningful_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#"[\s\.,!\?\-'\"]+"#).unwrap())
}

/// Regex to collapse whitespace.
fn whitespace_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Run the Transcript Manager as a tokio task.
///
/// Processes TranscriptEvents sequentially from a single channel.
/// This is the key architectural win: the fallback race condition becomes
/// impossible because events are processed one at a time.
pub async fn run_transcript_task(
    mut rx: mpsc::Receiver<TranscriptEvent>,
    type_tx: mpsc::Sender<TypeCommand>,
    metrics_tx: mpsc::Sender<MetricsEvent>,
    cancel: CancellationToken,
    config: Config,
) {
    // Locate filters.yaml (same path as Python version)
    let filters_path = find_filters_yaml();
    let filters = Filters::load(&filters_path);

    let mut state = TranscriptState {
        filters,
        item_order: Vec::new(),
        completed_transcripts: HashMap::new(),
        next_output_index: 0,
        recent_transcripts: Vec::new(),
        transcript_buffer: String::new(),
        completed_items: std::collections::HashSet::new(),
        allow_bye_thank_you: config.allow_bye_thank_you,
        allow_non_ascii: config.allow_non_ascii,
        allow_fillers: config.allow_fillers,
        type_tx,
        metrics_tx,
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Transcript task shutting down");
                break;
            }
            event = rx.recv() => {
                match event {
                    Some(event) => handle_event(&mut state, event).await,
                    None => break, // Channel closed
                }
            }
        }
    }
}

async fn handle_event(state: &mut TranscriptState, event: TranscriptEvent) {
    match event {
        TranscriptEvent::ItemCreated { item_id } => {
            if !state.item_order.contains(&item_id) {
                state.item_order.push(item_id);
            }
        }

        TranscriptEvent::RealtimeCompleted {
            item_id,
            transcript,
            duration_ms,
        } => {
            // Race prevention: check if already completed
            if state.completed_items.contains(&item_id) {
                debug!("Skipping already-completed item {}", &item_id[..20.min(item_id.len())]);
                state.metrics_tx.send(MetricsEvent::FallbackRace).await.ok();
                return;
            }
            state.completed_items.insert(item_id.clone());
            state.transcript_buffer.clear();

            state.completed_transcripts.insert(item_id, (transcript, duration_ms));
            flush_ordered_transcripts(state).await;
        }

        TranscriptEvent::FallbackCompleted {
            item_id,
            transcript,
            duration_ms,
        } => {
            // Same race prevention
            if state.completed_items.contains(&item_id) {
                debug!("Skipping already-completed item (fallback) {}", &item_id[..20.min(item_id.len())]);
                state.metrics_tx.send(MetricsEvent::FallbackRace).await.ok();
                return;
            }
            state.completed_items.insert(item_id.clone());

            state.completed_transcripts.insert(item_id, (transcript, duration_ms));
            flush_ordered_transcripts(state).await;
        }

        TranscriptEvent::RealtimeDelta { delta } => {
            state.transcript_buffer.push_str(&delta);
            let filtered = filter_text(state, &state.transcript_buffer.clone());
            if !filtered.is_empty() {
                log_transcript(&filtered, true);
            }
        }

        TranscriptEvent::SessionReset => {
            state.item_order.clear();
            state.completed_transcripts.clear();
            state.completed_items.clear();
            state.next_output_index = 0;
            state.transcript_buffer.clear();
            // Keep recent_transcripts to prevent duplicates across reconnections
            info!("Transcript state reset for new session");
        }
    }
}

async fn flush_ordered_transcripts(state: &mut TranscriptState) {
    while state.next_output_index < state.item_order.len() {
        let next_id = &state.item_order[state.next_output_index];

        if let Some((transcript, duration_ms)) = state.completed_transcripts.remove(next_id) {
            output_transcript(state, &transcript, duration_ms).await;
            state.next_output_index += 1;
        } else {
            break;
        }
    }

    // Trim to prevent unbounded growth
    if state.next_output_index > 100 {
        state.item_order = state.item_order[state.next_output_index..].to_vec();
        state.next_output_index = 0;
    }
}

async fn output_transcript(state: &mut TranscriptState, transcript: &str, duration_ms: Option<u64>) {
    // Check for repetition/speed hallucination before applying regex filters
    if is_repetition_hallucination(transcript, duration_ms) {
        info!("Repetition hallucination detected, dropping: {transcript}");
        state.metrics_tx.send(MetricsEvent::ContentFiltered).await.ok();
        return;
    }

    // Reload filters if changed
    state.filters.reload();

    let filtered = filter_text(state, transcript);

    if !filtered.is_empty() && is_fuzzy_duplicate(state, &filtered) {
        state.metrics_tx.send(MetricsEvent::DuplicateFiltered).await.ok();
        return;
    }

    if !filtered.is_empty() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        state.recent_transcripts.push((now, filtered.clone()));
        if state.recent_transcripts.len() > 14 {
            let len = state.recent_transcripts.len();
            state.recent_transcripts = state.recent_transcripts[len - 7..].to_vec();
        }

        log_transcript(&filtered, false);
        info!("Transcript output: {filtered}");

        // Send to typer
        state
            .type_tx
            .send(TypeCommand {
                text: filtered,
            })
            .await
            .ok();
    } else if !transcript.is_empty() {
        debug!("Filtered out: {transcript}");
        state.metrics_tx.send(MetricsEvent::ContentFiltered).await.ok();
    }
}

/// Apply a fancy-regex filter.
fn apply_fancy_replace(regex: &fancy_regex::Regex, text: &str) -> String {
    regex.replace_all(text, "").into_owned()
}

/// Apply hallucination filters with the 50% rule, then filler filters.
fn filter_text(state: &TranscriptState, text: &str) -> String {
    let filtered = apply_hallucination_filters(state, text);
    if filtered.is_empty() {
        return String::new();
    }

    // Apply filler filters (don't trigger 50% rule)
    let mut result = filtered;
    if !state.allow_fillers {
        for f in &state.filters.fillers {
            result = apply_fancy_replace(&f.regex, &result);
        }
    }

    let result = whitespace_pattern().replace_all(&result, " ").trim().to_string();

    // Post-filler safety: discard if only punctuation/whitespace remains
    let meaningful = meaningful_pattern().replace_all(&result, "");
    if meaningful.is_empty() {
        return String::new();
    }

    result
}

/// Apply hallucination + non-ASCII filters with the 50% rule.
fn apply_hallucination_filters(state: &TranscriptState, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let original = text.trim();
    let original_len = original.len();
    if original_len == 0 {
        return String::new();
    }

    let had_non_ascii = non_ascii_pattern().is_match(original);

    let mut filtered = text.to_string();

    if !state.allow_bye_thank_you {
        for f in &state.filters.hallucinations {
            filtered = apply_fancy_replace(&f.regex, &filtered);
        }
    }

    if !state.allow_non_ascii {
        for f in &state.filters.non_ascii {
            filtered = apply_fancy_replace(&f.regex, &filtered);
        }
    }

    let filtered = whitespace_pattern().replace_all(&filtered, " ").trim().to_string();

    if filtered.is_empty() {
        return String::new();
    }

    let removed_pct = (original_len as f64 - filtered.len() as f64) / original_len as f64;

    // Rule 1: >=50% removed → likely hallucination
    if removed_pct >= 0.5 {
        return String::new();
    }

    // Rule 2: Only punctuation/whitespace remaining
    let meaningful = meaningful_pattern().replace_all(&filtered, "");
    if meaningful.is_empty() {
        return String::new();
    }

    // Rule 3: Very short remaining with significant removal
    if meaningful.len() < 6 && removed_pct > 0.2 {
        return String::new();
    }

    // Rule 4: Foreign language hallucination
    if had_non_ascii && meaningful.len() < 13 && removed_pct > 0.05 {
        return String::new();
    }

    filtered
}

/// Detect hallucinations via vocabulary repetition or impossible speaking speed.
///
/// Thresholds derived from analysis of 34,448 transcriptions with zero false positives:
/// - unique_ratio ≤ 0.40: catches repetitive hallucinations ("One more" x30, etc.)
/// - wps ≥ 10.0: catches fluent but impossibly fast hallucinations (YouTube outros, etc.)
fn is_repetition_hallucination(text: &str, duration_ms: Option<u64>) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    let word_count = words.len();

    if word_count < 10 {
        return false;
    }

    // Check unique word ratio (type-token ratio), case-insensitive
    let unique_lower: std::collections::HashSet<String> = words
        .iter()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();

    let unique_ratio = unique_lower.len() as f64 / word_count as f64;
    if unique_ratio <= 0.40 {
        info!(
            "Repetition hallucination: unique_ratio={:.2} ({}/{} unique words)",
            unique_ratio,
            unique_lower.len(),
            word_count
        );
        return true;
    }

    // Check words per second (if timing available)
    if let Some(ms) = duration_ms {
        if ms > 0 {
            let wps = word_count as f64 / (ms as f64 / 1000.0);
            if wps >= 10.0 {
                info!(
                    "Speed hallucination: {:.1} words/sec ({} words in {}ms)",
                    wps, word_count, ms
                );
                return true;
            }
        }
    }

    false
}

/// Check if text is a fuzzy duplicate of a recent transcript.
fn is_fuzzy_duplicate(state: &TranscriptState, text: &str) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    for (i, (timestamp, previous)) in state.recent_transcripts.iter().rev().enumerate() {
        if i >= 7 {
            break;
        }
        if now - timestamp > 7.0 {
            break;
        }
        let ratio = strsim::normalized_levenshtein(text, previous);
        if ratio >= 0.85 {
            debug!("Fuzzy duplicate ({ratio:.2}): {text}");
            // Can't send async here, so just return
            // Metrics will be sent by caller
            return true;
        }
    }
    false
}

fn log_transcript(text: &str, partial: bool) {
    if text.trim().is_empty() {
        return;
    }
    let prefix = if partial { "[PARTIAL] " } else { "[FINAL]   " };
    println!("{prefix}{text}");
}


fn find_filters_yaml() -> PathBuf {
    // Look for filters.yaml relative to the binary, then in common locations
    let candidates = [
        PathBuf::from("filters.yaml"),
    ];

    for path in &candidates {
        if path.exists() {
            return path.clone();
        }
    }

    // Default — will be caught at load time
    candidates[0].clone()
}
