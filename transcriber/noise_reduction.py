"""
Audio noise reduction using WebRTC noise suppression.

This module provides real-time noise reduction by:
1. Resampling from 24kHz to 16kHz (required by WebRTC)
2. Processing through WebRTC noise suppression
3. Applying manual gain boost
4. Resampling back to 24kHz for the transcription API
"""

import numpy as np
from typing import Optional


class AudioProcessor:
    """
    Real-time audio processor with noise suppression and gain boost.

    Handles resampling between 24kHz (transcription API) and 16kHz (WebRTC).
    """

    def __init__(
        self,
        noise_suppression_level: int = 2,
        gain_multiplier: float = 1.0,
        input_sample_rate: int = 24000,
    ):
        """
        Initialize the audio processor.

        Args:
            noise_suppression_level: 0-4, where 0=off, 4=max suppression (default: 2)
            gain_multiplier: Volume multiplier, e.g. 2.0 = double volume (default: 1.0)
            input_sample_rate: Sample rate of input audio (default: 24000)
        """
        from webrtc_noise_gain import AudioProcessor as WebRTCAudioProcessor

        self.input_sample_rate = input_sample_rate
        self.webrtc_sample_rate = 16000
        self.noise_suppression_level = noise_suppression_level
        self.gain_multiplier = gain_multiplier

        # Initialize WebRTC processor with NO auto-gain (0) - we do gain manually
        # Args: (auto_gain_dbfs, noise_suppression_level)
        self.processor = WebRTCAudioProcessor(0, noise_suppression_level)

        # WebRTC requires exactly 10ms chunks at 16kHz = 160 samples = 320 bytes
        self.webrtc_chunk_samples = 160
        self.webrtc_chunk_bytes = 320

        # Buffer for accumulating resampled audio
        self._resample_buffer = np.array([], dtype=np.int16)

        # Buffer for accumulating processed audio to resample back
        self._output_buffer = np.array([], dtype=np.int16)

    def _resample(self, audio: np.ndarray, from_rate: int, to_rate: int) -> np.ndarray:
        """Simple linear interpolation resampling."""
        if from_rate == to_rate:
            return audio

        # Calculate output length
        duration = len(audio) / from_rate
        output_length = int(duration * to_rate)

        if output_length == 0:
            return np.array([], dtype=audio.dtype)

        # Linear interpolation
        x_old = np.linspace(0, 1, len(audio))
        x_new = np.linspace(0, 1, output_length)
        resampled = np.interp(x_new, x_old, audio.astype(np.float32))

        return resampled.astype(audio.dtype)

    def _apply_gain(self, audio: np.ndarray) -> np.ndarray:
        """Apply gain multiplier with clipping to prevent overflow."""
        if self.gain_multiplier == 1.0:
            return audio

        # Convert to float, apply gain, clip, convert back
        amplified = audio.astype(np.float32) * self.gain_multiplier
        clipped = np.clip(amplified, -32768, 32767)
        return clipped.astype(np.int16)

    def process_chunk(self, audio_bytes: bytes) -> bytes:
        """
        Process an audio chunk through noise suppression and gain.

        Args:
            audio_bytes: Raw PCM16 audio at input_sample_rate

        Returns:
            Processed PCM16 audio at input_sample_rate
        """
        # Convert bytes to numpy array (16-bit signed integers)
        audio = np.frombuffer(audio_bytes, dtype=np.int16)

        # Resample from 24kHz to 16kHz
        resampled = self._resample(audio, self.input_sample_rate, self.webrtc_sample_rate)

        # Add to buffer
        self._resample_buffer = np.concatenate([self._resample_buffer, resampled])

        # Process complete 10ms chunks through WebRTC
        processed_chunks = []
        while len(self._resample_buffer) >= self.webrtc_chunk_samples:
            # Extract 10ms chunk
            chunk = self._resample_buffer[:self.webrtc_chunk_samples]
            self._resample_buffer = self._resample_buffer[self.webrtc_chunk_samples:]

            # Convert to bytes for WebRTC
            chunk_bytes = chunk.tobytes()

            # Process through WebRTC noise suppression
            result = self.processor.Process10ms(chunk_bytes)

            # Collect processed audio
            processed_audio = np.frombuffer(result.audio, dtype=np.int16)
            processed_chunks.append(processed_audio)

        if not processed_chunks:
            # Not enough audio accumulated yet, return empty
            return b''

        # Combine processed chunks
        processed = np.concatenate(processed_chunks)

        # Apply manual gain boost AFTER noise suppression
        processed = self._apply_gain(processed)

        # Add to output buffer
        self._output_buffer = np.concatenate([self._output_buffer, processed])

        # Resample back to 24kHz
        if len(self._output_buffer) > 0:
            # Resample whatever we have
            upsampled = self._resample(
                self._output_buffer,
                self.webrtc_sample_rate,
                self.input_sample_rate
            )
            self._output_buffer = np.array([], dtype=np.int16)

            return upsampled.tobytes()

        return b''

    def flush(self) -> bytes:
        """
        Flush any remaining buffered audio.

        Call this at the end of a session to get any remaining processed audio.
        """
        if len(self._output_buffer) == 0 and len(self._resample_buffer) == 0:
            return b''

        # Process any remaining samples in resample buffer
        # Pad to make a complete chunk if needed
        if len(self._resample_buffer) > 0:
            padding_needed = self.webrtc_chunk_samples - len(self._resample_buffer)
            if padding_needed > 0:
                self._resample_buffer = np.concatenate([
                    self._resample_buffer,
                    np.zeros(padding_needed, dtype=np.int16)
                ])

            chunk_bytes = self._resample_buffer[:self.webrtc_chunk_samples].tobytes()
            result = self.processor.Process10ms(chunk_bytes)
            processed = np.frombuffer(result.audio, dtype=np.int16)
            processed = self._apply_gain(processed)
            self._output_buffer = np.concatenate([self._output_buffer, processed])
            self._resample_buffer = np.array([], dtype=np.int16)

        # Resample remaining output buffer
        if len(self._output_buffer) > 0:
            upsampled = self._resample(
                self._output_buffer,
                self.webrtc_sample_rate,
                self.input_sample_rate
            )
            self._output_buffer = np.array([], dtype=np.int16)
            return upsampled.tobytes()

        return b''


def create_audio_processor(
    noise_suppression_level: int = 2,
    gain_multiplier: float = 1.0,
) -> Optional["AudioProcessor"]:
    """
    Create an audio processor if webrtc-noise-gain is available.

    Args:
        noise_suppression_level: 0-4, where 0=off, 4=max suppression
        gain_multiplier: Volume boost, e.g. 2.0 = 2x louder

    Returns None if the library is not installed.
    """
    try:
        return AudioProcessor(
            noise_suppression_level=noise_suppression_level,
            gain_multiplier=gain_multiplier,
        )
    except ImportError:
        return None
