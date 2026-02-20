#!/usr/bin/env python3
"""
Generate a report of time saved by voice transcription.

Uses pre-computed ratio distributions and CPS values.
"""

import argparse
import json
import math
import re
from datetime import datetime
from pathlib import Path

import matplotlib.pyplot as plt
import matplotlib.dates as mdates

RATIO_FILE = Path(__file__).parent / "ratio_distributions.json"
DEFAULT_CONVERSATIONS_DIR = Path(__file__).parent.parent / "conversations"


def parse_speech_transcripts(conversations_dir: Path) -> list[dict]:
    """Parse speech transcripts from debug JSONL files.

    Reads 'Transcript output: ...' entries from debug_events_*.jsonl files,
    with fallback to legacy transcription_*.txt files.
    """
    entries = []

    # Parse JSONL debug logs (current format)
    for filepath in sorted(conversations_dir.glob("debug_events_*.jsonl")):
        for line in filepath.read_text().split('\n'):
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = event.get("fields", {}).get("message", "")
            if not msg.startswith("Transcript output: "):
                continue
            text = msg[len("Transcript output: "):]
            ts_str = event.get("timestamp", "")
            if not ts_str:
                continue
            # Parse ISO 8601 UTC timestamp, convert to local naive time
            # (matches legacy .txt format which used local time)
            ts_utc = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
            ts = ts_utc.astimezone().replace(tzinfo=None)
            entries.append({'timestamp': ts, 'text': text, 'chars': len(text)})

    # Also parse legacy .txt files (historical data before JSONL migration)
    # These use local time (naive), so we keep them naive for consistency
    jsonl_timestamps = {e['timestamp'] for e in entries}
    txt_pattern = r'\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\] (.+)'
    for filepath in sorted(conversations_dir.glob("transcription_*.txt")):
        for line in filepath.read_text().split('\n'):
            match = re.match(txt_pattern, line)
            if match:
                ts_str, text = match.groups()
                ts = datetime.strptime(ts_str, "%Y-%m-%d %H:%M:%S")
                # Skip if we already have JSONL data at this timestamp (avoid dupes
                # from sessions that wrote both formats)
                if ts not in jsonl_timestamps:
                    entries.append({'timestamp': ts, 'text': text, 'chars': len(text)})

    entries.sort(key=lambda e: e['timestamp'])
    return entries


def load_ratio_distributions() -> dict:
    """Load pre-computed ratio distributions and CPS values."""
    if not RATIO_FILE.exists():
        raise FileNotFoundError(f"Ratio file not found: {RATIO_FILE}")

    with open(RATIO_FILE) as f:
        data = json.load(f)

    def calc_stats(values):
        if not values:
            return {"mean": 0, "std": 0, "stderr": 0, "n": 0}
        n = len(values)
        mean = sum(values) / n
        if n < 2:
            return {"mean": mean, "std": 0, "stderr": 0, "n": n}
        variance = sum((v - mean) ** 2 for v in values) / (n - 1)
        std = math.sqrt(variance)
        stderr = std / math.sqrt(n)
        return {"mean": mean, "std": std, "stderr": stderr, "n": n}

    return {
        "s2c": calc_stats(data["s2c_ratios"]),
        "t2c": calc_stats(data["t2c_ratios"]),
        "typing_cps": data["typing_cps"],
        "speech_cps": data["speech_cps"],
    }


def main():
    parser = argparse.ArgumentParser(description="Generate time saved report from transcription data")
    parser.add_argument(
        "conversations_dir",
        nargs="?",
        type=Path,
        default=DEFAULT_CONVERSATIONS_DIR,
        help=f"Path to conversations directory (default: {DEFAULT_CONVERSATIONS_DIR})"
    )
    args = parser.parse_args()

    print("Loading data...")
    ratios = load_ratio_distributions()

    s2c_stats = ratios["s2c"]
    t2c_stats = ratios["t2c"]
    typing_cps = ratios["typing_cps"]
    speech_cps = ratios["speech_cps"]

    # Load user's speech data
    speech_entries = parse_speech_transcripts(args.conversations_dir)
    total_speech_chars = sum(e['chars'] for e in speech_entries)

    # Compute speedups using pre-computed values
    s2c = s2c_stats["mean"]
    t2c = t2c_stats["mean"]
    content_speedup = speech_cps / (t2c * typing_cps)
    time_saved_per_sec = s2c * (content_speedup - 1)
    actual_speedup = 1 + time_saved_per_sec

    # Print report
    print("=" * 60)
    print("TIME SAVED REPORT")
    print("=" * 60)

    print(f"\n--- MEASURED RATIOS ---")
    print(f"Speech -> Core (s2c): {s2c_stats['mean']:.3f} +/- {s2c_stats['stderr']:.3f} (n={s2c_stats['n']})")
    print(f"Typed -> Core (t2c):  {t2c_stats['mean']:.3f} +/- {t2c_stats['stderr']:.3f} (n={t2c_stats['n']})")

    print(f"\n--- CPS (pre-computed) ---")
    print(f"Typing: {typing_cps:.2f} chars/sec")
    print(f"Speech: {speech_cps:.2f} chars/sec")

    print(f"\n--- THINKING TIME ---")
    content_pct = s2c * 100
    thinking_pct = (1 - s2c) * 100
    print(f"Content: {content_pct:.0f}% of speaking")
    print(f"Thinking (filler): {thinking_pct:.0f}% of speaking")

    print(f"\n--- PER MINUTE OF SPEAKING ---")
    content_speak = 60 * s2c
    thinking = 60 * (1 - s2c)
    content_type = content_speak * content_speedup
    print(f"Speaking: {content_speak:.1f}s content + {thinking:.1f}s thinking = 60s")
    print(f"Typing:   {content_type:.1f}s content + {thinking:.1f}s thinking = {content_type + thinking:.1f}s")
    print(f"Saved:    {time_saved_per_sec * 60:.0f} seconds")

    print(f"\n--- SPEEDUP ---")
    print(f"Content speedup: {content_speedup:.2f}x")
    print(f"Actual speedup:  {actual_speedup:.2f}x (including thinking)")

    # Compute cumulative time saved
    speech_entries_sorted = sorted(speech_entries, key=lambda e: e['timestamp'])
    timestamps = []
    cumulative = []
    total_time_saved = 0
    for entry in speech_entries_sorted:
        core_chars = entry['chars'] * s2c
        time_to_type = core_chars / t2c / typing_cps
        time_to_speak = core_chars / speech_cps
        total_time_saved += time_to_type - time_to_speak
        timestamps.append(entry['timestamp'])
        cumulative.append(total_time_saved / 60)
    total_min = total_time_saved / 60

    print(f"\n--- TOTAL ---")
    print(f"Speech chars: {total_speech_chars:,}")
    print(f"Time saved: {total_min:.0f} minutes ({total_min/60:.1f} hours)")

    # Cumulative time saved with 95% confidence interval
    z = 1.96  # 95% CI
    s2c_lo = s2c_stats['mean'] - z * s2c_stats['stderr']
    s2c_hi = s2c_stats['mean'] + z * s2c_stats['stderr']
    t2c_lo = t2c_stats['mean'] - z * t2c_stats['stderr']
    t2c_hi = t2c_stats['mean'] + z * t2c_stats['stderr']

    cumulative_lo, cumulative_hi = [], []
    total_lo, total_hi = 0, 0
    for entry in speech_entries_sorted:
        # Pessimistic: low s2c, high t2c
        core_lo = entry['chars'] * s2c_lo
        time_to_type_lo = core_lo / t2c_hi / typing_cps
        time_to_speak_lo = core_lo / speech_cps
        total_lo += time_to_type_lo - time_to_speak_lo
        cumulative_lo.append(total_lo / 60)

        # Optimistic: high s2c, low t2c
        core_hi = entry['chars'] * s2c_hi
        time_to_type_hi = core_hi / t2c_lo / typing_cps
        time_to_speak_hi = core_hi / speech_cps
        total_hi += time_to_type_hi - time_to_speak_hi
        cumulative_hi.append(total_hi / 60)

    fig, ax = plt.subplots(figsize=(10, 5))
    ax.fill_between(timestamps, cumulative_lo, cumulative_hi, alpha=0.3, color='blue', label='95% CI')
    ax.plot(timestamps, cumulative, 'b-', linewidth=2, label='Point estimate')
    ax.set_xlabel('Date')
    ax.set_ylabel('Cumulative Time Saved (minutes)')
    ax.set_title(f'Total: {total_min:.0f} min ({total_min/60:.1f} hours)')
    ax.xaxis.set_major_formatter(mdates.DateFormatter('%m/%d'))
    ax.xaxis.set_major_locator(mdates.DayLocator(interval=5))
    ax.grid(True, alpha=0.3)
    ax.legend(loc='upper left')
    plt.setp(ax.xaxis.get_majorticklabels(), rotation=45)

    plt.tight_layout()

    out_path = Path(__file__).parent / 'time_saved.png'
    plt.savefig(out_path, dpi=150)
    plt.close()
    print(f"\nPlot saved to: {out_path}")


if __name__ == "__main__":
    main()
