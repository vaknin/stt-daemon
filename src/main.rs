use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{error, info};
use signal_hook::consts::SIGUSR1;
use signal_hook::iterator::Signals;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use transcribe_rs::{
    engines::parakeet::{
        ParakeetEngine, ParakeetInferenceParams, ParakeetModelParams, TimestampGranularity,
    },
    TranscriptionEngine,
};

const MODEL_NAME: &str = "parakeet-tdt-0.6b-v3-int8";
const MODEL_URL: &str = "https://blob.handy.computer/parakeet-v3-int8.tar.gz";
const VAD_MODEL_NAME: &str = "silero_vad_v4.onnx";
const SAMPLE_RATE: u32 = 16000;

// Recording state
#[derive(Clone, Copy, PartialEq)]
enum RecordingState {
    Idle,
    Recording,
}

struct App {
    state: Arc<Mutex<RecordingState>>,
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    audio_sample_rate: Arc<Mutex<u32>>,
    parakeet_engine: Arc<Mutex<Option<ParakeetEngine>>>,
    vad_model_path: PathBuf,
}

impl App {
    fn new() -> Result<Self> {
        // Set up model directory
        let model_dir = PathBuf::from_str("models/").unwrap();
        fs::create_dir_all(&model_dir)?;
        let model_path = model_dir.join(MODEL_NAME);
        let vad_model_path = model_dir.join(VAD_MODEL_NAME);

        // Download and extract model if needed
        if !model_path.exists() {
            info!("Downloading Parakeet model...");
            Self::download_and_extract_model(&model_dir)?;
            info!("Model downloaded and extracted successfully!");
        }

        // Load Parakeet model once
        info!("Loading Parakeet model...");
        let mut engine = ParakeetEngine::new();
        engine
            .load_model_with_params(&model_path, ParakeetModelParams::int8())
            .map_err(|e| anyhow::anyhow!("Failed to load Parakeet model: {}", e))?;
        info!("Model loaded successfully!");

        Ok(Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
            audio_sample_rate: Arc::new(Mutex::new(48000)),
            parakeet_engine: Arc::new(Mutex::new(Some(engine))),
            vad_model_path,
        })
    }

    fn download_and_extract_model(model_dir: &PathBuf) -> Result<()> {
        use flate2::read::GzDecoder;
        use std::io::Write;
        use tar::Archive;

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?;

        info!("Downloading from {}", MODEL_URL);
        let mut response = client.get(MODEL_URL).send()?;
        let tar_gz_path = model_dir.join("parakeet.tar.gz");
        let mut file = fs::File::create(&tar_gz_path)?;

        let total_size = response.content_length().unwrap_or(0);
        info!("Downloading {} MB...", total_size / 1_000_000);

        let mut downloaded = 0u64;
        let mut buffer = [0; 8192];

        loop {
            let bytes_read = response.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            file.write_all(&buffer[..bytes_read])?;
            downloaded += bytes_read as u64;

            if total_size > 0 {
                let progress = (downloaded as f64 / total_size as f64 * 100.0) as u64;
                if downloaded % 5_000_000 < 8192 {
                    info!("Progress: {}%", progress);
                }
            }
        }

        file.flush()?;
        drop(file);

        // Extract tar.gz
        info!("Extracting archive...");
        let tar_gz = fs::File::open(&tar_gz_path)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        archive.unpack(model_dir)?;

        // Clean up tar.gz
        fs::remove_file(&tar_gz_path)?;
        info!("Extraction complete!");

        Ok(())
    }

    fn toggle_recording(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        match *state {
            RecordingState::Idle => {
                info!("State: Idle -> Recording");
                *state = RecordingState::Recording;
                self.audio_buffer.lock().unwrap().clear();
                self.start_audio_capture()?;
            }
            RecordingState::Recording => {
                info!("State: Recording -> Idle");
                *state = RecordingState::Idle;
                drop(state);
                self.process_and_paste()?;
            }
        }
        Ok(())
    }

    fn get_preferred_config(&self, device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
        let supported_configs = device
            .supported_input_configs()
            .context("Failed to get supported configs")?;

        for config_range in supported_configs {
            let format = config_range.sample_format();
            if (format == cpal::SampleFormat::F32 || format == cpal::SampleFormat::I16)
                && config_range.min_sample_rate().0 <= SAMPLE_RATE
                && config_range.max_sample_rate().0 >= SAMPLE_RATE
            {
                return Ok(config_range.with_sample_rate(cpal::SampleRate(SAMPLE_RATE)));
            }
        }

        Ok(device.default_input_config()?)
    }

    fn start_audio_capture(&self) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No input device available")?;

        info!("Using input device: {}", device.name().unwrap_or_default());

        let config = self.get_preferred_config(&device)?;
        let sample_rate = config.sample_rate().0;
        *self.audio_sample_rate.lock().unwrap() = sample_rate;

        let audio_buffer = Arc::clone(&self.audio_buffer);
        let state = Arc::clone(&self.state);

        thread::spawn(move || {
            let err_fn = |err| error!("Audio stream error: {}", err);

            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => {
                    let config: cpal::StreamConfig = config.into();
                    let audio_buffer_f32 = Arc::clone(&audio_buffer);
                    let state_f32 = Arc::clone(&state);
                    device
                        .build_input_stream(
                            &config,
                            move |data: &[f32], _: &_| {
                                if *state_f32.lock().unwrap() == RecordingState::Recording {
                                    audio_buffer_f32.lock().unwrap().extend_from_slice(data);
                                }
                            },
                            err_fn,
                            None,
                        )
                        .unwrap()
                }
                cpal::SampleFormat::I16 => {
                    let config: cpal::StreamConfig = config.into();
                    let audio_buffer_i16 = Arc::clone(&audio_buffer);
                    let state_i16 = Arc::clone(&state);
                    device
                        .build_input_stream(
                            &config,
                            move |data: &[i16], _: &_| {
                                if *state_i16.lock().unwrap() == RecordingState::Recording {
                                    let f32_data: Vec<f32> =
                                        data.iter().map(|&s| s as f32 / 32768.0).collect();
                                    audio_buffer_i16.lock().unwrap().extend(f32_data);
                                }
                            },
                            err_fn,
                            None,
                        )
                        .unwrap()
                }
                _ => panic!("Unsupported sample format"),
            };

            stream.play().unwrap();

            // Keep stream alive while recording
            loop {
                if *state.lock().unwrap() == RecordingState::Idle {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }

            // Explicitly pause and drop the stream to clean up resources
            let _ = stream.pause();
            drop(stream);
        });

        Ok(())
    }

    fn process_and_paste(&self) -> Result<()> {
        let audio_data = self.audio_buffer.lock().unwrap().clone();
        let sample_rate = *self.audio_sample_rate.lock().unwrap();

        info!(
            "Processing audio: {} samples at {}Hz",
            audio_data.len(),
            sample_rate
        );

        if audio_data.is_empty() {
            info!("Audio buffer is empty");
            return Ok(());
        }

        let resampled = if sample_rate != SAMPLE_RATE {
            info!("Resampling from {}Hz to {}Hz...", sample_rate, SAMPLE_RATE);
            self.resample_audio(&audio_data, sample_rate, SAMPLE_RATE)?
        } else {
            audio_data
        };

        let mut filtered = self.apply_vad(&resampled)?;
        info!("After VAD: {} samples", filtered.len());

        if filtered.is_empty() {
            info!("No speech detected (VAD filtered everything)");
            return Ok(());
        }

        let min_samples = SAMPLE_RATE as usize;
        if filtered.len() < min_samples {
            filtered.resize(min_samples, 0.0);
        }

        info!("Transcribing...");
        let text = self.transcribe(&filtered)?;
        info!("Transcription result: '{}'", text);

        if text.is_empty() || text == "[Music]" || text.trim().is_empty() {
            info!("Transcription ignored (empty or filtered)");
            return Ok(());
        }

        let text_with_space = format!("{} ", text);
        info!("Pasting text...");
        self.paste_text(&text_with_space)?;
        info!("Paste complete!");

        Ok(())
    }

    fn resample_audio(&self, audio: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
        use rubato::{FftFixedIn, Resampler};

        // Use FFT resampler for better speed (5-10x faster than Sinc)
        let chunk_size = 1024;
        let mut resampler = FftFixedIn::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            chunk_size,
            1, // sub_chunks
            1, // channels
        )?;

        let mut output = Vec::new();
        let mut pos = 0;

        // Process in chunks
        while pos + chunk_size <= audio.len() {
            let chunk = &audio[pos..pos + chunk_size];
            let resampled = resampler.process(&[chunk.to_vec()], None)?;
            output.extend_from_slice(&resampled[0]);
            pos += chunk_size;
        }

        // Process remaining samples
        if pos < audio.len() {
            let mut last_chunk = audio[pos..].to_vec();
            last_chunk.resize(chunk_size, 0.0);
            let resampled = resampler.process(&[last_chunk], None)?;
            let valid_samples =
                ((audio.len() - pos) as f64 * to_rate as f64 / from_rate as f64) as usize;
            output.extend_from_slice(&resampled[0][..valid_samples]);
        }

        Ok(output)
    }

    fn apply_vad(&self, audio: &[f32]) -> Result<Vec<f32>> {
        use vad_rs::Vad;

        let mut vad = Vad::new(self.vad_model_path.to_str().unwrap(), SAMPLE_RATE as usize)
            .map_err(|e| anyhow::anyhow!("Failed to create VAD: {}", e))?;

        let frame_size = 480;
        let threshold = 0.2;
        let prefill_frames = 5;
        let hangover_frames = 5;

        let mut speech_detected = Vec::new();
        let total_frames = audio.len().div_ceil(frame_size);

        for chunk in audio.chunks(frame_size) {
            let padded: Vec<f32> = if chunk.len() < frame_size {
                let mut p = chunk.to_vec();
                p.resize(frame_size, 0.0);
                p
            } else {
                chunk.to_vec()
            };

            let result = vad
                .compute(&padded)
                .map_err(|e| anyhow::anyhow!("VAD computation error: {}", e))?;

            speech_detected.push(result.prob > threshold);
        }

        let mut start_frame = None;
        let mut end_frame = None;

        for (i, &is_speech) in speech_detected.iter().enumerate() {
            if is_speech {
                if start_frame.is_none() {
                    start_frame = Some(i.saturating_sub(prefill_frames));
                }
                end_frame = Some((i + hangover_frames).min(total_frames - 1));
            }
        }

        let mut output = Vec::new();
        if let (Some(start), Some(end)) = (start_frame, end_frame) {
            let start_sample = start * frame_size;
            let end_sample = ((end + 1) * frame_size).min(audio.len());
            output.extend_from_slice(&audio[start_sample..end_sample]);
        }

        Ok(output)
    }

    fn paste_text(&self, text: &str) -> Result<()> {
        // Save current clipboard using wl-paste
        let old_clipboard = Command::new("wl-paste")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok()
                } else {
                    None
                }
            })
            .unwrap_or_default();

        // Write text to clipboard using wl-copy
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .context("Failed to spawn wl-copy")?;

        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(text.as_bytes())?;
        }

        child.wait().context("Failed to wait for wl-copy")?;

        thread::sleep(Duration::from_millis(100));

        // Send Ctrl+Shift+V using wtype (Wayland keyboard simulation)
        // Press modifiers, press+release v, then release modifiers
        Command::new("wtype")
            .args([
                "-M", "ctrl", "-M", "shift", "-P", "v", "-p", "v", "-m", "shift", "-m", "ctrl",
            ])
            .status()
            .context("Failed to execute wtype")?;

        thread::sleep(Duration::from_millis(100));

        // Restore clipboard using wl-copy (ignore errors as clipboard manager may timeout)
        if !old_clipboard.is_empty() {
            let child = Command::new("wl-copy").stdin(Stdio::piped()).spawn();

            if let Ok(mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(old_clipboard.as_bytes());
                }
                let _ = child.wait();
            }
        }

        Ok(())
    }

    fn transcribe(&self, audio: &[f32]) -> Result<String> {
        let params = ParakeetInferenceParams {
            timestamp_granularity: TimestampGranularity::Segment,
        };

        let mut engine_lock = self.parakeet_engine.lock().unwrap();
        let engine = engine_lock
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ParakeetEngine not available"))?;

        let result = engine
            .transcribe_samples(audio.to_vec(), Some(params))
            .map_err(|e| anyhow::anyhow!("Transcription failed: {}", e))?;

        Ok(result.text.trim().to_string())
    }

    fn run(&self) -> Result<()> {
        info!("Speech-to-text daemon started");

        let mut signals = Signals::new(&[SIGUSR1]).context("Failed to register signal handler")?;

        let mut last_process_time = Instant::now() - Duration::from_secs(1);

        for sig in signals.forever() {
            if sig == SIGUSR1 {
                info!("Received SIGUSR1 signal");
                if last_process_time.elapsed() < Duration::from_millis(300) {
                    info!(
                        "Debouncing signal (elapsed: {:?})",
                        last_process_time.elapsed()
                    );
                    continue;
                }

                info!("Toggling recording state...");
                if let Err(e) = self.toggle_recording() {
                    error!("Error toggling recording: {}", e);
                }

                last_process_time = Instant::now();
            }
        }

        Ok(())
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let app = App::new()?;
    app.run()?;

    Ok(())
}
