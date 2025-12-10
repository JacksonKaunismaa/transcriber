#!/usr/bin/env python3
"""
Calculate and visualize time saved by using voice transcription vs typing.

Reads all transcription logs, counts words, and estimates time saved based on
typing speed vs speaking speed.
"""

import argparse
import re
from collections import defaultdict
from datetime import datetime
from pathlib import Path

try:
    import matplotlib.pyplot as plt
    import matplotlib.dates as mdates
    HAS_MATPLOTLIB = True
except ImportError:
    HAS_MATPLOTLIB = False


def parse_transcription_file(filepath: Path) -> list[tuple[datetime, str]]:
    """Parse a transcription log file and return list of (timestamp, text) tuples."""
    entries = []
    pattern = re.compile(r'^\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\] (.+)$')

    with open(filepath, 'r', encoding='utf-8') as f:
        for line in f:
            match = pattern.match(line.strip())
            if match:
                timestamp = datetime.strptime(match.group(1), '%Y-%m-%d %H:%M:%S')
                text = match.group(2)
                entries.append((timestamp, text))

    return entries


def count_words(text: str) -> int:
    """Count words in text using standard WPM definition (characters / 5)."""
    return len(text) // 5


def calculate_time_saved(
    conversations_dir: Path,
    typing_wpm: float = 90,
    speaking_wpm: float = 240,
) -> dict:
    """
    Calculate time saved from all transcription logs.

    Returns dict with:
        - daily_words: {date: word_count}
        - daily_time_saved_minutes: {date: minutes_saved}
        - total_words: int
        - total_time_saved_minutes: float
    """
    daily_words = defaultdict(int)

    # Find all transcription files
    transcription_files = sorted(conversations_dir.glob('transcription_*.txt'))

    for filepath in transcription_files:
        entries = parse_transcription_file(filepath)
        for timestamp, text in entries:
            date = timestamp.date()
            words = count_words(text)
            daily_words[date] += words

    # Calculate time saved per day
    # Time to type = words / typing_wpm
    # Time to speak = words / speaking_wpm
    # Time saved = time_to_type - time_to_speak
    daily_time_saved = {}
    for date, words in daily_words.items():
        time_to_type = words / typing_wpm
        time_to_speak = words / speaking_wpm
        time_saved = time_to_type - time_to_speak
        daily_time_saved[date] = time_saved

    total_words = sum(daily_words.values())
    total_time_saved = sum(daily_time_saved.values())

    return {
        'daily_words': dict(daily_words),
        'daily_time_saved_minutes': daily_time_saved,
        'total_words': total_words,
        'total_time_saved_minutes': total_time_saved,
    }


def plot_time_saved(results: dict, output_path: Path = None):
    """Plot cumulative time saved and daily usage."""
    if not HAS_MATPLOTLIB:
        print("\n[WARNING] matplotlib not installed, skipping plot")
        print("  Install with: uv pip install matplotlib")
        return

    dates = sorted(results['daily_time_saved_minutes'].keys())
    daily_saved = [results['daily_time_saved_minutes'][d] for d in dates]
    daily_words = [results['daily_words'][d] for d in dates]

    # Calculate cumulative time saved
    cumulative = []
    total = 0
    for saved in daily_saved:
        total += saved
        cumulative.append(total)

    # Create figure with two subplots
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(12, 8), sharex=True)
    fig.suptitle('Time Saved Using Voice Transcription', fontsize=14, fontweight='bold')

    # Plot 1: Cumulative time saved
    ax1.fill_between(dates, cumulative, alpha=0.3, color='green')
    ax1.plot(dates, cumulative, color='green', linewidth=2, marker='o', markersize=4)
    ax1.set_ylabel('Cumulative Time Saved (minutes)', fontsize=11)
    ax1.grid(True, alpha=0.3)
    ax1.set_ylim(bottom=0)

    # Add total hours annotation
    total_hours = results['total_time_saved_minutes'] / 60
    ax1.annotate(
        f'Total: {total_hours:.1f} hours',
        xy=(dates[-1], cumulative[-1]),
        xytext=(10, -10),
        textcoords='offset points',
        fontsize=12,
        fontweight='bold',
        color='green',
    )

    # Plot 2: Daily usage (words per day)
    ax2.bar(dates, daily_words, color='steelblue', alpha=0.7, width=0.8)
    ax2.set_ylabel('Words Transcribed per Day', fontsize=11)
    ax2.set_xlabel('Date', fontsize=11)
    ax2.grid(True, alpha=0.3, axis='y')

    # Format x-axis dates
    ax2.xaxis.set_major_formatter(mdates.DateFormatter('%b %d'))
    ax2.xaxis.set_major_locator(mdates.AutoDateLocator())
    plt.xticks(rotation=45, ha='right')

    plt.tight_layout()

    if output_path:
        plt.savefig(output_path, dpi=150, bbox_inches='tight')
        print(f"\nPlot saved to: {output_path}")
    else:
        plt.show()


def main():
    parser = argparse.ArgumentParser(
        description='Calculate time saved using voice transcription vs typing',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python scripts/time_saved.py
  python scripts/time_saved.py --typing-wpm 80 --speaking-wpm 200
  python scripts/time_saved.py --save-plot time_saved.png
        """
    )
    parser.add_argument(
        '--typing-wpm',
        type=float,
        default=90,
        help='Your typing speed in words per minute (default: 90)'
    )
    parser.add_argument(
        '--speaking-wpm',
        type=float,
        default=240,
        help='Your speaking speed in words per minute (default: 240)'
    )
    parser.add_argument(
        '--conversations-dir',
        type=Path,
        default=Path('conversations'),
        help='Directory containing transcription logs (default: conversations/)'
    )
    parser.add_argument(
        '--save-plot',
        type=Path,
        default=None,
        help='Save plot to file instead of displaying'
    )
    parser.add_argument(
        '--no-plot',
        action='store_true',
        help='Skip plotting, just show summary'
    )

    args = parser.parse_args()

    if not args.conversations_dir.exists():
        print(f"Error: Conversations directory not found: {args.conversations_dir}")
        return 1

    print(f"Analyzing transcription logs in: {args.conversations_dir}")
    print(f"Assumptions: typing={args.typing_wpm} WPM, speaking={args.speaking_wpm} WPM")
    print()

    results = calculate_time_saved(
        args.conversations_dir,
        typing_wpm=args.typing_wpm,
        speaking_wpm=args.speaking_wpm,
    )

    # Print summary
    total_words = results['total_words']
    total_minutes = results['total_time_saved_minutes']
    total_hours = total_minutes / 60
    num_days = len(results['daily_words'])

    print("=" * 50)
    print("SUMMARY")
    print("=" * 50)
    print(f"  Days with transcriptions:  {num_days}")
    print(f"  Total words transcribed:   {total_words:,}")
    print(f"  Time saved (minutes):      {total_minutes:.1f}")
    print(f"  Time saved (hours):        {total_hours:.2f}")
    print()

    if num_days > 0:
        avg_words_per_day = total_words / num_days
        avg_minutes_per_day = total_minutes / num_days
        print(f"  Avg words/day:             {avg_words_per_day:.0f}")
        print(f"  Avg time saved/day:        {avg_minutes_per_day:.1f} min")

    # Time breakdown
    time_typing = total_words / args.typing_wpm
    time_speaking = total_words / args.speaking_wpm
    print()
    print(f"  Would have taken to type:  {time_typing:.1f} min ({time_typing/60:.2f} hrs)")
    print(f"  Took to speak:             {time_speaking:.1f} min ({time_speaking/60:.2f} hrs)")
    print("=" * 50)

    # Plot
    if not args.no_plot and num_days > 0:
        plot_time_saved(results, args.save_plot)

    return 0


if __name__ == '__main__':
    exit(main())
