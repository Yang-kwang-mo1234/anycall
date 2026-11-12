//! anycall: Windows에서 재생 중인 영어·일본어 소리를 한국어 자막으로 실시간 번역한다.
//!
//! 실행 방법
//!   anycall [파일] [--lang auto|en|ja|off]    자막이 나오는 동영상 플레이어(기본)
//!   anycall --overlay                         시스템 소리 자막 오버레이와 제어 창
//!   anycall --console en|ja [--wav 파일]      콘솔에 원문·번역과 지연 시간을 찍는다(1단계)
//!   anycall --bench-translate                 번역 시간·부분 번역 흔들림 측정(0단계)
//!   anycall --audio-info 파일                  동영상 파일의 소리 형식·읽기 속도·탐색 확인(플레이어 0단계)
//!   anycall --file-bench 파일 --lang en|ja    파일 자막 분석 속도 측정(플레이어 0단계)
//!   anycall --langid 파일                      동영상 파일의 음성 언어 판별(플레이어 0단계)
//!   공통 옵션: --model tiny|base (Moonshine 크기, 기본 tiny)
//!
//! 모델은 models/ 에 둔다(scripts/download-models.ps1). 실행 파일 위치에서 위로 올라가며 찾는다.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analyze;
mod asr;
mod audio;
mod cues;
mod langid;
mod media_audio;
mod overlay;
mod pipeline;
mod player;
mod player_ui;
mod subfile;
mod translate;

use intel_onemkl_prebuild as _; // MKL 정적 라이브러리를 링크에 넣는다.

use asr::Model;
use pipeline::{Models, Shared, Source};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "en" => Some(Lang::En),
            "ja" => Some(Lang::Ja),
            _ => None,
        }
    }
}

fn main() {
    translate::prepare_backend();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let model = match value("--model").as_deref() {
        None | Some("tiny") => Model::MoonshineTiny,
        Some("base") => Model::MoonshineBase,
        Some(other) => exit_with(&format!("알 수 없는 모델: {other}")),
    };

    let abs = |p: String| std::path::absolute(&p).map_or(p, |a| a.display().to_string());
    if let Some(file) = value("--audio-info") {
        attach_console();
        audio_info(&abs(file));
    } else if let Some(file) = value("--langid") {
        attach_console();
        let file = abs(file);
        find_models_dir();
        langid_file(&file);
    } else if let Some(file) = value("--file-bench") {
        attach_console();
        let lang = value("--lang").and_then(|l| Lang::parse(&l)).unwrap_or_else(|| exit_with("--lang en|ja 를 준다"));
        let file = abs(file);
        find_models_dir();
        file_bench(&file, lang, model);
    } else if args.iter().any(|a| a == "--bench-translate") {
        attach_console();
        find_models_dir();
        bench_translate();
    } else if let Some(lang) = value("--console") {
        attach_console();
        let lang = Lang::parse(&lang).unwrap_or_else(|| exit_with("--console 에는 en 또는 ja 를 준다"));
        // 아래에서 작업 폴더를 바꾸므로 wav 경로는 먼저 절대 경로로 만든다.
        let wav = value("--wav").map(|w| std::path::absolute(&w).map_or(w, |p| p.display().to_string()));
        find_models_dir();
        console(lang, model, wav);
    } else if args.iter().any(|a| a == "--overlay") {
        find_models_dir();
        if let Err(e) = overlay::run(model) {
            exit_with(&format!("창을 열 수 없습니다: {e}"));
        }
    } else {
        // 플레이어. 옵션이 아닌 첫 인자는 열 파일이다(탐색기에서 "연결 프로그램"으로 열 때도 이렇게 온다).
        let file = args
            .iter()
            .enumerate()
            .find(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[i - 1].starts_with("--")))
            .map(|(_, a)| abs(a.clone()));
        let ai_lang = match value("--lang").as_deref() {
            None | Some("auto") => player_ui::AiLang::Auto,
            Some("off") => player_ui::AiLang::Off,
            Some(l) => player_ui::AiLang::Fixed(Lang::parse(l).unwrap_or_else(|| exit_with("--lang 에는 auto, en, ja, off 를 준다"))),
        };
        find_models_dir();
        if let Err(e) = player_ui::run(player_ui::Options { file, ai_lang, model }) {
            exit_with(&format!("창을 열 수 없습니다: {e}"));
        }
    }
}

/// 모델 경로(models/...)는 작업 폴더 기준이다. 작업 폴더에 models가 없으면 실행 파일 위치에서
/// 위로 올라가며 models 폴더가 있는 곳을 찾아 작업 폴더로 삼는다(target/release/anycall.exe를 바로 실행할 때).
fn find_models_dir() {
    if std::path::Path::new("models").is_dir() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    if let Some(dir) = exe.ancestors().skip(1).find(|d| d.join("models").is_dir()) {
        let _ = std::env::set_current_dir(dir);
    }
}

fn exit_with(msg: &str) -> ! {
    attach_console();
    eprintln!("{msg}");
    std::process::exit(1);
}

/// 릴리스 빌드는 창 프로그램이라 콘솔이 없다. 콘솔에서 실행했으면 그 콘솔에 붙는다.
fn attach_console() {
    #[cfg(not(debug_assertions))]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        unsafe { AttachConsole(u32::MAX) }; // ATTACH_PARENT_PROCESS
    }
}

fn console(lang: Lang, model: Model, wav: Option<String>) {
    let shared = Shared::new(true, Box::new(|| {}));
    let source = match &wav {
        Some(p) => Source::Wav(p.clone()),
        None => Source::Loopback,
    };
    println!("원문 {}, 인식 {:?}. 줄 앞 +ms는 말 시작(부분) 또는 말 끝(확정)에서 지난 시간이다.", lang.code(), model);
    let session = pipeline::start(lang, model, source, Arc::new(Mutex::new(Models::default())), shared.clone());
    if wav.is_some() {
        session.wait();
    } else {
        println!("Enter를 누르면 끝난다.");
        let _ = std::io::stdin().read_line(&mut String::new());
        session.stop();
    }
    if let Some(e) = &shared.subs.lock().unwrap().error {
        eprintln!("오류: {e}");
    }
    let st = shared.stats.lock().unwrap();
    println!("\n== 지연 요약 (구간 수, 평균 / 최대 ms) ==");
    for (name, v) in [
        ("말 시작 → 원문 부분 텍스트", &st.src_partial),
        ("말 시작 → 첫 번역 자막", &st.ko_first),
        ("말 끝 → 확정 번역", &st.ko_final),
    ] {
        println!("{name}: {}", summary(v));
    }
}

fn summary(v: &[Duration]) -> String {
    if v.is_empty() {
        return "없음".into();
    }
    let avg = v.iter().sum::<Duration>() / v.len() as u32;
    let max = v.iter().max().unwrap();
    format!("{}개, {} / {}", v.len(), avg.as_millis(), max.as_millis())
}

fn bench_translate() {
    let t0 = Instant::now();
    let t = translate::Translator::load().unwrap_or_else(|e| exit_with(&e));
    println!("번역 모델 로드: {}ms", t0.elapsed().as_millis());
    let cases: &[(Lang, &str)] = &[
        (Lang::En, "I don't think so."),
        (Lang::En, "We need to leave before the storm gets any worse tonight."),
        (Lang::En, "If you had told me about the meeting earlier, I would have rearranged my schedule so that we could all discuss the budget together."),
        (Lang::Ja, "今日はいい天気ですね。"),
        (Lang::Ja, "明日の会議の資料はもう準備できましたか、まだなら手伝います。"),
        (Lang::Ja, "子供の頃から住んでいたこの町を離れるのは寂しいけれど、新しい仕事のために東京へ引っ越すことに決めました。"),
    ];
    println!("\n== 문장 길이별 번역 시간 (3회 중 최소) ==");
    let none = || false;
    let run = |lang, text: &str| t.translate(lang, text, &none).map(Option::unwrap_or_default).unwrap_or_else(|e| e);
    run(Lang::En, "warm up");
    for (lang, text) in cases {
        let mut best = Duration::MAX;
        let mut out = String::new();
        for _ in 0..3 {
            let s = Instant::now();
            out = run(*lang, text);
            best = best.min(s.elapsed());
        }
        let len = match lang {
            Lang::En => format!("{}단어", text.split_whitespace().count()),
            Lang::Ja => format!("{}글자", text.chars().count()),
        };
        println!("[{} {len} {}ms] {text}\n    → {out}", lang.code(), best.as_millis());
    }
    println!("\n== 앞부분만 번역했을 때(부분 번역 흔들림) ==");
    for (lang, text) in [cases[2], cases[5]] {
        let prefixes: Vec<String> = match lang {
            Lang::En => {
                let w: Vec<&str> = text.split_whitespace().collect();
                (1..=4).map(|k| w[..w.len() * k / 4].join(" ")).collect()
            }
            Lang::Ja => {
                let c: Vec<char> = text.chars().collect();
                (1..=4).map(|k| c[..c.len() * k / 4].iter().collect()).collect()
            }
        };
        for p in prefixes {
            println!("  {p}\n    → {}", run(lang, &p));
        }
    }
}

fn audio_info(file: &str) {
    let t0 = Instant::now();
    let mut m = media_audio::MediaAudio::open(file).unwrap_or_else(|e| exit_with(&e));
    println!("열기: {}ms, 소리 형식 {}, 길이 {:?}초", t0.elapsed().as_millis(), m.native_format, m.duration);
    let t1 = Instant::now();
    let (mut n, mut first, mut last, mut peak) = (0usize, None, 0.0f64, 0f32);
    while let Some(c) = m.read().unwrap_or_else(|e| exit_with(&e)) {
        first.get_or_insert(c.start);
        last = c.start + c.samples.len() as f64 / audio::SAMPLE_RATE as f64;
        peak = c.samples.iter().fold(peak, |p, s| p.max(s.abs()));
        n += c.samples.len();
    }
    let secs = n as f64 / audio::SAMPLE_RATE as f64;
    let el = t1.elapsed().as_secs_f64();
    println!(
        "전체 읽기: {el:.2}초, 샘플 {secs:.2}초 분량(첫 시각 {:.3}, 끝 시각 {last:.3}), 재생 속도의 {:.0}배, 최대 크기 {peak:.3}",
        first.unwrap_or(0.0),
        secs / el
    );
    for target in [10.0, 3.0, 15.5] {
        let t = Instant::now();
        m.seek(target).unwrap_or_else(|e| exit_with(&e));
        let got = m.read().unwrap_or_else(|e| exit_with(&e)).map(|c| c.start);
        println!("탐색 {target}초 → 첫 덩어리 {got:?}초 ({}ms)", t.elapsed().as_millis());
    }
}

fn file_bench(file: &str, lang: Lang, model: Model) {
    let t0 = Instant::now();
    let recognizer = Arc::new(asr::Recognizer::load(model, lang).unwrap_or_else(|e| exit_with(&e)));
    let translator = Arc::new(translate::Translator::load().unwrap_or_else(|e| exit_with(&e)));
    println!("모델 로드: {}ms", t0.elapsed().as_millis());
    let stop = std::sync::atomic::AtomicBool::new(false);
    let t1 = Instant::now();
    let mut cues = 0usize;
    let mut lag_max = 0f64;
    let r = analyze::run(file, 0.0, lang, recognizer, translator, &stop, |p| match p {
        analyze::Progress::Read(_) | analyze::Progress::Ready(_) => {}
        analyze::Progress::Cue(c) => {
            cues += 1;
            // 재생을 분석 시작과 동시에 했다면 이 큐가 필요한 시각(c.start)에 비해 얼마나 늦었나
            let lag = t1.elapsed().as_secs_f64() - c.start;
            lag_max = lag_max.max(lag);
            println!("[{:>7.2} ~ {:>7.2}] (완성 {:>6.2}초, 늦음 {:>5.2}) {}\n                    → {}", c.start, c.end, t1.elapsed().as_secs_f64(), lag, c.src, c.ko);
        }
    });
    if let Err(e) = r {
        exit_with(&e);
    }
    let el = t1.elapsed().as_secs_f64();
    let dur = media_audio::MediaAudio::open(file).ok().and_then(|m| m.duration).unwrap_or(0.0);
    println!(
        "\n큐 {cues}개, 분석 {el:.1}초 / 파일 {dur:.1}초 = 재생 속도의 {:.2}배. 재생과 동시에 시작했을 때 가장 늦은 큐: {lag_max:.2}초",
        dur / el
    );
}

fn langid_file(file: &str) {
    let t0 = Instant::now();
    let id = langid::LangId::load().unwrap_or_else(|e| exit_with(&e));
    println!("모델 로드: {}ms", t0.elapsed().as_millis());
    let t1 = Instant::now();
    let (lang, details) = id.detect_file(file).unwrap_or_else(|e| exit_with(&e));
    for d in &details {
        println!("  {:>7.1}초 지점: 말소리 {:.1}초 → {:?}", d.at, d.speech, d.lang);
    }
    println!("판별: {lang:?} ({}ms)", t1.elapsed().as_millis());
}
