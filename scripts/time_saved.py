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

import numpy as np
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from scipy.stats import gaussian_kde

RATIO_FILE = Path(__file__).parent / "ratio_distributions.json"
DEFAULT_CONVERSATIONS_DIR = Path(__file__).parent.parent / "conversations"


def parse_speech_transcripts(conversations_dir: Path) -> list[dict]:
    """Parse speech transcripts into entries with timestamps and char count."""
    entries = []
    pattern = r'\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\] (.+)'

    for filepath in sorted(conversations_dir.glob("transcription_*.txt")):
        for line in filepath.read_text().split('\n'):
            match = re.match(pattern, line)
            if match:
                ts_str, text = match.groups()
                ts = datetime.strptime(ts_str, "%Y-%m-%d %H:%M:%S")
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
            return {"mean": 0, "std": 0, "stderr": 0, "n": 0, "values": []}
        n = len(values)
        mean = sum(values) / n
        if n < 2:
            return {"mean": mean, "std": 0, "stderr": 0, "n": n, "values": values}
        variance = sum((v - mean) ** 2 for v in values) / (n - 1)
        std = math.sqrt(variance)
        stderr = std / math.sqrt(n)
        return {"mean": mean, "std": std, "stderr": stderr, "n": n, "values": values}

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

    # Plot - 4 subplots: s2c hist, t2c hist, bubble plot, cumulative with CI
    fig, axes = plt.subplots(2, 2, figsize=(12, 10))

    # 1. s2c distribution (top-left)
    ax1 = axes[0, 0]
    ax1.hist(s2c_stats['values'], bins=20, edgecolor='black', alpha=0.7)
    ax1.axvline(s2c_stats['mean'], color='red', linestyle='--', label=f"mean={s2c_stats['mean']:.3f}")
    ax1.set_xlabel('Speech -> Core ratio')
    ax1.set_ylabel('Count')
    ax1.set_title(f"s2c Distribution (n={s2c_stats['n']})")
    ax1.legend()

    # 2. t2c distribution (top-right)
    ax2 = axes[0, 1]
    ax2.hist(t2c_stats['values'], bins=20, edgecolor='black', alpha=0.7)
    ax2.axvline(t2c_stats['mean'], color='red', linestyle='--', label=f"mean={t2c_stats['mean']:.3f}")
    ax2.set_xlabel('Typed -> Core ratio')
    ax2.set_ylabel('Count')
    ax2.set_title(f"t2c Distribution (n={t2c_stats['n']})")
    ax2.legend()

    # 3. Bubble plot: s2c vs t2c with speedup as color, density as size (bottom-left)
    ax3 = axes[1, 0]
    s2c_vals = np.array(s2c_stats['values'])
    t2c_vals = np.array(t2c_stats['values'])

    s2c_range = np.linspace(s2c_vals.min(), s2c_vals.max(), 15)
    t2c_range = np.linspace(t2c_vals.min(), t2c_vals.max(), 15)
    S2C_grid, T2C_grid = np.meshgrid(s2c_range, t2c_range)
    s2c_flat = S2C_grid.flatten()
    t2c_flat = T2C_grid.flatten()

    speedup_flat = 1 + s2c_flat * (speech_cps / (t2c_flat * typing_cps) - 1)

    s2c_kde = gaussian_kde(s2c_vals)
    t2c_kde = gaussian_kde(t2c_vals)
    density_flat = s2c_kde(s2c_flat) * t2c_kde(t2c_flat)
    size_flat = (density_flat / density_flat.max()) * 300 + 10

    scatter = ax3.scatter(s2c_flat, t2c_flat, s=size_flat, c=speedup_flat,
                          cmap='hot_r', alpha=0.7, edgecolors='black', linewidths=0.3)
    plt.colorbar(scatter, ax=ax3, label='Actual Speedup')

    ax3.axhline(t2c, color='black', linestyle=':', alpha=0.7, linewidth=1)
    ax3.axvline(s2c, color='black', linestyle=':', alpha=0.7, linewidth=1)
    ax3.scatter([s2c], [t2c], color='black', s=150, marker='x', linewidths=2, zorder=10,
                label=f'Mean ({s2c:.3f}, {t2c:.3f}) -> {actual_speedup:.2f}x')

    ax3.annotate(f'{s2c:.3f}', xy=(s2c, t2c_vals.min()), xytext=(s2c, t2c_vals.min() - 0.05),
                 ha='center', fontsize=8)
    ax3.annotate(f'{t2c:.3f}', xy=(s2c_vals.min(), t2c), xytext=(s2c_vals.min() - 0.03, t2c),
                 ha='right', va='center', fontsize=8)

    ax3.set_xlabel('s2c (Speech -> Core)')
    ax3.set_ylabel('t2c (Typed -> Core)')
    ax3.set_title('Speedup (color) & Density (size)')
    ax3.legend(loc='upper right', fontsize=8)

    # 4. Cumulative time saved with 95% confidence interval (bottom-right)
    # Confidence bounds from uncertainty in s2c and t2c
    z = 1.96  # 95% CI
    s2c_lo = s2c_stats['mean'] - z * s2c_stats['stderr']
    s2c_hi = s2c_stats['mean'] + z * s2c_stats['stderr']
    t2c_lo = t2c_stats['mean'] - z * t2c_stats['stderr']
    t2c_hi = t2c_stats['mean'] + z * t2c_stats['stderr']

    # Compute cumulative bounds
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

    ax4 = axes[1, 1]
    ax4.fill_between(timestamps, cumulative_lo, cumulative_hi, alpha=0.3, color='blue', label='95% CI')
    ax4.plot(timestamps, cumulative, 'b-', linewidth=2, label='Point estimate')
    ax4.set_xlabel('Date')
    ax4.set_ylabel('Cumulative Time Saved (minutes)')
    ax4.set_title(f'Total: {total_min:.0f} min ({total_min/60:.1f} hours)')
    ax4.xaxis.set_major_formatter(mdates.DateFormatter('%m/%d'))
    ax4.xaxis.set_major_locator(mdates.DayLocator(interval=5))
    ax4.grid(True, alpha=0.3)
    ax4.legend(loc='upper left')
    plt.setp(ax4.xaxis.get_majorticklabels(), rotation=45)

    plt.tight_layout()

    out_path = Path(__file__).parent / 'time_saved.png'
    plt.savefig(out_path, dpi=150)
    plt.close()
    print(f"\nPlot saved to: {out_path}")


if __name__ == "__main__":
    main()
