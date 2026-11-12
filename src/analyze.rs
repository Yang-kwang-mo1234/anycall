//! 동영상 파일의 소리를 재생보다 앞서 읽어 자막 큐(시작, 끝, 원문, 번역)를 만든다.
//!
//! 실시간 모드(pipeline.rs)와 달리 부분 텍스트·부분 번역을 만들지 않는다. 구간이 끝나면
//! 한 번 인식하고 한 번 번역한다. 시각은 실제 시간이 아니라 파일 안의 위치(초)로 센다.

use crate::asr::{self, Recognizer};
use crate::audio::SAMPLE_RATE;
use crate::media_audio::MediaAudio;
use crate::translate::Translator;
use crate::Lang;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// 말 시작 직전을 이만큼 붙인다(VAD가 첫소리를 늦게 잡는 것을 보완).
const PRE_ROLL: f64 = 0.25;
/// 구간 앞에 붙일 소리를 위해 남겨 두는 길이
const KEEP: usize = SAMPLE_RATE as usize * 2;

#[derive(Clone, Debug)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub src: String,
    pub ko: String,
}

/// 인식할 말소리 구간
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub samples: Vec<f32>,
}

/// 파일 위치 기준으로 VAD 구간을 낸다.
pub struct FileSegmenter {
    vad: sherpa_onnx::VoiceActivityDetector,
    /// VAD에 넣은 첫 샘플의 파일 위치(초)
    origin: f64,
    /// 최근 샘플(앞에 붙일 소리용). `recent_start`는 그 첫 샘플의 VAD 기준 번호
    recent: VecDeque<f32>,
    recent_start: i64,
    pending: Vec<f32>,
}

impl FileSegmenter {
    pub fn new(origin: f64) -> Result<Self, String> {
        Ok(Self {
            vad: asr::create_vad()?,
            origin,
            recent: VecDeque::new(),
            recent_start: 0,
            pending: Vec::new(),
        })
    }

    pub fn push(&mut self, samples: &[f32], out: &mut impl FnMut(Segment)) {
        self.recent.extend(samples.iter().copied());
        if self.recent.len() > KEEP * 2 {
            let cut = self.recent.len() - KEEP;
            self.recent.drain(..cut);
            self.recent_start += cut as i64;
        }
        self.pending.extend_from_slice(samples);
        let w = asr::VAD_WINDOW;
        let n = self.pending.len() / w * w;
        for chunk in self.pending[..n].chunks(w) {
            self.vad.accept_waveform(chunk);
        }
        self.pending.drain(..n);
        self.drain(out);
    }

    pub fn flush(&mut self, out: &mut impl FnMut(Segment)) {
        self.vad.flush();
        self.drain(out);
    }

    fn drain(&mut self, out: &mut impl FnMut(Segment)) {
        while let Some(seg) = self.vad.front() {
            let (start, body) = (seg.start(), seg.samples().to_vec());
            self.vad.pop();
            // VAD 구간 앞에 최근 샘플에서 PRE_ROLL만큼 붙인다.
            let pre = (PRE_ROLL * SAMPLE_RATE as f64) as i64;
            let from = (start - pre).max(self.recent_start);
            let mut samples: Vec<f32> = if from < start {
                let a = (from - self.recent_start) as usize;
                let b = ((start - self.recent_start) as usize).min(self.recent.len());
                self.recent.range(a..b).copied().collect()
            } else {
                Vec::new()
            };
            let lead = samples.len();
            samples.extend(body);
            let sr = SAMPLE_RATE as f64;
            out(Segment {
                start: self.origin + (start as f64 - lead as f64) / sr,
                end: self.origin + (start as f64 + (samples.len() - lead) as f64) / sr,
                samples,
            });
        }
    }
}

/// 진행 상황
pub enum Progress {
    /// 이 위치(초)까지 소리를 읽었다.
    Read(f64),
    /// 이 위치(초)까지의 큐는 모두 나왔다(분석 시작 위치부터).
    Ready(f64),
    /// 큐 하나가 완성됐다.
    Cue(Cue),
}

/// 말하는 중인 구간은 아직 VAD 안에 있으므로, 읽은 위치에서 이만큼 앞까지만 끝났다고 본다.
const VAD_LAG: f64 = asr::MAX_SPEECH as f64;

/// `path`의 `from`초부터 끝까지 분석한다. `stop`이 서면 멈춘다.
pub fn run(
    path: &str,
    from: f64,
    lang: Lang,
    recognizer: Arc<Recognizer>,
    translator: Arc<Translator>,
    stop: &AtomicBool,
    mut on: impl FnMut(Progress),
) -> Result<(), String> {
    let mut media = MediaAudio::open(path)?;
    if from > 0.0 {
        media.seek(from)?;
    }

    // 인식 결과를 번역 스레드로 넘긴다. 번역이 더 느리므로 번역 쪽이 병목이다.
    // 번역 스레드는 받은 구간마다 하나씩 답한다(번역할 것이 없으면 None).
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx_src, rx_src) = mpsc::channel::<(f64, f64, String)>();
    let (tx_cue, rx_cue) = mpsc::channel::<Option<Cue>>();
    let translate_thread = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            for (start, end, src) in rx_src {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let cue = match translator.translate(lang, &src, &|| cancel.load(Ordering::Relaxed)) {
                    Ok(Some(ko)) => Some(Cue { start, end, src, ko }),
                    Ok(None) => None,
                    Err(e) => Some(Cue { start, end, ko: format!("[번역 실패: {e}]"), src }),
                };
                if tx_cue.send(cue).is_err() {
                    break;
                }
            }
        })
    };

    // 번역을 기다리는 구간의 시작 시각(보낸 순서)
    let mut pending: VecDeque<f64> = VecDeque::new();
    let mut seg: Option<FileSegmenter> = None;
    let on_segment = |s: Segment, pending: &mut VecDeque<f64>| {
        let text = recognizer.decode(&s.samples);
        if !text.is_empty() {
            pending.push_back(s.start);
            let _ = tx_src.send((s.start, s.end, text));
        }
    };
    let mut read_to = from;
    let receive = |pending: &mut VecDeque<f64>, read_to: f64, block: bool, on: &mut dyn FnMut(Progress)| {
        // block이면 하나는 기다려 받고, 나머지는 와 있는 것만 받는다.
        let mut wait = block && !pending.is_empty();
        loop {
            let got = if wait { rx_cue.recv_timeout(Duration::from_millis(200)).ok() } else { rx_cue.try_recv().ok() };
            wait = false;
            let Some(cue) = got else { break };
            pending.pop_front();
            if let Some(c) = cue {
                on(Progress::Cue(c));
            }
        }
        let ready = pending.front().copied().unwrap_or(f64::MAX).min(read_to);
        on(Progress::Ready(ready));
    };
    while !stop.load(Ordering::Relaxed) {
        let Some(chunk) = media.read()? else { break };
        let s = match &mut seg {
            Some(s) => s,
            // 탐색 뒤 첫 덩어리의 시각을 기준으로 삼는다.
            None => seg.insert(FileSegmenter::new(chunk.start)?),
        };
        read_to = chunk.start + chunk.samples.len() as f64 / SAMPLE_RATE as f64;
        s.push(&chunk.samples, &mut |g| on_segment(g, &mut pending));
        on(Progress::Read(read_to));
        receive(&mut pending, (read_to - VAD_LAG).max(from), false, &mut on);
    }
    let stopped = stop.load(Ordering::Relaxed);
    if !stopped {
        if let Some(s) = &mut seg {
            s.flush(&mut |g| on_segment(g, &mut pending));
        }
        // 파일 끝: 남은 번역을 모두 받는다.
        while !pending.is_empty() && !stop.load(Ordering::Relaxed) {
            receive(&mut pending, read_to, true, &mut on);
        }
        if pending.is_empty() {
            on(Progress::Ready(f64::MAX));
        }
    }
    cancel.store(true, Ordering::Relaxed);
    drop(tx_src);
    let _ = translate_thread.join();
    Ok(())
}
