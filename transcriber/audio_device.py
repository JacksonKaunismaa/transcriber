#!/usr/bin/env python3
"""Audio device selection and management."""

import pyaudio
import sys
from typing import Optional


def find_compatible_device(audio: pyaudio.PyAudio, target_rate: int = 24000, verbose: bool = True) -> Optional[int]:
    """
    Find an audio input device that supports the target sample rate.

    Args:
        audio: PyAudio instance
        target_rate: Target sample rate (default: 24000 Hz)
        verbose: Print device selection info (default: True)

    Returns:
        Device index if found, None otherwise
    """
    # First, try to find PipeWire device (supports all rates)
    for i in range(audio.get_device_count()):
        info = audio.get_device_info_by_index(i)
        if info['maxInputChannels'] > 0 and 'pipewire' in info['name'].lower():
            try:
                # Test if it supports target rate
                if audio.is_format_supported(
                    target_rate,
                    input_device=i,
                    input_channels=1,
                    input_format=pyaudio.paInt16
                ):
                    if verbose:
                        print(f"[INFO] Using audio device: {info['name']}")
                    return i
            except:
                pass

    # If no PipeWire, try default device
    try:
        default = audio.get_default_input_device_info()
        if audio.is_format_supported(
            target_rate,
            input_device=default['index'],
            input_channels=1,
            input_format=pyaudio.paInt16
        ):
            if verbose:
                print(f"[INFO] Using default audio device: {default['name']}")
            return default['index']
    except:
        pass

    # Last resort: find ANY device that supports target rate
    for i in range(audio.get_device_count()):
        info = audio.get_device_info_by_index(i)
        if info['maxInputChannels'] > 0:
            try:
                if audio.is_format_supported(
                    target_rate,
                    input_device=i,
                    input_channels=1,
                    input_format=pyaudio.paInt16
                ):
                    if verbose:
                        print(f"[INFO] Using audio device: {info['name']}")
                    return i
            except:
                pass

    return None


def list_audio_devices(audio: pyaudio.PyAudio, input_only: bool = True):
    """
    List all available audio devices with their capabilities.

    Args:
        audio: PyAudio instance
        input_only: Only show input devices (default: True)
    """
    print("=" * 70)
    print("AVAILABLE AUDIO DEVICES")
    print("=" * 70)
    print()

    # Get default input device
    try:
        default_input = audio.get_default_input_device_info()
        print(f"Default Input Device:")
        print(f"  Name: {default_input['name']}")
        print(f"  Index: {default_input['index']}")
        print(f"  Max Input Channels: {default_input['maxInputChannels']}")
        print(f"  Default Sample Rate: {default_input['defaultSampleRate']}")
        print()
    except Exception as e:
        print(f"[ERROR] No default input device found: {e}")
        print()

    print(f"Total audio devices: {audio.get_device_count()}")
    print()

    for i in range(audio.get_device_count()):
        info = audio.get_device_info_by_index(i)

        # Skip output-only devices if input_only is True
        if input_only and info['maxInputChannels'] == 0:
            continue

        print(f"Device #{i}: {info['name']}")
        print(f"  Max Input Channels: {info['maxInputChannels']}")
        print(f"  Max Output Channels: {info['maxOutputChannels']}")
        print(f"  Default Sample Rate: {info['defaultSampleRate']}")

        # Test common sample rates for input devices
        if info['maxInputChannels'] > 0:
            test_rates = [8000, 16000, 22050, 24000, 44100, 48000]
            supported_rates = []

            for rate in test_rates:
                try:
                    if audio.is_format_supported(
                        rate,
                        input_device=i,
                        input_channels=1,
                        input_format=pyaudio.paInt16
                    ):
                        supported_rates.append(rate)
                except:
                    pass

            print(f"  Supported Sample Rates: {supported_rates}")
            print(f"  24kHz Supported: {'YES' if 24000 in supported_rates else 'NO'}")
        print()


def open_audio_stream(audio: pyaudio.PyAudio, rate: int = 24000,
                     channels: int = 1, format: int = pyaudio.paInt16,
                     frames_per_buffer: int = 1024, verbose: bool = True):
    """
    Open an audio input stream with automatic device selection.

    Args:
        audio: PyAudio instance
        rate: Sample rate (default: 24000 Hz)
        channels: Number of channels (default: 1 for mono)
        format: Audio format (default: paInt16)
        frames_per_buffer: Buffer size (default: 1024)
        verbose: Print device selection info (default: True)

    Returns:
        PyAudio stream object if successful, None otherwise
    """
    # Find a compatible device
    device_index = find_compatible_device(audio, rate, verbose)

    if device_index is None:
        print(f"\n[ERROR] No audio device supports {rate}Hz sample rate!", file=sys.stderr)
        print("[ERROR] Please ensure PipeWire/PulseAudio is running.", file=sys.stderr)
        return None

    # Open the stream
    try:
        stream = audio.open(
            format=format,
            channels=channels,
            rate=rate,
            input=True,
            input_device_index=device_index,
            frames_per_buffer=frames_per_buffer
        )
        return stream
    except Exception as e:
        print(f"\n[ERROR] Failed to open audio stream: {e}", file=sys.stderr)
        return None
