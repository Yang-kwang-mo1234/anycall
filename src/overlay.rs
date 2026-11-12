//! 자막 오버레이(테두리 없는 투명 창, 항상 위, 마우스 통과)와 시작/정지 제어 창.

use crate::asr::Model;
use crate::pipeline::{self, Models, Session, Shared, Source};
use crate::Lang;
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Pos2, Vec2, ViewportBuilder, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FONTS: &[(&str, &str)] = &[
    ("malgun", "C:/Windows/Fonts/malgun.ttf"),
    ("yugothic", "C:/Windows/Fonts/YuGothM.ttc"), // 일본어 원문(가나)용
];
const LINE_TTL: Duration = Duration::from_secs(5);
const KO_SIZE: f32 = 32.0;
const SRC_SIZE: f32 = 20.0;
const OVERLAY_HEIGHT: f32 = 260.0;

pub fn run(model: Model) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("anycall 자막")
            .with_inner_size([340.0, 200.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native("anycall", options, Box::new(move |cc| Ok(Box::new(App::new(cc, model)))))
}

struct App {
    model: Model,
    lang: Lang,
    show_source: Arc<AtomicBool>,
    shared: Arc<Shared>,
    models: Arc<Mutex<Models>>,
    session: Option<Session>,
    overlay_rect: Option<(Pos2, Vec2)>,
}

fn overlay_id() -> ViewportId {
    ViewportId::from_hash_of("overlay")
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, model: Model) -> Self {
        let mut fonts = FontDefinitions::default();
        for (name, path) in FONTS.iter().rev() {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert((*name).into(), Arc::new(FontData::from_owned(bytes)));
                fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, (*name).into());
            }
        }
        cc.egui_ctx.set_fonts(fonts);

        let ctx = cc.egui_ctx.clone();
        let shared = Shared::new(
            false,
            Box::new(move || {
                ctx.request_repaint_of(overlay_id());
                ctx.request_repaint_of(ViewportId::ROOT);
            }),
        );
        Self {
            model,
            lang: Lang::En,
            show_source: Arc::new(AtomicBool::new(false)),
            shared,
            models: Arc::default(),
            session: None,
            overlay_rect: None,
        }
    }

    fn running(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.is_finished())
    }
}

impl eframe::App for App {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // 오버레이 창을 투명하게. 제어 창은 패널이 배경을 칠한다.
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let running = self.running();
        let (status, error) = {
            let s = self.shared.subs.lock().unwrap();
            (s.status.clone(), s.error.clone())
        };
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("원문 언어");
                ui.add_enabled_ui(!running, |ui| {
                    ui.radio_value(&mut self.lang, Lang::En, "영어");
                    ui.radio_value(&mut self.lang, Lang::Ja, "일본어");
                });
            });
            let mut show = self.show_source.load(Ordering::Relaxed);
            if ui.checkbox(&mut show, "원문 함께 보기").changed() {
                self.show_source.store(show, Ordering::Relaxed);
                ui.ctx().request_repaint_of(overlay_id());
            }
            ui.add_space(8.0);
            let label = if running { "정지" } else { "시작" };
            if ui.add_sized([120.0, 32.0], egui::Button::new(label)).clicked() {
                if let Some(s) = self.session.take() {
                    s.stop();
                } else {
                    self.session = Some(pipeline::start(
                        self.lang,
                        self.model,
                        Source::Loopback,
                        self.models.clone(),
                        self.shared.clone(),
                    ));
                }
            }
            ui.add_space(8.0);
            if let Some(e) = error {
                ui.colored_label(Color32::from_rgb(230, 80, 80), e);
            } else if running {
                ui.label(status);
            } else {
                ui.label("정지됨");
            }
        });
        if !running && self.session.as_ref().is_some_and(|s| s.is_finished()) {
            self.session.take().unwrap().stop(); // 오류로 끝난 세션을 거둔다.
        }

        // 오버레이는 모니터 크기를 안 뒤에 띄운다(화면 아래쪽 가운데).
        if self.overlay_rect.is_none() {
            if let Some(m) = ui.input(|i| i.viewport().monitor_size) {
                let size = Vec2::new(m.x * 0.8, OVERLAY_HEIGHT);
                let pos = Pos2::new(m.x * 0.1, m.y - OVERLAY_HEIGHT - 70.0);
                self.overlay_rect = Some((pos, size));
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }
        if let Some((pos, size)) = self.overlay_rect {
            let shared = self.shared.clone();
            let show_source = self.show_source.clone();
            ui.ctx().show_viewport_deferred(
                overlay_id(),
                ViewportBuilder::default()
                    .with_title("anycall overlay")
                    .with_position(pos)
                    .with_inner_size(size)
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_has_shadow(false)
                    .with_always_on_top()
                    .with_mouse_passthrough(true)
                    .with_taskbar(false)
                    .with_resizable(false)
                    .with_active(false),
                move |ui, _| draw_overlay(ui, &shared, show_source.load(Ordering::Relaxed)),
            );
        }
    }

    fn on_exit(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop();
        }
    }
}

/// 아래에서 위로: 진행 중인 줄(흐림), 그 위에 확정 줄(흰색, 5초 뒤 사라짐).
fn draw_overlay(ui: &mut egui::Ui, shared: &Shared, show_source: bool) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let wrap = rect.width() - 40.0;
    let mut y = rect.bottom() - 8.0;
    let mut soonest: Option<Duration> = None;

    let s = shared.subs.lock().unwrap();
    let mut rows: Vec<(String, String, bool)> = Vec::new(); // (한국어, 원문, 확정)
    for line in &s.lines {
        let age = line.at.map(|t| t.elapsed()).unwrap_or_default();
        if age < LINE_TTL {
            soonest = Some(soonest.map_or(LINE_TTL - age, |d| d.min(LINE_TTL - age)));
            rows.push((line.ko.clone(), line.src.clone(), true));
        }
    }
    let cur = &s.current;
    if !cur.ko.is_empty() || !cur.src.is_empty() {
        // 번역이 아직 없으면 원문 부분 텍스트를 먼저 보여 준다.
        let ko = if cur.ko.is_empty() { cur.src.clone() } else { cur.ko.clone() };
        rows.push((ko, cur.src.clone(), false));
    }
    drop(s);

    for (ko, src, done) in rows.iter().rev() {
        let color = if *done { Color32::WHITE } else { Color32::from_rgb(200, 200, 160) };
        y = outlined(painter, ko, KO_SIZE, color, wrap, rect.center().x, y);
        if show_source && !src.is_empty() && src != ko {
            y = outlined(painter, src, SRC_SIZE, Color32::from_gray(190), wrap, rect.center().x, y);
        }
        y -= 6.0;
        if y < rect.top() {
            break;
        }
    }
    if let Some(d) = soonest {
        ui.ctx().request_repaint_after(d + Duration::from_millis(50));
    }
}

/// 검은 외곽선을 두른 글자를 `bottom` 위에 줄마다 가운데 정렬로 그리고, 그린 영역의 윗변을 돌려준다.
fn outlined(painter: &egui::Painter, text: &str, size: f32, color: Color32, wrap: f32, center_x: f32, bottom: f32) -> f32 {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), FontId::proportional(size), color, wrap);
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    let pos = Pos2::new(center_x - galley.rect.center().x, bottom - galley.rect.max.y);
    let r = 2.0;
    for (dx, dy) in [(-r, 0.0), (r, 0.0), (0.0, -r), (0.0, r), (-r, -r), (r, r), (-r, r), (r, -r)] {
        painter.galley_with_override_text_color(pos + Vec2::new(dx, dy), galley.clone(), Color32::BLACK);
    }
    let top = pos.y + galley.rect.min.y;
    painter.galley(pos, galley, color);
    top
}
