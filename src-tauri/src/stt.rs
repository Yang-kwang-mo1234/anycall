//! 마이크 입력 → Silero VAD → Moonshine 인식 → 프론트 이벤트. 세 OS 공용.
//!
//! Moonshine은 스트리밍 모델이 아니다. VAD가 말하는 중이라고 판단하면 지금까지의 구간을
//! 0.2초마다 다시 디코딩해 `stt-partial`로 보내고, VAD가 구간을 끝내면 그 구간을 디코딩해
//! `stt-final`로 보낸다.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use serde::Serialize;
use sherpa_onnx::{
    LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, VadModelConfig,
    VoiceActivityDetector,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const MOONSHINE_DIR: &str = "models/sherpa-onnx-moonshine-tiny-ko-quantized-2026-02-27";
const VAD_MODEL: &str = "models/silero_vad.onnx";
const SAMPLE_RATE: i32 = 16000;
const VAD_WINDOW: usize = 512; // Silero VAD 입력 단위(샘플)
const PRE_ROLL_WINDOWS: usize = 10; // 말 시작 직전 약 0.3초를 남겨 둔다.
const PARTIAL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct Stt {
    session: Mutex<Option<Session>>,
    /// 모델 로딩은 시간이 걸리므로 처음 한 번만 만든다. 실패하면 다음 시작 때 다시 시도한다.
    recognizer: Arc<Mutex<Option<Arc<OfflineRecognizer>>>>,
}

struct Session {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

#[derive(Clone, Serialize)]
struct SttError {
    code: &'static str,
    message: String,
}

fn err(code: &'static str, message: impl Into<String>) -> SttError {
    SttError {
        code,
        message: message.into(),
    }
}

enum Msg {
    Samples(Vec<f32>),
    Error(String),
}

impl Stt {
    pub fn start(&self, app: AppHandle) {
        let mut session = self.session.lock().unwrap();
        if session.as_ref().is_some_and(|s| !s.handle.is_finished()) {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let cache = self.recognizer.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            if let Err(e) = listen(&app, &cache, &thread_stop) {
                let _ = app.emit("stt-error", e);
            }
            let _ = app.emit("stt-state", false);
        });
        *session = Some(Session { stop, handle });
    }

    pub fn stop(&self) {
        if let Some(s) = self.session.lock().unwrap().take() {
            s.stop.store(true, Ordering::Relaxed);
            let _ = s.handle.join();
        }
    }
}

fn model_file(app: &AppHandle, rel: &str) -> Result<String, SttError> {
    let dir: PathBuf = app
        .path()
        .resource_dir()
        .map_err(|e| err("model-missing", e.to_string()))?;
    let p = dir.join(rel);
    if !p.is_file() {
        return Err(err(
            "model-missing",
            format!("모델 파일이 없습니다: {}", p.display()),
        ));
    }
    Ok(p.to_string_lossy().into_owned())
}

fn load_recognizer(app: &AppHandle) -> Result<OfflineRecognizer, SttError> {
    let mut c = OfflineRecognizerConfig::default();
    c.model_config.moonshine.encoder = Some(model_file(
        app,
        &format!("{MOONSHINE_DIR}/encoder_model.ort"),
    )?);
    c.model_config.moonshine.merged_decoder = Some(model_file(
        app,
        &format!("{MOONSHINE_DIR}/decoder_model_merged.ort"),
    )?);
    c.model_config.tokens = Some(model_file(app, &format!("{MOONSHINE_DIR}/tokens.txt"))?);
    c.model_config.num_threads = 2;
    OfflineRecognizer::create(&c)
        .ok_or_else(|| err("model-load", "음성인식 모델을 불러오지 못했습니다."))
}

fn create_vad(app: &AppHandle) -> Result<VoiceActivityDetector, SttError> {
    let mut c = VadModelConfig::default();
    c.silero_vad.model = Some(model_file(app, VAD_MODEL)?);
    c.silero_vad.threshold = 0.5;
    c.silero_vad.min_silence_duration = 0.5; // 이만큼 조용하면 문장 끝
    c.silero_vad.min_speech_duration = 0.25;
    c.silero_vad.max_speech_duration = 10.0; // 더 길면 끊어서 확정한다.
    c.silero_vad.window_size = VAD_WINDOW as i32;
    c.sample_rate = SAMPLE_RATE;
    VoiceActivityDetector::create(&c, 30.0)
        .ok_or_else(|| err("model-load", "VAD 모델을 불러오지 못했습니다."))
}

fn decode(recognizer: &OfflineRecognizer, samples: &[f32]) -> String {
    let stream = recognizer.create_stream();
    stream.accept_waveform(SAMPLE_RATE, samples);
    recognizer.decode(&stream);
    // 토크나이저의 단어 시작 표시(▁)가 결과에 섞여 나오는 경우가 있다.
    stream
        .get_result()
        .map(|r| r.text.replace('▁', " ").trim().to_string())
        .unwrap_or_default()
}

fn listen(
    app: &AppHandle,
    cache: &Mutex<Option<Arc<OfflineRecognizer>>>,
    stop: &AtomicBool,
) -> Result<(), SttError> {
    let recognizer = {
        let mut cache = cache.lock().unwrap();
        if cache.is_none() {
            *cache = Some(Arc::new(load_recognizer(app)?));
        }
        cache.clone().unwrap()
    };
    let vad = create_vad(app)?;

    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| err("no-mic", "마이크를 찾을 수 없습니다."))?;
    let supported = device.default_input_config().map_err(mic_error)?;
    let mic_rate = supported.sample_rate().0 as i32;
    let resampler = if mic_rate == SAMPLE_RATE {
        None
    } else {
        Some(
            LinearResampler::create(mic_rate, SAMPLE_RATE).ok_or_else(|| {
                err(
                    "mic-unavailable",
                    format!("{mic_rate}Hz 입력을 변환할 수 없습니다."),
                )
            })?,
        )
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let config = supported.config();
    let input = match supported.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, tx),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, tx),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, tx),
        other => {
            return Err(err(
                "mic-unavailable",
                format!("지원하지 않는 오디오 형식: {other:?}"),
            ))
        }
    }
    .map_err(mic_error)?;
    input.play().map_err(mic_error)?;
    let _ = app.emit("stt-state", true);

    // buffer: 지금 말하는 구간(말 시작 전에는 직전 PRE_ROLL_WINDOWS 만큼만 유지)
    let mut buffer = Vec::<f32>::new();
    let mut fed = 0; // buffer 중 VAD에 넣은 샘플 수
    let mut speaking = false;
    let mut last_partial_at = Instant::now();
    let mut partial = String::new();

    while !stop.load(Ordering::Relaxed) {
        let samples = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Samples(s)) => s,
            Ok(Msg::Error(e)) => return Err(err("mic-unavailable", format!("마이크 오류: {e}"))),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match &resampler {
            Some(r) => buffer.extend(r.resample(&samples, false)),
            None => buffer.extend(samples),
        }

        while fed + VAD_WINDOW <= buffer.len() {
            vad.accept_waveform(&buffer[fed..fed + VAD_WINDOW]);
            fed += VAD_WINDOW;
            if !speaking && vad.detected() {
                speaking = true;
                last_partial_at = Instant::now();
            }
        }

        if !speaking && buffer.len() > PRE_ROLL_WINDOWS * VAD_WINDOW {
            let cut = buffer.len() - PRE_ROLL_WINDOWS * VAD_WINDOW;
            buffer.drain(..cut);
            fed = fed.saturating_sub(cut);
        }

        if speaking && last_partial_at.elapsed() >= PARTIAL_INTERVAL {
            let text = decode(&recognizer, &buffer);
            if text != partial {
                let _ = app.emit("stt-partial", &text);
                partial = text;
            }
            last_partial_at = Instant::now();
        }

        if !vad.is_empty() {
            emit_segments(app, &vad, &recognizer);
            buffer.clear();
            fed = 0;
            speaking = false;
            partial.clear();
        }
    }
    drop(input); // 마이크를 놓는다.

    // 정지할 때 말하던 문장도 확정한다.
    vad.flush();
    emit_segments(app, &vad, &recognizer);
    Ok(())
}

fn emit_segments(app: &AppHandle, vad: &VoiceActivityDetector, recognizer: &OfflineRecognizer) {
    while let Some(segment) = vad.front() {
        vad.pop();
        let text = decode(recognizer, segment.samples());
        if text.is_empty() {
            let _ = app.emit("stt-partial", "");
        } else {
            let _ = app.emit("stt-final", &text);
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: mpsc::Sender<Msg>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let err_tx = tx.clone();
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mono = data
                .chunks(channels)
                .map(|f| f.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32)
                .collect();
            let _ = tx.send(Msg::Samples(mono));
        },
        move |e| {
            let _ = err_tx.send(Msg::Error(e.to_string()));
        },
        None,
    )
}

fn mic_error(e: impl std::fmt::Display) -> SttError {
    err(
        "mic-unavailable",
        format!("마이크를 열 수 없습니다. 마이크 연결과 OS의 마이크 권한을 확인하세요. ({e})"),
    )
}
