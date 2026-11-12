//! Silero VAD로 말하는 구간을 찾고, 그 구간을 sherpa-onnx 인식기로 다시 디코딩한다.
//!
//! 인식 모델(Moonshine, 언어별)은 스트리밍 모델이 아니다. 말하는 동안 지금까지의 구간을
//! 0.2초마다 다시 디코딩해 부분 결과를 내고, 구간이 끝나면 확정 결과를 낸다.
//! 구간은 침묵 0.4초 또는 최대 6초에서 끝난다. Moonshine은 토큰 시각을 주지 않아 문장부호 위치에서
//! 오디오를 자를 수 없으므로 문장부호로는 끊지 않는다(docs/PLAN.md "0단계 결과").

use crate::audio::SAMPLE_RATE;
use crate::Lang;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, VadModelConfig, VoiceActivityDetector};
use std::path::Path;
use std::time::{Duration, Instant};

const VAD_MODEL: &str = "models/silero_vad.onnx";
pub const VAD_WINDOW: usize = 512; // Silero VAD 입력 단위(샘플)
const PRE_ROLL: usize = 16 * VAD_WINDOW; // 말 시작 직전 약 0.5초를 남겨 둔다.
const PARTIAL_INTERVAL: Duration = Duration::from_millis(200);
const MIN_SILENCE: f32 = 0.4;
const MIN_SPEECH: f32 = 0.25;
pub const MAX_SPEECH: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    MoonshineTiny,
    MoonshineBase,
}

pub struct Recognizer {
    inner: OfflineRecognizer,
}


fn model_file(path: String) -> Result<String, String> {
    if Path::new(&path).is_file() {
        Ok(path)
    } else {
        Err(format!("인식 모델 파일이 없습니다: {path}"))
    }
}

impl Recognizer {
    pub fn load(model: Model, lang: Lang) -> Result<Self, String> {
        let size = match model {
            Model::MoonshineTiny => "tiny",
            Model::MoonshineBase => "base",
        };
        let dir = format!("models/sherpa-onnx-moonshine-{size}-{}-quantized-2026-02-27", lang.code());
        let mut c = OfflineRecognizerConfig::default();
        c.model_config.moonshine.encoder = Some(model_file(format!("{dir}/encoder_model.ort"))?);
        c.model_config.moonshine.merged_decoder = Some(model_file(format!("{dir}/decoder_model_merged.ort"))?);
        c.model_config.tokens = Some(model_file(format!("{dir}/tokens.txt"))?);
        c.model_config.num_threads = 2;
        let inner = OfflineRecognizer::create(&c).ok_or("음성 인식 모델을 불러오지 못했습니다.")?;
        Ok(Self { inner })
    }

    pub fn decode(&self, samples: &[f32]) -> String {
        let stream = self.inner.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.inner.decode(&stream);
        // 토크나이저의 단어 시작 표시(▁)가 결과에 섞여 나오는 경우가 있다.
        stream
            .get_result()
            .map(|r| tidy_spaces(r.text.replace('▁', " ").trim()))
            .unwrap_or_default()
    }
}

/// Moonshine 일본어 모델은 가끔 글자마다 공백을 넣는다(`国 が 花 と …`). 양쪽이 모두 일본어·한자·전각 문자인
/// 공백은 지운다. 영어 단어 사이 공백은 그대로 둔다.
pub fn tidy_spaces(s: &str) -> String {
    fn cjk(c: char) -> bool {
        matches!(c as u32, 0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    }
    let chars: Vec<char> = s.chars().collect();
    let has_cjk = chars.iter().any(|c| cjk(*c));
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = out.chars().last();
            let next = chars[i + 1..].iter().find(|c| **c != ' ');
            let between_cjk = prev.is_some_and(|p| cjk(p) || p.is_ascii_digit()) && next.is_some_and(|n| cjk(*n) || n.is_ascii_digit());
            if (between_cjk && has_cjk) || prev == Some(' ') {
                continue;
            }
        }
        out.push(c);
    }
    out
}

pub enum Event {
    /// 말하는 중인 구간의 지금까지 텍스트. `start`는 말이 시작된 시각(추정).
    Partial { seg: u64, text: String, start: Instant },
    /// 구간 확정. `end`는 말이 끝난 시각(추정).
    Final { seg: u64, text: String, start: Instant, end: Instant },
}

pub struct Segmenter {
    recognizer: std::sync::Arc<Recognizer>,
    vad: VoiceActivityDetector,
    /// 지금 말하는 구간의 샘플(말 시작 전에는 직전 PRE_ROLL만 유지)
    buffer: Vec<f32>,
    fed: usize, // buffer 중 VAD에 넣은 샘플 수
    speaking: bool,
    start: Instant,
    last_partial_at: Instant,
    partial: String,
    seg: u64,
}

/// 침묵 0.4초 또는 최대 6초에서 구간을 끝내는 Silero VAD
pub fn create_vad() -> Result<VoiceActivityDetector, String> {
    let mut c = VadModelConfig::default();
    c.silero_vad.model = Some(model_file(VAD_MODEL.into())?);
    c.silero_vad.threshold = 0.5;
    c.silero_vad.min_silence_duration = MIN_SILENCE;
    c.silero_vad.min_speech_duration = MIN_SPEECH;
    c.silero_vad.max_speech_duration = MAX_SPEECH;
    c.silero_vad.window_size = VAD_WINDOW as i32;
    c.sample_rate = SAMPLE_RATE as i32;
    VoiceActivityDetector::create(&c, 30.0).ok_or_else(|| "VAD 모델을 불러오지 못했습니다.".into())
}

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s.max(0.0))
}

impl Segmenter {
    pub fn new(recognizer: std::sync::Arc<Recognizer>) -> Result<Self, String> {
        let vad = create_vad()?;
        let now = Instant::now();
        Ok(Self {
            recognizer,
            vad,
            buffer: Vec::new(),
            fed: 0,
            speaking: false,
            start: now,
            last_partial_at: now,
            partial: String::new(),
            seg: 0,
        })
    }

    /// 16kHz 모노 샘플을 넣는다. 결과는 `out`으로 나온다.
    pub fn push(&mut self, samples: &[f32], out: &mut impl FnMut(Event)) {
        self.buffer.extend_from_slice(samples);
        while self.fed + VAD_WINDOW <= self.buffer.len() {
            self.vad.accept_waveform(&self.buffer[self.fed..self.fed + VAD_WINDOW]);
            self.fed += VAD_WINDOW;
            if !self.speaking && self.vad.detected() {
                self.speaking = true;
                self.start = Instant::now() - secs(MIN_SPEECH);
                self.last_partial_at = Instant::now() - PARTIAL_INTERVAL;
            }
        }

        if !self.speaking && self.buffer.len() > PRE_ROLL {
            let cut = self.buffer.len() - PRE_ROLL;
            self.buffer.drain(..cut);
            self.fed = self.fed.saturating_sub(cut);
        }

        if !self.vad.is_empty() {
            // VAD가 구간을 끝냈다(침묵 또는 최대 길이). 샘플은 VAD 구간 대신 말 시작 직전부터 담은 우리 버퍼를 쓴다.
            self.vad.clear();
            let silence = if self.vad.detected() { 0.0 } else { MIN_SILENCE };
            let n = self.fed;
            self.finish(n, Instant::now() - secs(silence), out);
            self.speaking = self.vad.detected();
            return;
        }

        if self.speaking && self.last_partial_at.elapsed() >= PARTIAL_INTERVAL {
            self.last_partial_at = Instant::now();
            let text = self.recognizer.decode(&self.buffer);
            if text != self.partial {
                self.partial = text.clone();
                out(Event::Partial { seg: self.seg, text, start: self.start });
            }
        }
    }

    /// 버퍼 앞 `n` 샘플을 디코딩해 확정한다.
    fn finish(&mut self, n: usize, end: Instant, out: &mut impl FnMut(Event)) {
        let text = self.recognizer.decode(&self.buffer[..n]);
        if !text.is_empty() {
            out(Event::Final { seg: self.seg, text, start: self.start, end });
        } else if !self.partial.is_empty() {
            out(Event::Partial { seg: self.seg, text: String::new(), start: self.start });
        }
        self.next_segment(n);
        self.start = Instant::now();
    }

    fn next_segment(&mut self, cut: usize) {
        self.buffer.drain(..cut);
        self.fed = self.fed.saturating_sub(cut);
        self.partial.clear();
        self.seg += 1;
    }

    /// 정지할 때 말하던 구간도 확정한다.
    pub fn flush(&mut self, out: &mut impl FnMut(Event)) {
        if self.speaking {
            let n = self.buffer.len();
            self.finish(n, Instant::now(), out);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tidy_spaces_joins_japanese_only() {
        use super::tidy_spaces;
        assert_eq!(tidy_spaces("国 が 花 と の た め に 5 0 円"), "国が花とのために50円");
        assert_eq!(tidy_spaces("Ask not what  your country"), "Ask not what your country");
        assert_eq!(tidy_spaces("東京 Tower へ"), "東京 Tower へ");
    }
}
