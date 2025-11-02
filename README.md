# stt-daemon

A minimal, CLI-only speech-to-text tool for Linux. Press a shortcut, speak, press again, and it pastes the transcription.

## Features

- **No GUI**: Runs as a background daemon
- **Simple workflow**: Mod+S to start recording, Mod+S to stop and paste
- **Parakeet TDT 0.6B V3**: Fast and accurate inference (~0.5-1s per transcription)
- **VAD filtering**: Silero VAD removes silence and noise for better accuracy
- **Auto model download**: Downloads model on first run (~640MB)
- **Direct paste**: Uses clipboard + Ctrl+Shift+V to paste text directly into focused application

## Requirements

- Linux
- Rust (latest stable)
- Microphone

## Installation

```bash
cd stt-daemon
cargo build --release
```

## Usage

1. Run the binary:
```bash
RUST_LOG=info ./target/release/stt-daemon
```

2. Press **Mod+S** (Super/Windows key + S) to start recording
3. Speak your text
4. Press **Mod+S** again to stop recording
5. The transcription will be automatically pasted into your focused window

## How it works

1. **Global hotkey**: Registers Mod+S shortcut using global-hotkey (properly consumes the key)
2. **Audio capture**: Records audio from default microphone at 16kHz using cpal
3. **VAD filtering**: Silero VAD removes silence and noise, keeping only speech
4. **Transcription**: Uses Parakeet TDT 0.6B V3 via transcribe-rs (cached for performance)
5. **Paste**: Uses clipboard + Ctrl+Shift+V via enigo (restores clipboard after)

## Model Storage

The Parakeet model is downloaded to `~/.stt-daemon/models/` on first run (~640MB).

## Running as a Daemon

To run stt-daemon automatically on startup, add to your i3 config (`~/.config/i3/config`):

```
exec --no-startup-id /path/to/stt-daemon/target/release/stt-daemon
```

Or create a systemd user service (`~/.config/systemd/user/stt-daemon.service`):

```ini
[Unit]
Description=STT Daemon Speech-to-Text
After=graphical-session.target

[Service]
ExecStart=/path/to/stt-daemon/target/release/stt-daemon
Restart=on-failure
Environment="RUST_LOG=info"

[Install]
WantedBy=default.target
```

Then enable it:
```bash
systemctl --user enable stt-daemon
systemctl --user start stt-daemon
```

## Resource Usage

**Idle (waiting for hotkey):**
- CPU: ~0% (sleeps 100ms between checks)
- RAM: ~700MB (Parakeet model loaded and cached)

**During transcription:**
- CPU: High for ~0.5-1 second (model inference)
- Then back to idle

The model stays loaded in memory for instant response when you press the hotkey.

## Permissions

On Linux, you may need to run with appropriate permissions for:
- Global keyboard hotkeys (global-hotkey)
- Audio device access (cpal)
- Simulating keyboard input (enigo)
- Clipboard access (arboard)

## License

MIT
