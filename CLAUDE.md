# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

handy-simple is a minimal, CLI-only speech-to-text tool for Linux that runs as a background daemon. It provides a simple workflow: press Mod+S to start recording, speak, press Mod+S again to stop, and the transcription is automatically pasted into the focused application.

## Build and Run Commands

```bash
# Build release binary
cargo build --release

# Run with logging
RUST_LOG=info ./target/release/handy-simple

# Build and run development version
cargo run

# Check compilation without building
cargo check
```

## Architecture

### Application Flow

1. **Initialization** (`App::new()`):
   - Model directory setup at `~/.handy-simple/models/`
   - Parakeet TDT 0.6B V3 model download (~640MB) if not present
   - Model loaded into memory once at startup and cached for performance
   - VAD model (Silero VAD v4) loaded from same directory

2. **Hotkey Handling** (`App::run()`):
   - Uses `signal-hook` to listen for `SIGUSR1`
   - 300ms debouncing prevents double-triggers
   - Events processed synchronously in the main loop
   - Designed to be triggered by window manager (e.g., `bind = $mainMod, s, exec, pkill -USR1 stt-daemon`)

3. **Recording State Machine** (`toggle_recording()`):
   - `Idle` → `Recording`: Clears buffer, starts audio capture thread
   - `Recording` → `Idle`: Stops capture, processes audio, transcribes, and pastes

4. **Audio Capture** (`start_audio_capture()`):
   - Uses `cpal` for cross-platform audio recording
   - Attempts native 16kHz capture (F32 or I16 format) to avoid resampling
   - Falls back to device default with resampling if needed
   - Separate thread captures audio into shared buffer while state is Recording
   - Supports both F32 and I16 sample formats with conversion

5. **Audio Processing Pipeline** (`process_and_paste()`):
   - **Resampling** (if needed): FFT-based resampling (rubato) from device rate to 16kHz
   - **VAD Filtering**: Silero VAD removes silence/noise with 30ms frame analysis
     - Threshold: 0.2 for speech detection
     - 150ms prefill before first speech
     - 150ms hangover after speech ends
   - **Minimum length**: Pads to 1 second if shorter (Whisper requirement)
   - **Transcription**: Parakeet engine transcribes with segment-level timestamps
   - **Paste**: Clipboard manipulation + Ctrl+Shift+V keyboard simulation

6. **Text Pasting** (`paste_text()`):
   - Saves current clipboard contents using `wl-paste`
   - Writes transcription to clipboard using `wl-copy`
   - Simulates Ctrl+Shift+V using `wtype`
   - Restores original clipboard using `wl-copy` (best effort)

### Key Dependencies

- **transcribe-rs**: Provides ParakeetEngine for speech recognition
  - Model loaded once and cached in Arc<Mutex<Option<ParakeetEngine>>>
  - Uses int8 quantization for speed/memory efficiency
- **vad-rs**: Voice activity detection (Silero VAD v4)
- **cpal**: Cross-platform audio I/O
- **rubato**: FFT-based audio resampling
- **signal-hook**: Handling Unix signals (SIGUSR1)
- **External Tools**:
  - `wl-clipboard`: Clipboard manipulation
  - `wtype`: Wayland keyboard simulation
- **arboard**: Clipboard access

### Threading Model

- Main thread: Signal handling loop with 100ms sleep
- Audio capture thread: Spawned per recording session, self-terminates when state returns to Idle
- Synchronous processing: Transcription and pasting block the hotkey handler to prevent overlapping operations

### Model Management

- **Model URL**: https://blob.handy.computer/parakeet-v3-int8.tar.gz
- **Storage**: `~/.handy-simple/models/parakeet-tdt-0.6b-v3-int8/`
- **VAD Model**: `~/.handy-simple/models/silero_vad_v4.onnx`
- **Download**: Automatic on first run with progress logging
- **Size**: ~640MB total

### Performance Characteristics

- **Idle**: ~700MB RAM (model cached), ~0% CPU
- **Transcription**: High CPU for 0.5-1s, then back to idle
- **Sample rate**: Native 16kHz preferred, resamples if needed (FFT for speed)
- **VAD frame size**: 480 samples (30ms at 16kHz)

## Development Notes

- The ParakeetEngine is never reloaded - it stays in memory for instant response
- Audio format conversion happens inline (I16 → F32 if needed)
- VAD threshold tuned to 0.2 for better sensitivity
- Empty/noise transcriptions filtered (empty, whitespace-only, "[Music]")
- Clipboard restoration is best-effort (non-critical failure)
- No GUI - pure CLI daemon meant for window manager integration (i3, systemd)
