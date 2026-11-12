//! 캡처 → 인식 → 번역 스레드를 잇고, 화면이 읽을 자막 상태를 관리한다.
//!
//! 번역 스레드는 확정 문장 큐를 먼저 비우고, 부분 텍스트는 가장 최근 것 하나만 번역한다.
//! 그래서 번역이 인식보다 느려도 자막이 밀리지 않는다.

use crate::asr::{Event, Model, Recognizer, Segmenter};
use crate::translate::Translator;
use crate::{audio, Lang};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const KEEP_LINES: usize = 2;
const PARTIAL_EVERY: Duration = Duration::from_millis(600);

pub enum Source {
    Loopback,
    Wav(String),
}

#[derive(Clone, Default)]
pub struct Line {
    pub ko: String,
    pub src: String,
    pub at: Option<Instant>,
}

/// 화면이 읽는 상태.
#[derive(Default)]
pub struct Subtitles {
    /// 확정 줄(오래된 것부터)
    pub lines: VecDeque<Line>,
    /// 진행 중인 줄
    pub current: Line,
    current_seg: u64,
    pub running: bool,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Stats {
    pub src_partial: Vec<Duration>, // 말 시작 → 첫 원문 부분 텍스트
    pub ko_first: Vec<Duration>,    // 말 시작 → 첫 번역 자막(부분 또는 확정)
    pub ko_final: Vec<Duration>,    // 말 끝 → 확정 번역
    first_seen: Option<u64>,
    first_ko: Option<u64>,
}

pub struct Shared {
    pub subs: Mutex<Subtitles>,
    pub stats: Mutex<Stats>,
    /// 상태가 바뀌면 부른다(화면 다시 그리기).
    pub notify: Box<dyn Fn() + Send + Sync>,
    /// 콘솔에 지연 로그를 찍는다.
    pub log: bool,
}

impl Shared {
    pub fn new(log: bool, notify: Box<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            subs: Mutex::default(),
            stats: Mutex::default(),
            notify,
            log,
        })
    }

    fn update(&self, f: impl FnOnce(&mut Subtitles)) {
        f(&mut self.subs.lock().unwrap());
        (self.notify)();
    }
}

/// 모델은 시작할 때 한 번만 올리고 다음 시작 때 다시 쓴다.
#[derive(Default)]
pub struct Models {
    recognizer: Option<(Model, Lang, Arc<Recognizer>)>,
    translator: Option<Arc<Translator>>,
}

impl Models {
    fn get(&mut self, model: Model, lang: Lang, shared: &Shared) -> Result<(Arc<Recognizer>, Arc<Translator>), String> {
        if !matches!(&self.recognizer, Some((m, l, _)) if *m == model && *l == lang) {
            shared.update(|s| s.status = "음성 인식 모델 불러오는 중…".into());
            self.recognizer = Some((model, lang, Arc::new(Recognizer::load(model, lang)?)));
        }
        if self.translator.is_none() {
            shared.update(|s| s.status = "번역 모델 불러오는 중…".into());
            self.translator = Some(Arc::new(Translator::load()?));
        }
        let r = self.recognizer.as_ref().unwrap().2.clone();
        Ok((r, self.translator.clone().unwrap()))
    }
}

struct Job {
    seg: u64,
    text: String,
    start: Instant,
    end: Option<Instant>, // 확정 문장이면 Some
}

#[derive(Default)]
struct Jobs {
    finals: VecDeque<Job>,
    partial: Option<Job>,
    closed: bool,
}

type JobQueue = Arc<(Mutex<Jobs>, Condvar)>;

pub struct Session {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Session {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }

    /// 정지하지 않고 끝날 때까지 기다린다(wav 입력).
    pub fn wait(self) {
        let _ = self.handle.join();
    }
}

pub fn start(lang: Lang, model: Model, source: Source, models: Arc<Mutex<Models>>, shared: Arc<Shared>) -> Session {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    shared.update(|s| {
        s.running = true;
        s.error = None;
        s.lines.clear();
        s.current = Line::default();
    });
    let handle = std::thread::spawn(move || {
        let result = run(lang, model, source, &models, &shared, &thread_stop);
        shared.update(|s| {
            s.running = false;
            s.status.clear();
            s.error = result.err();
        });
    });
    Session { stop, handle }
}

fn run(
    lang: Lang,
    model: Model,
    source: Source,
    models: &Mutex<Models>,
    shared: &Arc<Shared>,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    let (recognizer, translator) = models.lock().unwrap().get(model, lang, shared)?;
    let mut segmenter = Segmenter::new(recognizer)?;
    shared.update(|s| s.status = "듣는 중".into());

    let jobs: JobQueue = Arc::default();
    let translate_thread = {
        let (jobs, shared) = (jobs.clone(), shared.clone());
        std::thread::spawn(move || translate_loop(lang, &translator, &jobs, &shared))
    };

    // 캡처 스레드: 샘플을 채널로 넘긴다.
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let capture_thread = {
        let stop = stop.clone();
        std::thread::spawn(move || match source {
            Source::Loopback => audio::capture_loopback(&stop, |s| drop(tx.send(s))),
            Source::Wav(path) => audio::play_wav(&path, &stop, |s| drop(tx.send(s))),
        })
    };

    // 인식: 이 스레드에서 돈다.
    let mut last_sent = String::new();
    let mut last_sent_at = Instant::now();
    let mut on_event = |e: Event| match e {
        Event::Partial { seg, text, start } => {
            log_first_source(shared, seg, start, &text);
            shared.update(|s| {
                s.current_seg = seg;
                s.current.src = text.clone();
                if text.is_empty() {
                    s.current.ko.clear();
                }
            });
            if !text.is_empty() && should_translate(lang, &last_sent, &text, last_sent_at) {
                last_sent = text.clone();
                last_sent_at = Instant::now();
                let (lock, cvar) = &*jobs;
                lock.lock().unwrap().partial = Some(Job { seg, text, start, end: None });
                cvar.notify_one();
            }
        }
        Event::Final { seg, text, start, end } => {
            log_first_source(shared, seg, start, &text);
            if shared.log {
                println!("[원문 확정  +{:>5}ms] {text}", ms(end.elapsed()));
            }
            last_sent.clear();
            shared.update(|s| {
                s.current_seg = seg + 1;
                s.current.src = text.clone();
            });
            let (lock, cvar) = &*jobs;
            let mut j = lock.lock().unwrap();
            if j.partial.as_ref().is_some_and(|p| p.seg == seg) {
                j.partial = None;
            }
            j.finals.push_back(Job { seg, text, start, end: Some(end) });
            cvar.notify_one();
        }
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(mut samples) => {
                // 디코딩이 밀려 쌓인 샘플은 한 번에 넣는다. 부분 디코딩은 묶음마다 한 번이라 실시간을 따라잡는다.
                while let Ok(more) = rx.try_recv() {
                    samples.extend(more);
                }
                segmenter.push(&samples, &mut on_event)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // 캡처 끝(정지, wav 끝, 오류)
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    segmenter.flush(&mut on_event);
    let capture_result = capture_thread.join().unwrap_or(Err("소리 입력 스레드가 멈췄습니다".into()));

    {
        let (lock, cvar) = &*jobs;
        lock.lock().unwrap().closed = true;
        cvar.notify_one();
    }
    let _ = translate_thread.join();
    capture_result
}

/// 원문이 영어 단어 3개 / 일본어 8글자 이상 늘었거나, 마지막 요청에서 0.6초가 지났으면 번역한다.
fn should_translate(lang: Lang, last: &str, text: &str, last_at: Instant) -> bool {
    if last.is_empty() || last_at.elapsed() >= PARTIAL_EVERY {
        return last != text;
    }
    match lang {
        Lang::En => text.split_whitespace().count() >= last.split_whitespace().count() + 3,
        Lang::Ja => text.chars().count() >= last.chars().count() + 8,
    }
}

fn translate_loop(lang: Lang, translator: &Translator, jobs: &JobQueue, shared: &Shared) {
    let (lock, cvar) = &**jobs;
    loop {
        let job = {
            let mut j = lock.lock().unwrap();
            loop {
                if let Some(job) = j.finals.pop_front() {
                    break job;
                }
                if let Some(job) = j.partial.take() {
                    break job;
                }
                if j.closed {
                    return;
                }
                j = cvar.wait(j).unwrap();
            }
        };
        // 부분 번역 중에 확정 문장이 들어오면 부분 번역은 버리고 확정 문장을 바로 번역한다.
        let is_partial = job.end.is_none();
        let cancel = || is_partial && !lock.lock().unwrap().finals.is_empty();
        let ko = match translator.translate(lang, &job.text, &cancel) {
            Ok(Some(ko)) => ko,
            Ok(None) => continue,
            Err(e) => {
                shared.update(|s| s.error = Some(e));
                continue;
            }
        };
        let first = {
            let mut st = shared.stats.lock().unwrap();
            let first = st.first_ko != Some(job.seg);
            if first {
                st.first_ko = Some(job.seg);
                st.ko_first.push(job.start.elapsed());
            }
            if let Some(end) = job.end {
                st.ko_final.push(end.elapsed());
            }
            first
        };
        if shared.log {
            match job.end {
                Some(end) => println!("[번역 확정  +{:>5}ms] {ko}", ms(end.elapsed())),
                None => println!("[번역 부분  +{:>5}ms] {ko}", ms(job.start.elapsed())),
            }
            if first && job.end.is_some() {
                println!("              (첫 번역: 말 시작 +{}ms)", ms(job.start.elapsed()));
            }
        }
        shared.update(|s| match job.end {
            Some(_) => {
                s.lines.push_back(Line { ko, src: job.text, at: Some(Instant::now()) });
                while s.lines.len() > KEEP_LINES {
                    s.lines.pop_front();
                }
                if s.current_seg == job.seg + 1 && s.current.src == s.lines.back().unwrap().src {
                    s.current = Line::default();
                }
            }
            None => {
                if s.current_seg == job.seg {
                    s.current.ko = ko;
                }
            }
        });
    }
}

fn log_first_source(shared: &Shared, seg: u64, start: Instant, text: &str) {
    if text.is_empty() {
        return;
    }
    let mut st = shared.stats.lock().unwrap();
    if st.first_seen != Some(seg) {
        st.first_seen = Some(seg);
        st.src_partial.push(start.elapsed());
    }
    drop(st);
    if shared.log {
        println!("[원문 부분  +{:>5}ms] {text}", ms(start.elapsed()));
    }
}

pub fn ms(d: Duration) -> u128 {
    d.as_millis()
}
