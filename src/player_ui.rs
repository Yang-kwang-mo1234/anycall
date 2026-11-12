//! 플레이어 화면: 영상 창(player.rs) 위에 겹친 투명 egui 창. 자막, 조작 막대, 키보드·마우스 입력을 맡는다.

use crate::asr::{Model, Recognizer};
use crate::cues::CueStore;
use crate::langid::{LangId, Spoken};
use crate::overlay::{install_fonts, outlined};
use crate::player::{self, Engine, VideoWindow};
use crate::translate::Translator;
use crate::{analyze, Lang};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const BAR_HEIGHT: f32 = 56.0;
const HIDE_CONTROLS_AFTER: Duration = Duration::from_millis(2500);
const SEEK_STEP: f64 = 5.0;

/// 음성 언어를 어떻게 정할지
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiLang {
    Auto,
    Fixed(Lang),
    Off,
}

pub struct Options {
    pub file: Option<String>,
    pub ai_lang: AiLang,
    pub model: Model,
}

pub fn run(opts: Options) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("anycall 자막")
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_taskbar(false)
            .with_resizable(false)
            .with_drag_and_drop(true)
            .with_inner_size([640.0, 360.0]),
        // wgpu는 Windows에서 창을 투명하게 합성하지 못한다(흰 배경이 된다). OpenGL을 쓴다.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native("anycall-player", options, Box::new(move |cc| Ok(Box::new(App::new(cc, opts)?))))
}

/// AI 자막 분석. 한 번에 하나만 돈다. 탐색하면 새 위치에서 다시 시작한다.
struct Analysis {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    from: f64,
    /// 소리를 읽어 나간 위치
    read: Arc<Mutex<f64>>,
}

/// 모델은 한 번 올리고 다시 쓴다.
#[derive(Default)]
struct Models {
    langid: Option<Arc<LangId>>,
    recognizer: Option<(Lang, Arc<Recognizer>)>,
    translator: Option<Arc<Translator>>,
}

/// 지금 보여 주는 자막
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// 동영상 옆의 자막 파일
    File,
    /// 음성을 인식·번역한 자막
    Ai,
    Off,
}

struct FileSubs {
    name: String,
    cues: Vec<crate::analyze::Cue>,
}

/// 분석 스레드와 화면이 함께 보는 상태
#[derive(Default)]
struct AiState {
    /// 판별했거나 고른 언어. None이면 아직 모른다.
    lang: Option<Lang>,
    /// 화면에 보여 줄 상태 문구(모델 불러오는 중, 한국어 음성 등)
    status: String,
    error: Option<String>,
    /// 자막을 만들지 않기로 했다(한국어·지원하지 않는 언어·소리 없음).
    no_subs: bool,
}

struct App {
    video: VideoWindow,
    engine: Engine,
    overlay: HWND,
    file: Option<String>,
    model: Model,
    /// AI 자막의 음성 언어(자동 또는 고정). Off는 쓰지 않는다(그때는 mode가 Off).
    ai_lang: AiLang,
    mode: Mode,
    /// 파일을 열 때 고를 자막(자막 파일이 없을 때)
    default_mode: Mode,
    file_subs: Option<FileSubs>,
    store: Arc<Mutex<CueStore>>,
    ai: Arc<Mutex<AiState>>,
    models: Arc<Mutex<Models>>,
    analysis: Option<Analysis>,
    last_activity: Instant,
    /// 탐색 막대를 끄는 중이면 그 위치
    dragging: Option<f64>,
    /// 자막을 이만큼 늦춘다(초, 음수면 앞당긴다).
    sub_delay: f64,
    toast: Option<(String, Instant)>,
    /// 재생할 수 없을 때 보여 줄 문구
    play_error: Option<String>,
    placed: bool,
    show_source: bool,
}

fn hwnd_of(cc: &eframe::CreationContext<'_>) -> Result<HWND, String> {
    match cc.window_handle().map_err(|e| e.to_string())?.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => Err("Win32 창이 아닙니다".into()),
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        install_fonts(&cc.egui_ctx);
        let wake = |ctx: &egui::Context| {
            let ctx = ctx.clone();
            Box::new(move || ctx.request_repaint()) as Box<dyn Fn() + Send + Sync>
        };
        let video = VideoWindow::create("anycall", wake(&cc.egui_ctx))?;
        let overlay = hwnd_of(cc)?;
        video.adopt(overlay);
        video.place_overlay(overlay);
        let engine = Engine::new(video.hwnd, wake(&cc.egui_ctx))?;
        let mut app = Self {
            video,
            engine,
            overlay,
            file: None,
            model: opts.model,
            ai_lang: if opts.ai_lang == AiLang::Off { AiLang::Auto } else { opts.ai_lang },
            mode: Mode::Off,
            default_mode: if opts.ai_lang == AiLang::Off { Mode::Off } else { Mode::Ai },
            file_subs: None,
            store: Arc::default(),
            ai: Arc::default(),
            models: Arc::default(),
            analysis: None,
            last_activity: Instant::now(),
            dragging: None,
            sub_delay: 0.0,
            toast: None,
            play_error: None,
            placed: false,
            show_source: false,
        };
        if let Some(f) = opts.file {
            app.open(&f, &cc.egui_ctx);
        }
        Ok(app)
    }

    fn open(&mut self, path: &str, ctx: &egui::Context) {
        self.stop_analysis();
        *self.ai.lock().unwrap() = AiState::default();
        *self.store.lock().unwrap() = CueStore::generated();
        self.sub_delay = 0.0;
        self.play_error = None;
        if let Err(e) = self.engine.open(path) {
            self.play_error = Some(e);
            return;
        }
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.video.set_title(&format!("{name} - anycall"));
        self.file = Some(path.to_string());
        // 자막 파일이 있으면 그것을 쓰고, 없으면 AI 자막을 만든다.
        self.file_subs = None;
        if let Some(found) = crate::subfile::find(std::path::Path::new(path)) {
            self.load_sub_file(&found.path, ctx);
        }
        let mode = if self.file_subs.is_some() { Mode::File } else { self.default_mode };
        self.mode = Mode::Off;
        self.set_mode(mode, ctx);
    }

    /// 자막 파일을 읽어 파일 자막으로 바꾼다.
    fn load_sub_file(&mut self, path: &std::path::Path, ctx: &egui::Context) {
        match crate::subfile::load(path) {
            Ok(cues) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(format!("자막 파일: {name} ({}개)", cues.len()));
                self.file_subs = Some(FileSubs { name, cues });
                if self.mode == Mode::File {
                    self.mode = Mode::Off; // 같은 모드로 다시 들어가 새 자막을 넣는다.
                }
                if self.file.is_some() {
                    self.set_mode(Mode::File, ctx);
                }
            }
            Err(e) => self.toast(format!("{}: {e}", path.display())),
        }
    }

    fn set_mode(&mut self, mode: Mode, ctx: &egui::Context) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.stop_analysis();
        *self.ai.lock().unwrap() = AiState::default();
        *self.store.lock().unwrap() = match (mode, &self.file_subs) {
            (Mode::File, Some(f)) => CueStore::from_cues(f.cues.clone()),
            _ => CueStore::generated(),
        };
        if mode == Mode::Ai {
            let t = self.engine.time();
            self.start_analysis((t - 1.0).max(0.0), ctx);
        }
    }

    fn stop_analysis(&mut self) {
        if let Some(a) = self.analysis.take() {
            a.stop.store(true, Ordering::Relaxed);
            // 멈추는 데 시간이 걸릴 수 있어(번역 한 문장) 기다리지 않는다. 스레드는 스스로 끝난다.
            drop(a.handle);
        }
    }

    fn start_analysis(&mut self, from: f64, ctx: &egui::Context) {
        self.stop_analysis();
        let Some(path) = self.file.clone() else { return };
        if self.mode != Mode::Ai {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let read = Arc::new(Mutex::new(from));
        let (store, ai, models, ctx2) = (self.store.clone(), self.ai.clone(), self.models.clone(), ctx.clone());
        let (ai_lang, model, stop2, read2) = (self.ai_lang, self.model, stop.clone(), read.clone());
        let handle = std::thread::spawn(move || {
            crate::media_audio::init_thread();
            let r = analysis_thread(&path, from, ai_lang, model, &store, &ai, &models, &stop2, &read2, &ctx2);
            if let Err(e) = r {
                if !stop2.load(Ordering::Relaxed) {
                    let mut a = ai.lock().unwrap();
                    a.error = Some(e);
                    a.status.clear();
                }
            }
            ctx2.request_repaint();
        });
        self.analysis = Some(Analysis { stop, handle, from, read });
    }

    /// 분석하지 않은 곳으로 건너뛰면 분석도 그리로 옮긴다.
    fn follow_seek(&mut self, t: f64, ctx: &egui::Context) {
        if self.mode != Mode::Ai || self.ai.lock().unwrap().no_subs || self.store.lock().unwrap().is_done(t) {
            return;
        }
        if let Some(a) = &self.analysis {
            let read = *a.read.lock().unwrap();
            // 지금 분석이 곧 닿을 위치면 그대로 둔다.
            if !a.handle.is_finished() && a.from <= t && t <= read + 20.0 {
                return;
            }
        }
        self.start_analysis((t - 1.0).max(0.0), ctx);
    }

    fn seek(&mut self, t: f64, ctx: &egui::Context) {
        self.engine.seek(t);
        self.follow_seek(t, ctx);
        self.touch();
    }

    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// AI 자막으로 바꾸고 음성 언어를 정한다.
    fn set_ai_lang(&mut self, l: AiLang, ctx: &egui::Context) {
        if self.mode == Mode::Ai && self.ai_lang == l {
            return;
        }
        self.ai_lang = l;
        self.mode = Mode::Off;
        self.set_mode(Mode::Ai, ctx);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (keys, dropped) = ctx.input(|i| {
            let keys: Vec<(Key, egui::Modifiers)> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect();
            let dropped: Vec<String> = i.raw.dropped_files.iter().map(|f| f.path().display().to_string()).filter(|p| !p.is_empty()).collect();
            (keys, dropped)
        });
        // 자막 파일을 끌어다 놓으면 그 자막으로, 그 밖의 파일은 동영상으로 연다.
        for f in &dropped {
            self.open_path(f, ctx);
        }
        for (key, m) in keys {
            self.touch();
            let t = self.engine.time();
            match key {
                Key::Space => self.toggle_play(),
                Key::ArrowLeft => self.seek(t - SEEK_STEP, ctx),
                Key::ArrowRight => self.seek(t + SEEK_STEP, ctx),
                Key::ArrowUp => {
                    self.engine.set_volume(self.engine.volume() + 0.05);
                    self.toast(format!("볼륨 {:.0}%", self.engine.volume() * 100.0));
                }
                Key::ArrowDown => {
                    self.engine.set_volume(self.engine.volume() - 0.05);
                    self.toast(format!("볼륨 {:.0}%", self.engine.volume() * 100.0));
                }
                Key::F | Key::Enter => self.toggle_fullscreen(),
                Key::Escape if self.video.is_fullscreen() => self.toggle_fullscreen(),
                Key::OpenBracket | Key::CloseBracket => {
                    let step = if m.shift { 0.1 } else { 0.5 };
                    self.sub_delay += if key == Key::OpenBracket { -step } else { step };
                    self.toast(format!("자막 싱크 {:+.1}초", self.sub_delay));
                }
                Key::S => self.show_source = !self.show_source,
                Key::O => self.open_dialog(ctx),
                _ => {}
            }
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = player::open_file_dialog(self.video.hwnd) else { return };
        self.open_path(&path, ctx);
    }

    /// 자막 파일이면 자막으로, 그 밖에는 동영상으로 연다.
    fn open_path(&mut self, f: &str, ctx: &egui::Context) {
        let ext = std::path::Path::new(f).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if crate::subfile::is_subtitle_ext(&ext) {
            self.load_sub_file(std::path::Path::new(f), ctx);
        } else {
            self.open(f, ctx);
        }
    }

    fn toggle_play(&mut self) {
        if self.engine.is_paused() || self.engine.is_ended() {
            if self.engine.is_ended() {
                self.engine.seek(0.0);
            }
            self.engine.play();
        } else {
            self.engine.pause();
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.video.toggle_fullscreen();
        self.video.place_overlay(self.overlay);
    }

    fn draw_subtitles(&self, ui: &egui::Ui, rect: Rect, bar_visible: bool) {
        let t = self.engine.time() - self.sub_delay;
        let store = self.store.lock().unwrap();
        let cues = store.active(t);
        let ko_size = (rect.height() * 0.05).clamp(18.0, 56.0);
        let src_size = ko_size * 0.62;
        let wrap = rect.width() * 0.9;
        let mut y = rect.bottom() - if bar_visible { BAR_HEIGHT + 12.0 } else { rect.height() * 0.06 };
        let painter = ui.painter();
        for c in cues.iter().rev() {
            let text = if c.ko.is_empty() { &c.src } else { &c.ko };
            y = outlined(painter, text, ko_size, Color32::WHITE, wrap, rect.center().x, y);
            if self.show_source && !c.src.is_empty() && c.src != c.ko {
                y = outlined(painter, &c.src, src_size, Color32::from_gray(200), wrap, rect.center().x, y);
            }
            y -= 6.0;
        }
        drop(store);

        // 분석이 재생을 못 따라갔으면 알려 준다.
        let ai = self.ai.lock().unwrap();
        let waiting = self.mode == Mode::Ai
            && !ai.no_subs
            && ai.error.is_none()
            && self.file.is_some()
            && !self.store.lock().unwrap().is_done(t)
            && self.analysis.as_ref().is_some_and(|a| !a.handle.is_finished());
        let status = if let Some(e) = &ai.error {
            Some((e.clone(), Color32::from_rgb(255, 120, 120)))
        } else if !ai.status.is_empty() {
            Some((ai.status.clone(), Color32::from_gray(220)))
        } else if waiting {
            Some(("자막 준비 중…".to_string(), Color32::from_gray(220)))
        } else {
            None
        };
        if let Some((s, color)) = status {
            let job = egui::text::LayoutJob::simple(s, FontId::proportional(16.0), color, rect.width() - 32.0);
            painter.galley(rect.left_top() + Vec2::new(16.0, 14.0), painter.layout_job(job), color);
        }
        if let Some((s, at)) = &self.toast {
            if at.elapsed() < Duration::from_millis(1500) {
                painter.text(rect.right_top() + Vec2::new(-16.0, 14.0), Align2::RIGHT_TOP, s, FontId::proportional(18.0), Color32::WHITE);
            }
        }
    }

    fn draw_bar(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context) {
        let bar = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - BAR_HEIGHT), rect.max);
        ui.painter().rect_filled(bar, CornerRadius::ZERO, Color32::from_black_alpha(170));
        let dur = self.engine.duration();
        let now = self.dragging.unwrap_or_else(|| self.engine.time());

        // 탐색 막대(맨 위 한 줄)
        let track = Rect::from_min_size(bar.min + Vec2::new(12.0, 6.0), Vec2::new(bar.width() - 24.0, 14.0));
        let resp = ui.allocate_rect(track, Sense::click_and_drag());
        let line = Rect::from_center_size(track.center(), Vec2::new(track.width(), 4.0));
        ui.painter().rect_filled(line, CornerRadius::same(2), Color32::from_gray(90));
        if dur > 0.0 {
            // 분석을 마친 구간
            let store = self.store.lock().unwrap();
            if self.mode == Mode::Ai && store.generated {
                let mut x = 0.0;
                while x < dur {
                    if let Some(end) = store.done_until(x) {
                        let a = line.left() + line.width() * (x / dur) as f32;
                        let b = line.left() + line.width() * (end.min(dur) / dur) as f32;
                        ui.painter().rect_filled(
                            Rect::from_min_max(Pos2::new(a, line.top()), Pos2::new(b, line.bottom())),
                            CornerRadius::ZERO,
                            Color32::from_rgb(80, 110, 150),
                        );
                        x = end + 0.01;
                    } else {
                        x += dur / 400.0;
                    }
                }
            }
            drop(store);
            let fx = line.left() + line.width() * (now / dur).clamp(0.0, 1.0) as f32;
            ui.painter().rect_filled(Rect::from_min_max(line.min, Pos2::new(fx, line.bottom())), CornerRadius::same(2), Color32::from_rgb(230, 70, 70));
            ui.painter().circle_filled(Pos2::new(fx, line.center().y), if resp.hovered() || resp.dragged() { 7.0 } else { 5.0 }, Color32::WHITE);
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) as f64 * dur;
                self.dragging = Some(t);
                self.touch();
            }
            if resp.drag_stopped() || resp.clicked() {
                if let Some(t) = self.dragging.take() {
                    self.seek(t, ctx);
                }
            }
        }

        // 아래 줄: 재생 버튼, 시간, 언어, 볼륨, 전체 화면
        let row = Rect::from_min_max(Pos2::new(bar.left() + 8.0, track.bottom() + 2.0), Pos2::new(bar.right() - 8.0, bar.bottom() - 4.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        ui.visuals_mut().override_text_color = Some(Color32::WHITE);
        if ui.add(egui::Button::new("열기").frame(false)).on_hover_text("동영상 또는 자막 파일 열기 (O)").clicked() {
            self.open_dialog(ctx);
        }
        let play_label = if self.engine.is_paused() || self.engine.is_ended() { "▶" } else { "⏸" };
        if ui.add(egui::Button::new(egui::RichText::new(play_label).size(18.0)).frame(false)).clicked() {
            self.toggle_play();
        }
        ui.label(format!("{} / {}", clock(now), clock(dur)));
        ui.add_space(12.0);

        // 자막 고르기: 파일 자막 / AI 자막(자동·영어·일본어) / 끄기
        let label = match self.mode {
            Mode::File => format!("자막: {}", self.file_subs.as_ref().map(|f| f.name.as_str()).unwrap_or("파일")),
            Mode::Off => "자막: 끔".to_string(),
            Mode::Ai => match self.ai_lang {
                AiLang::Fixed(l) => format!("AI 자막: {}", lang_name(l)),
                _ => match self.ai.lock().unwrap().lang {
                    Some(l) => format!("AI 자막: 자동({})", lang_name(l)),
                    None => "AI 자막: 자동".to_string(),
                },
            },
        };
        #[derive(PartialEq, Clone, Copy)]
        enum Pick {
            File,
            Ai(AiLang),
            Off,
        }
        let current = match self.mode {
            Mode::File => Pick::File,
            Mode::Ai => Pick::Ai(self.ai_lang),
            Mode::Off => Pick::Off,
        };
        let mut choice = current;
        let file_name = self.file_subs.as_ref().map(|f| f.name.clone());
        egui::ComboBox::from_id_salt("subs").selected_text(label).width(220.0).show_ui(ui, |ui| {
            if let Some(name) = &file_name {
                ui.selectable_value(&mut choice, Pick::File, format!("파일: {name}"));
            }
            ui.selectable_value(&mut choice, Pick::Ai(AiLang::Auto), "AI 자막: 자동");
            ui.selectable_value(&mut choice, Pick::Ai(AiLang::Fixed(Lang::En)), "AI 자막: 영어");
            ui.selectable_value(&mut choice, Pick::Ai(AiLang::Fixed(Lang::Ja)), "AI 자막: 일본어");
            ui.selectable_value(&mut choice, Pick::Off, "끄기");
        });
        if choice != current {
            match choice {
                Pick::File => self.set_mode(Mode::File, ctx),
                Pick::Ai(l) => self.set_ai_lang(l, ctx),
                Pick::Off => self.set_mode(Mode::Off, ctx),
            }
        }
        if self.mode == Mode::Ai {
            ui.checkbox(&mut self.show_source, "원문");
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fs = if self.video.is_fullscreen() { "창 모드" } else { "전체 화면" };
            if ui.add(egui::Button::new(fs).frame(false)).clicked() {
                self.toggle_fullscreen();
            }
            let mut vol = self.engine.volume() as f32;
            if ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).show_value(false)).changed() {
                self.engine.set_volume(vol as f64);
                self.touch();
            }
            ui.label("볼륨");
        });
    }
}

impl eframe::App for App {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // 영상 창에서 일어난 일
        let (moved, close, activated) = player::with_state(|s| {
            let r = (s.moved, s.close_requested, s.activated);
            s.moved = false;
            s.activated = false;
            r
        });
        if close {
            self.stop_analysis();
            self.engine.shutdown();
            self.video.destroy();
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if moved || !self.placed {
            self.placed = true;
            self.video.place_overlay(self.overlay);
            let (w, h) = self.video.client_size();
            self.engine.resize(w, h);
        }
        if activated {
            // 영상 창(제목 표시줄)을 누르면 키보드 입력을 받도록 겹친 창으로 초점을 넘긴다.
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        // 파일을 열어 영상 크기를 알게 되면 창을 영상 비율에 맞춘다(한 번).
        let loaded = std::mem::take(&mut self.engine.events.lock().unwrap().loaded);
        if loaded {
            if let Some((w, h)) = self.engine.video_size() {
                self.video.fit_to_video(w, h);
            }
        }
        let engine_error = self.engine.events.lock().unwrap().error.take();
        if let Some(e) = engine_error {
            let path = self.file.clone().unwrap_or_default();
            self.play_error = Some(crate::media_audio::explain_playback_error(&path, &e));
            self.stop_analysis(); // 재생할 수 없으면 자막도 만들지 않는다.
        }

        self.handle_keys(&ctx);
        if ui.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_down()) {
            self.touch();
        }

        let rect = ui.max_rect();
        let bar_visible = self.file.is_none()
            || self.engine.is_paused()
            || self.dragging.is_some()
            || self.last_activity.elapsed() < HIDE_CONTROLS_AFTER
            || ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y > rect.bottom() - BAR_HEIGHT * 1.5);

        // 영상 위 클릭: 재생/일시정지, 두 번 누르기: 전체 화면
        let body = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - if bar_visible { BAR_HEIGHT } else { 0.0 }));
        let resp = ui.interact(body, ui.scope_id().with("video_area"), Sense::click());
        if resp.double_clicked() {
            self.toggle_fullscreen();
        } else if resp.clicked() {
            self.toggle_play();
        }

        if self.file.is_none() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "동영상 파일을 이 창에 끌어다 놓거나 O 키로 여세요",
                FontId::proportional(22.0),
                Color32::from_gray(200),
            );
        }
        if let Some(e) = &self.play_error {
            let center = rect.center();
            outlined(ui.painter(), e, 22.0, Color32::from_rgb(255, 150, 150), rect.width() * 0.8, center.x, center.y);
        }
        self.draw_subtitles(ui, rect, bar_visible);
        if bar_visible {
            self.draw_bar(ui, rect, &ctx);
        }
        if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2)) {
            self.toast = None;
        }
        // 재생 중에는 자막 시각을 맞추려고 자주 다시 그린다.
        let interval = if self.engine.is_paused() { 250 } else { 50 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_analysis();
        self.engine.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn analysis_thread(
    path: &str,
    from: f64,
    ai_lang: AiLang,
    model: Model,
    store: &Mutex<CueStore>,
    ai: &Mutex<AiState>,
    models: &Mutex<Models>,
    stop: &AtomicBool,
    read: &Mutex<f64>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let status = |s: &str| {
        ai.lock().unwrap().status = s.to_string();
        ctx.request_repaint();
    };
    // 잠금을 match 밖에서 풀어야 한다(안쪽에서 다시 잠근다).
    let known = ai.lock().unwrap().lang;
    let lang = match (ai_lang, known) {
        (AiLang::Off, _) => return Ok(()),
        (AiLang::Fixed(l), _) | (AiLang::Auto, Some(l)) => l,
        (AiLang::Auto, None) => {
            status("음성 언어 확인 중…");
            let id = {
                let mut m = models.lock().unwrap();
                match &m.langid {
                    Some(id) => id.clone(),
                    None => m.langid.insert(Arc::new(LangId::load()?)).clone(),
                }
            };
            let (spoken, _) = id.detect_file(path)?;
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let skip = |msg: &str| {
                let mut a = ai.lock().unwrap();
                a.no_subs = true;
                a.status = msg.to_string();
                ctx.request_repaint();
            };
            let msg = match &spoken {
                Spoken::En => None,
                Spoken::Ja => None,
                Spoken::Ko => Some("한국어 음성: 번역하지 않습니다".to_string()),
                Spoken::Other(code) => Some(format!("지원하지 않는 음성 언어({code}): 자막 메뉴에서 영어·일본어를 고를 수 있습니다")),
                Spoken::NoSpeech => Some("말소리를 찾지 못했습니다".to_string()),
            };
            if let Some(m) = msg {
                skip(&m);
                return Ok(());
            }
            if spoken == Spoken::Ja { Lang::Ja } else { Lang::En }
        }
    };
    ai.lock().unwrap().lang = Some(lang);
    let (recognizer, translator) = {
        let mut m = models.lock().unwrap();
        if !matches!(&m.recognizer, Some((l, _)) if *l == lang) {
            status("음성 인식 모델 불러오는 중…");
            m.recognizer = Some((lang, Arc::new(Recognizer::load(model, lang)?)));
        }
        if m.translator.is_none() {
            status("번역 모델 불러오는 중…");
            m.translator = Some(Arc::new(Translator::load()?));
        }
        (m.recognizer.as_ref().unwrap().1.clone(), m.translator.clone().unwrap())
    };
    status("");
    analyze::run(path, from, lang, recognizer, translator, stop, |p| {
        match p {
            analyze::Progress::Read(t) => *read.lock().unwrap() = t,
            analyze::Progress::Ready(t) => store.lock().unwrap().mark_done(from, t),
            analyze::Progress::Cue(c) => store.lock().unwrap().insert(c),
        }
        ctx.request_repaint();
    })
}

fn lang_name(l: Lang) -> &'static str {
    match l {
        Lang::En => "영어",
        Lang::Ja => "일본어",
    }
}

fn clock(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
