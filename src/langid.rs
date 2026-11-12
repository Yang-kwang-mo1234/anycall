//! 동영상 파일의 음성 언어를 판별한다(sherpa-onnx, Whisper tiny 다국어 모델).
//!
//! 오프닝 곡이나 한 장면의 외국어 대사에 속지 않도록 파일의 세 지점(처음, 30%, 60%)에서
//! 말소리를 모아 각각 판별하고 다수결로 정한다.

use crate::analyze::FileSegmenter;
use crate::audio::SAMPLE_RATE;
use crate::media_audio::MediaAudio;
use sherpa_onnx::{SpokenLanguageIdentification, SpokenLanguageIdentificationConfig};

/// Whisper 크기. ANYCALL_LANGID_MODEL(tiny|base)로 바꿀 수 있다.
fn model_size() -> String {
    std::env::var("ANYCALL_LANGID_MODEL").unwrap_or_else(|_| "tiny".into())
}
/// 지점마다 모을 말소리 길이(초)
const SPEECH_PER_POINT: f64 = 8.0;
/// 지점마다 말소리를 찾아 읽어 나갈 최대 길이(초)
const SCAN_PER_POINT: f64 = 90.0;
const POINTS: [f64; 3] = [0.0, 0.3, 0.6];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spoken {
    En,
    Ja,
    Ko,
    /// 그 밖의 언어(Whisper 언어 코드)
    Other(String),
    /// 말소리를 찾지 못했다.
    NoSpeech,
}

impl Spoken {
    fn from_code(code: &str) -> Self {
        match code {
            "en" => Spoken::En,
            "ja" => Spoken::Ja,
            "ko" => Spoken::Ko,
            other => Spoken::Other(other.to_string()),
        }
    }
}

pub struct LangId {
    inner: SpokenLanguageIdentification,
}

/// 지점별 판별 결과(진단용)
pub struct Detail {
    pub at: f64,
    pub speech: f64,
    pub lang: Option<String>,
}

impl LangId {
    pub fn load() -> Result<Self, String> {
        let size = model_size();
        let file = |name: &str| {
            let p = format!("models/sherpa-onnx-whisper-{size}/{size}-{name}");
            if std::path::Path::new(&p).is_file() {
                Ok(p)
            } else {
                Err(format!("언어 판별 모델 파일이 없습니다: {p}"))
            }
        };
        let mut c = SpokenLanguageIdentificationConfig::default();
        c.whisper.encoder = Some(file("encoder.int8.onnx")?);
        c.whisper.decoder = Some(file("decoder.int8.onnx")?);
        c.num_threads = 2;
        let inner = SpokenLanguageIdentification::create(&c).ok_or("언어 판별 모델을 불러오지 못했습니다.")?;
        Ok(Self { inner })
    }

    /// 16kHz 모노 말소리 하나의 언어 코드
    pub fn identify(&self, samples: &[f32]) -> Option<String> {
        let stream = self.inner.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.inner.compute(&stream).map(|r| r.lang).filter(|l| !l.is_empty())
    }

    /// 파일의 음성 언어를 판별한다.
    pub fn detect_file(&self, path: &str) -> Result<(Spoken, Vec<Detail>), String> {
        let mut media = MediaAudio::open(path)?;
        let duration = media.duration.unwrap_or(0.0);
        let mut details = Vec::new();
        for p in POINTS {
            let at = duration * p;
            if p > 0.0 && (duration <= 0.0 || media.seek(at).is_err()) {
                continue;
            }
            let speech = collect_speech(&mut media)?;
            let secs = speech.len() as f64 / SAMPLE_RATE as f64;
            let lang = if secs >= 1.0 { self.identify(&speech) } else { None };
            details.push(Detail { at, speech: secs, lang });
        }
        Ok((vote(&details), details))
    }
}

/// 지금 위치부터 말소리 구간을 SPEECH_PER_POINT초만큼 모은다.
fn collect_speech(media: &mut MediaAudio) -> Result<Vec<f32>, String> {
    let mut seg: Option<FileSegmenter> = None;
    let mut speech = Vec::new();
    let mut scanned = 0.0;
    let want = (SPEECH_PER_POINT * SAMPLE_RATE as f64) as usize;
    while speech.len() < want && scanned < SCAN_PER_POINT {
        let Some(chunk) = media.read()? else { break };
        scanned += chunk.samples.len() as f64 / SAMPLE_RATE as f64;
        let s = match &mut seg {
            Some(s) => s,
            None => seg.insert(FileSegmenter::new(chunk.start)?),
        };
        s.push(&chunk.samples, &mut |g| speech.extend(g.samples));
    }
    if speech.len() < want {
        if let Some(s) = &mut seg {
            s.flush(&mut |g| speech.extend(g.samples));
        }
    }
    speech.truncate(want);
    Ok(speech)
}

/// 가장 많이 나온 언어. 같으면 앞 지점의 결과를 따른다.
fn vote(details: &[Detail]) -> Spoken {
    let langs: Vec<&str> = details.iter().filter_map(|d| d.lang.as_deref()).collect();
    let best = langs
        .iter()
        .copied()
        .max_by_key(|l| (langs.iter().filter(|x| *x == l).count(), std::cmp::Reverse(langs.iter().position(|x| x == l))));
    best.map(Spoken::from_code).unwrap_or(Spoken::NoSpeech)
}
