use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::filters::Filters;
use crate::messages::{MetricsEvent, TranscriptEvent, TypeCommand};

/// How long to wait for an item to complete before skipping it
const QUEUE_TIMEOUT_SECS: f64 = 10.0;

/// When a completion arrives for an item the queue already skipped, type it
/// only if it's at most this many seconds past the skip. Later than that the
/// user has likely moved to a different window/context, and injecting stale
/// text is worse than logging it for manual recovery.
const SKIP_RECOVERY_TYPE_CUTOFF_SECS: f64 = 60.0;

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
    /// When each item was added to item_order (for queue timeout)
    item_created_at: HashMap<String, std::time::Instant>,
    /// Items the queue gave up waiting on: item_id -> when skipped. If their
    /// completion arrives later, we recover the text instead of dropping it.
    skipped_items: HashMap<String, std::time::Instant>,
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
        item_created_at: HashMap::new(),
        skipped_items: HashMap::new(),
        allow_bye_thank_you: config.allow_bye_thank_you,
        allow_non_ascii: config.allow_non_ascii,
        allow_fillers: config.allow_fillers,
        type_tx,
        metrics_tx,
    };

    let mut queue_check = tokio::time::interval(std::time::Duration::from_secs(5));
    queue_check.tick().await;

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
            _ = queue_check.tick() => {
                // Periodically check for timed-out items blocking the queue
                flush_ordered_transcripts(&mut state).await;
            }
        }
    }
}

async fn handle_event(state: &mut TranscriptState, event: TranscriptEvent) {
    match event {
        TranscriptEvent::ItemCreated { item_id } => {
            if !state.item_order.contains(&item_id) {
                state.item_created_at.insert(item_id.clone(), std::time::Instant::now());
                state.item_order.push(item_id);
            }
            // Flush in case a completion arrived before this creation event
            flush_ordered_transcripts(state).await;
        }

        TranscriptEvent::RealtimeCompleted {
            item_id,
            transcript,
            duration_ms,
        } => {
            if state.completed_items.contains(&item_id) {
                // Duplicate completion for an already-completed item. Don't
                // drop blindly — if the prior completion was empty we never
                // emitted text and would lose content. Route through
                // output_transcript. NOTE: fuzzy dedup is currently disabled
                // (see output_transcript), so a true duplicate would type
                // twice; we accept that over losing content. Never observed
                // from the realtime path alone since the fallback removal.
                debug!("Late Realtime for already-completed item {} — routing through output",
                       &item_id[..20.min(item_id.len())]);
                state.metrics_tx.send(MetricsEvent::FallbackRace).await.ok();
                output_transcript(state, &transcript, duration_ms).await;
                return;
            }
            if let Some(skipped_at) = state.skipped_items.remove(&item_id) {
                // The queue timed out on this item and moved past it — its
                // slot in item_order is behind next_output_index, so the
                // normal flush would never emit it. Recover it directly.
                state.completed_items.insert(item_id.clone());
                let late_secs = skipped_at.elapsed().as_secs_f64();
                if late_secs <= SKIP_RECOVERY_TYPE_CUTOFF_SECS {
                    info!("Recovered skipped item {item_id} ({late_secs:.1}s after skip) — typing");
                    output_transcript(state, &transcript, duration_ms).await;
                } else {
                    warn!(
                        "Completion for skipped item {item_id} arrived {late_secs:.0}s after skip — too stale to type, logging for manual recovery: {transcript}"
                    );
                    state.metrics_tx.send(MetricsEvent::ContentFiltered).await.ok();
                }
                return;
            }
            state.completed_items.insert(item_id.clone());
            state.transcript_buffer.clear();

            state.completed_transcripts.insert(item_id, (transcript, duration_ms));
            flush_ordered_transcripts(state).await;
        }

        // Dead path — no emitter since the whisper-1 REST fallback removal.
        // If revived: this arm needs the same skipped_items recovery check as
        // RealtimeCompleted above, or a fallback completion for a queue-skipped
        // item will insert behind next_output_index and silently stick.
        TranscriptEvent::FallbackCompleted {
            item_id,
            transcript,
            duration_ms,
        } => {
            if state.completed_items.contains(&item_id) {
                debug!("Late Fallback for already-completed item {} — routing through output",
                       &item_id[..20.min(item_id.len())]);
                state.metrics_tx.send(MetricsEvent::FallbackRace).await.ok();
                output_transcript(state, &transcript, duration_ms).await;
                return;
            }
            state.completed_items.insert(item_id.clone());

            state.completed_transcripts.insert(item_id, (transcript, duration_ms));
            flush_ordered_transcripts(state).await;
        }

        TranscriptEvent::RealtimeDelta { delta } => {
            state.transcript_buffer.push_str(&delta);
            let filtered = filter_text(state, &state.transcript_buffer);
            if !filtered.is_empty() {
                log_transcript(&filtered, true);
            }
        }

        TranscriptEvent::SessionReset => {
            state.item_order.clear();
            state.completed_transcripts.clear();
            state.completed_items.clear();
            state.item_created_at.clear();
            state.skipped_items.clear();
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
            state.item_created_at.remove(next_id);
            output_transcript(state, &transcript, duration_ms).await;
            state.next_output_index += 1;
        } else {
            // Safety: skip items that have been waiting too long (prevents permanent queue block)
            let next_id_owned = next_id.to_string();
            let should_skip = state.item_created_at
                .get(&next_id_owned)
                .map(|created| created.elapsed().as_secs_f64() >= QUEUE_TIMEOUT_SECS)
                .unwrap_or(false);
            if should_skip {
                warn!("Queue timeout: skipping item {next_id_owned} (waited >{QUEUE_TIMEOUT_SECS}s for completion)");
                state.item_created_at.remove(&next_id_owned);
                // Remember the skip so a late completion can still be
                // recovered (see RealtimeCompleted). Bounded by session
                // lifetime — cleared on SessionReset.
                state.skipped_items.insert(next_id_owned.clone(), std::time::Instant::now());
                state.next_output_index += 1;
            } else {
                // Log what we're blocked on (only once per item, at 3s)
                let next_id_ref = &next_id_owned;
                if let Some(created) = state.item_created_at.get(next_id_ref) {
                    let wait_secs = created.elapsed().as_secs_f64();
                    let pending = state.item_order.len() - state.next_output_index;
                    if wait_secs >= 3.0 {
                        warn!(
                            "Queue blocked on {next_id_owned} (waiting {wait_secs:.1}s, {pending} items pending)"
                        );
                    }
                }
                break;
            }
        }
    }

    // Trim to prevent unbounded growth
    if state.next_output_index > 100 {
        let trimmed: Vec<String> = state.item_order.drain(..state.next_output_index).collect();
        for id in &trimmed {
            state.item_created_at.remove(id);
        }
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

    // Fuzzy dedup temporarily disabled (2026-06-07): its primary justification
    // was the realtime/whisper-1-fallback race, which is gone now that the
    // fallback path was removed. With the race gone, the remaining effect is
    // suppressing legitimately repeated speech ("yes, yes", repeated commands).
    // Trialing with it off; re-enable by uncommenting if dupes reappear.
    // if !filtered.is_empty() && is_fuzzy_duplicate(state, &filtered) {
    //     state.metrics_tx.send(MetricsEvent::DuplicateFiltered).await.ok();
    //     return;
    // }

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

        // Send to typer (non-blocking: never let a stuck typer block transcription)
        match state.type_tx.try_send(TypeCommand {
            text: filtered,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("Typer channel full — dropping transcript (typer may be stuck)");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("Typer channel closed — typer task has died");
            }
        }
    } else if !transcript.is_empty() {
        debug!("Filtered out: {transcript}");
        state.metrics_tx.send(MetricsEvent::ContentFiltered).await.ok();
    }
}

/// Collect all match ranges from multiple patterns on the original text,
/// merge overlapping ranges, and remove them all at once.
/// This makes pattern order irrelevant — each pattern sees the original text.
fn collect_and_remove(text: &str, filters: &[crate::filters::CompiledFilter]) -> String {
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for f in filters {
        for m in f.regex.find_iter(text) {
            if let Ok(m) = m {
                ranges.push((m.start(), m.end()));
            }
        }
    }

    if ranges.is_empty() {
        return text.to_string();
    }

    // Sort by start position, then by end (longest first for same start)
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    // Merge overlapping ranges
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    // Build result by copying non-removed segments
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;
    for (start, end) in &merged {
        if pos < *start {
            result.push_str(&text[pos..*start]);
        }
        pos = *end;
    }
    if pos < text.len() {
        result.push_str(&text[pos..]);
    }

    result
}

/// Apply hallucination filters with the 50% rule, then filler filters.
fn filter_text(state: &TranscriptState, text: &str) -> String {
    let filtered = apply_hallucination_filters(state, text);
    if filtered.is_empty() {
        return String::new();
    }

    // Apply filler filters (don't trigger 50% rule)
    let result = if !state.allow_fillers {
        collect_and_remove(&filtered, &state.filters.fillers)
    } else {
        filtered
    };

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

    // Collect all match ranges from hallucination + non-ASCII patterns on original text
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    if !state.allow_bye_thank_you {
        for f in &state.filters.hallucinations {
            for m in f.regex.find_iter(text) {
                if let Ok(m) = m {
                    ranges.push((m.start(), m.end()));
                }
            }
        }
    }
    if !state.allow_non_ascii {
        for f in &state.filters.non_ascii {
            for m in f.regex.find_iter(text) {
                if let Ok(m) = m {
                    ranges.push((m.start(), m.end()));
                }
            }
        }
    }

    let filtered = if ranges.is_empty() {
        text.to_string()
    } else {
        ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in ranges {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        let mut result = String::with_capacity(text.len());
        let mut pos = 0;
        for (start, end) in &merged {
            if pos < *start {
                result.push_str(&text[pos..*start]);
            }
            pos = *end;
        }
        if pos < text.len() {
            result.push_str(&text[pos..]);
        }
        result
    };

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
///
/// `duration_ms` is the committed-audio length (pre-roll + speech + trailing
/// silence), which is what the transcript covers. The 10 wps threshold was
/// calibrated (2026-03) against server-VAD speech spans; against committed
/// audio it's more conservative. Re-evaluated 2026-09-08 over every paired
/// commit since the local-VAD switch (6,393 transcripts): real speech peaks at
/// 6.3 wps of committed audio; the old speech-only estimate (which excluded the
/// 1s pre-roll and was wall-clock, so it collapsed to ~0 on burst-delivered
/// audio after unmute) produced six false drops of real sentences and caught
/// nothing. Known cost: short outros (10–13 words in ~2s) fall below 10 wps of
/// committed audio — those are filters.yaml's job.
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
// Temporarily uncalled (2026-06-07) — see disabled check in output_transcript.
#[allow(dead_code)]
fn is_fuzzy_duplicate(state: &TranscriptState, text: &str) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    for (i, (timestamp, previous)) in state.recent_transcripts.iter().rev().enumerate() {
        if i >= 7 {
            break;
        }
        if now - timestamp > 20.0 {
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
    // Look for filters.yaml in CWD (expected to run from project root)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(
        type_tx: mpsc::Sender<TypeCommand>,
        metrics_tx: mpsc::Sender<MetricsEvent>,
    ) -> TranscriptState {
        TranscriptState {
            // Nonexistent path -> empty filter sets; text passes through.
            filters: Filters::load(&PathBuf::from("/nonexistent/filters.yaml")),
            item_order: Vec::new(),
            completed_transcripts: HashMap::new(),
            next_output_index: 0,
            recent_transcripts: Vec::new(),
            transcript_buffer: String::new(),
            completed_items: std::collections::HashSet::new(),
            item_created_at: HashMap::new(),
            skipped_items: HashMap::new(),
            allow_bye_thank_you: false,
            allow_non_ascii: false,
            allow_fillers: false,
            type_tx,
            metrics_tx,
        }
    }

    fn skipped_ago(secs: u64) -> std::time::Instant {
        std::time::Instant::now() - std::time::Duration::from_secs(secs)
    }

    /// A completion arriving for a queue-skipped item shortly after the skip
    /// must still be typed (2026-07-14 incident: previously it was logged
    /// and silently lost).
    #[tokio::test]
    async fn skipped_item_recovered_when_fresh() {
        let (ttx, mut trx) = mpsc::channel(8);
        let (mtx, mut _mrx) = mpsc::channel(64);
        let mut state = test_state(ttx, mtx);
        state.skipped_items.insert("item_A".to_string(), skipped_ago(5));

        handle_event(
            &mut state,
            TranscriptEvent::RealtimeCompleted {
                item_id: "item_A".to_string(),
                transcript: "this text was stalled but is still fresh".to_string(),
                duration_ms: Some(4000),
            },
        )
        .await;

        let cmd = trx.try_recv().expect("fresh recovery should be typed");
        assert_eq!(cmd.text, "this text was stalled but is still fresh");
        assert!(state.skipped_items.is_empty());
        assert!(state.completed_items.contains("item_A"));
    }

    /// Past the staleness cutoff the text must NOT be auto-typed (the user
    /// has moved on -- it would land in whatever window has focus), only
    /// WARN-logged for manual recovery.
    #[tokio::test]
    async fn skipped_item_too_stale_is_not_typed() {
        let (ttx, mut trx) = mpsc::channel(8);
        let (mtx, mut _mrx) = mpsc::channel(64);
        let mut state = test_state(ttx, mtx);
        state
            .skipped_items
            .insert("item_A".to_string(), skipped_ago(120));

        handle_event(
            &mut state,
            TranscriptEvent::RealtimeCompleted {
                item_id: "item_A".to_string(),
                transcript: "this arrived minutes late".to_string(),
                duration_ms: Some(4000),
            },
        )
        .await;

        assert!(trx.try_recv().is_err(), "stale recovery must not type");
        assert!(state.completed_items.contains("item_A"));
    }

    /// End-to-end through the ordering queue: skip is recorded when the
    /// timeout fires, and later items are not blocked.
    #[tokio::test]
    async fn queue_timeout_records_skip_and_unblocks() {
        let (ttx, mut trx) = mpsc::channel(8);
        let (mtx, mut _mrx) = mpsc::channel(64);
        let mut state = test_state(ttx, mtx);

        // Two items created; A never completes, B does.
        for id in ["item_A", "item_B"] {
            handle_event(
                &mut state,
                TranscriptEvent::ItemCreated {
                    item_id: id.to_string(),
                },
            )
            .await;
        }
        handle_event(
            &mut state,
            TranscriptEvent::RealtimeCompleted {
                item_id: "item_B".to_string(),
                transcript: "second utterance text here".to_string(),
                duration_ms: Some(3000),
            },
        )
        .await;
        // B is held back: queue is blocked on A.
        assert!(trx.try_recv().is_err(), "B must wait for A");

        // Simulate A exceeding the queue timeout, then run the flush.
        state
            .item_created_at
            .insert("item_A".to_string(), skipped_ago(QUEUE_TIMEOUT_SECS as u64 + 5));
        flush_ordered_transcripts(&mut state).await;

        // A was skipped (recorded for recovery), B was released.
        assert!(state.skipped_items.contains_key("item_A"));
        let cmd = trx.try_recv().expect("B should flush after A is skipped");
        assert_eq!(cmd.text, "second utterance text here");
    }

    /// Pins the wps calibration against committed-audio durations. The
    /// "real" cases mirror the word count and committed-audio length of two
    /// production false drops (2026-07-18: 12 words / 2250ms, rejected at 34
    /// wps by the old speech-only estimate; 2026-07-12: 14 words / 2225ms,
    /// rejected at 11 wps). The hallucination case mirrors a 2026-04 fluent
    /// blob (30 words / ~1.7s) that neither the yaml patterns nor the
    /// unique-ratio check caught — the speed rule is the only defense for
    /// that class and must keep firing.
    #[test]
    fn speed_rule_calibrated_against_committed_audio() {
        // 12 words / 2250ms committed (logged as "2250ms buffer / ~352ms speech").
        assert!(!is_repetition_hallucination(
            "please rerun the eval with the new config and paste the numbers",
            Some(2250)
        ));
        // 14 words / 2225ms committed (logged as "~1248ms speech").
        assert!(!is_repetition_hallucination(
            "can you move the second helper into its own file and add a docstring",
            Some(2225)
        ));
        // Fastest real speech observed over 6,393 transcripts: 6.3 wps. Anything
        // under 10 wps of committed audio must pass.
        assert!(!is_repetition_hallucination(
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen",
            Some(2225)
        ));
        // 30 fluent, non-repetitive words over ~1.7s of committed audio.
        assert!(is_repetition_hallucination(
            "and that wraps up everything for today so make sure you come back next time \
             for more tips and tutorials and as always take care of yourselves and each other",
            Some(1740)
        ));
        // No duration => speed rule can't fire (only the unique-ratio check remains).
        assert!(!is_repetition_hallucination(
            "please rerun the eval with the new config and paste the numbers",
            None
        ));
    }

    /// Runs the shipped filters.yaml through the real filter path.
    #[test]
    fn leave_comments_below_outro_is_filtered_but_dictation_survives() {
        let (type_tx, _trx) = mpsc::channel(8);
        let (metrics_tx, _mrx) = mpsc::channel(8);
        let mut state = test_state(type_tx, metrics_tx);
        state.filters = Filters::load(&PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../filters.yaml"
        )));
        assert!(!state.filters.hallucinations.is_empty(), "filters.yaml should have loaded");

        // Outro with a qualifier between "your" and "comments" (the shape that
        // slipped the old literal pattern): the clause is removed and the 50%
        // rule drops the remainder.
        assert_eq!(
            filter_text(&state, "Try it yourself at home and leave your honest comments below."),
            ""
        );
        assert_eq!(filter_text(&state, "Please leave a comment below!"), "");
        assert_eq!(filter_text(&state, "Leave your comments below"), "");

        // Dictation that happens to contain the phrase mid-clause must not be touched.
        assert_eq!(
            filter_text(&state, "leave your comments below the function"),
            "leave your comments below the function"
        );
        assert_eq!(
            filter_text(&state, "Can you leave a comment below that line explaining the retry?"),
            "Can you leave a comment below that line explaining the retry?"
        );
    }
}
