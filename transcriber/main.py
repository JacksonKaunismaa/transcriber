#!/usr/bin/env python3
"""
Real-time audio transcription with OpenAI API.

CLI entry point for the transcriber application.
"""

import os
import sys
import argparse
from dotenv import load_dotenv

from transcriber.deps import check_system_dependencies
from transcriber.session import TranscriptionSession

# Load environment variables
load_dotenv()


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Real-time audio transcription with OpenAI API",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Available models:
  whisper-1            Whisper transcription model (default, most accurate)
  gpt-4o-transcribe    GPT-4o transcription (fast, high quality)
  gpt-4o-mini-transcribe  GPT-4o mini transcription (faster, lower cost)

Examples:
  transcribe                    # Use default whisper-1 model
  transcribe --model gpt-4o-transcribe
  transcribe -m whisper-1
  transcribe --allow-bye-thank-you    # Disable hallucination filtering
  transcribe --allow-non-ascii        # Allow non-ASCII characters
""",
    )
    parser.add_argument(
        "-m",
        "--model",
        default="whisper-1",
        choices=["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"],
        help="Transcription model to use (default: whisper-1)",
    )
    parser.add_argument(
        "--allow-bye-thank-you",
        action="store_true",
        help="Disable hallucination filtering (false positives, YouTube outros, etc.)",
    )
    parser.add_argument(
        "--allow-non-ascii",
        action="store_true",
        help="Allow non-ASCII characters in transcription",
    )
    parser.add_argument(
        "--allow-fillers",
        action="store_true",
        help="Don't filter out filler words (um, uh, hmm, etc.)",
    )
    parser.add_argument(
        "--no-log",
        action="store_true",
        help="Don't save transcriptions to conversations/ directory",
    )
    args = parser.parse_args()

    print()
    print("=" * 60)
    print("Real-Time Transcription with OpenAI")
    print("=" * 60)
    print()

    if not check_system_dependencies():
        print(
            "\n[ERROR] Cannot start transcription due to missing dependencies.",
            file=sys.stderr,
        )
        sys.exit(1)

    api_key = os.getenv("OPENAI_API_KEY")

    if not api_key:
        print("\n[ERROR] OPENAI_API_KEY not found!", file=sys.stderr)
        print("[ERROR] Please create a .env file with:", file=sys.stderr)
        print("[ERROR]   OPENAI_API_KEY=your_api_key_here", file=sys.stderr)
        sys.exit(1)

    session = TranscriptionSession(
        api_key,
        model=args.model,
        allow_bye_thank_you=args.allow_bye_thank_you,
        allow_non_ascii=args.allow_non_ascii,
        allow_fillers=args.allow_fillers,
        no_log=args.no_log,
    )

    try:
        session.run()
    except Exception:
        session.logger.exception('"Fatal error"')
        session.cleanup()
        sys.exit(1)


if __name__ == "__main__":
    main()
