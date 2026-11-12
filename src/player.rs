//! 동영상 재생: Media Foundation Media Engine이 전용 Win32 창에 직접 그린다(HWND 모드).
//!
//! 영상 창은 제목 표시줄이 있는 보통 창이다. 자막과 조작 막대는 그 위에 겹친 투명 egui 창이 그린다
//! (player_ui.rs). 두 창은 UI 스레드(winit 이벤트 루프)에서 만들어지므로 영상 창의 메시지도 winit이 돌린다.

use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{implement, w, Interface, BSTR, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, GetStockObject, MonitorFromWindow, BLACK_BRUSH, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 영상 창에서 일어난 일. UI가 다음 프레임에 처리한다.
#[derive(Default)]
pub struct WindowState {
    /// 영상 창 클라이언트 영역(화면 좌표). 바뀌면 `moved`가 선다.
    pub client: RECT,
    pub moved: bool,
    pub close_requested: bool,
    pub minimized: bool,
    /// 영상 창이 활성화됐다(겹친 창으로 초점을 넘긴다).
    pub activated: bool,
}

struct Globals {
    state: Mutex<WindowState>,
    wake: Box<dyn Fn() + Send + Sync>,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

fn globals() -> &'static Globals {
    GLOBALS.get().expect("player::create_window를 먼저 불러야 한다")
}

/// 영상 창 상태를 읽고 바꾼다.
pub fn with_state<R>(f: impl FnOnce(&mut WindowState) -> R) -> R {
    f(&mut globals().state.lock().unwrap())
}

pub struct VideoWindow {
    pub hwnd: HWND,
    /// 전체 화면 전 창 위치·스타일
    saved: Option<(WINDOWPLACEMENT, i32)>,
}

impl VideoWindow {
    /// 영상 창을 만든다. `wake`는 창에 무슨 일이 생겼을 때 UI를 깨운다.
    pub fn create(title: &str, wake: Box<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let _ = GLOBALS.set(Globals { state: Mutex::default(), wake });
        // SAFETY: Win32 창 만들기. 클래스 이름과 제목은 호출 동안 살아 있다.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: w!("anycall_video"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&class);
            let title = windows::core::HSTRING::from(title);
            // 화면 너비의 60%, 16:9로 시작한다(파일을 열면 영상 비율에 맞춘다).
            let screen_w = GetSystemMetrics(SM_CXSCREEN).max(1024);
            let w = screen_w * 6 / 10;
            let h = w * 9 / 16 + GetSystemMetrics(SM_CYCAPTION) + GetSystemMetrics(SM_CYSIZEFRAME) * 2;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("anycall_video"),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| format!("영상 창을 만들 수 없습니다 ({e})"))?;
            update_client(hwnd);
            Ok(Self { hwnd, saved: None })
        }
    }

    pub fn set_title(&self, title: &str) {
        let t = windows::core::HSTRING::from(title);
        // SAFETY: 살아 있는 창이다.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(t.as_ptr()));
        }
    }

    /// 영상 비율(`vw`:`vh`)에 맞춰 창 높이를 바꾼다. 화면을 넘으면 줄인다. 최대화·전체 화면이면 두지 않는다.
    pub fn fit_to_video(&self, vw: u32, vh: u32) {
        // SAFETY: 살아 있는 창의 크기를 바꾼다.
        unsafe {
            if self.saved.is_some() || IsZoomed(self.hwnd).as_bool() {
                return;
            }
            let (mut win, mut client) = (RECT::default(), RECT::default());
            let _ = GetWindowRect(self.hwnd, &mut win);
            let _ = GetClientRect(self.hwnd, &mut client);
            let (frame_w, frame_h) = ((win.right - win.left) - client.right, (win.bottom - win.top) - client.bottom);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
            let work = mi.rcWork;
            let max_h = ((work.bottom - work.top) as f64 * 0.9) as i32 - frame_h;
            let mut cw = client.right;
            let mut ch = (cw as f64 * vh as f64 / vw as f64).round() as i32;
            if ch > max_h {
                ch = max_h;
                cw = (ch as f64 * vw as f64 / vh as f64).round() as i32;
            }
            let _ = SetWindowPos(self.hwnd, None, 0, 0, cw + frame_w, ch + frame_h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.saved.is_some()
    }

    /// 테두리 없이 모니터를 꽉 채우거나 원래대로 되돌린다.
    pub fn toggle_fullscreen(&mut self) {
        // SAFETY: 살아 있는 창의 스타일·위치를 바꾼다.
        unsafe {
            if let Some((placement, style)) = self.saved.take() {
                SetWindowLongW(self.hwnd, GWL_STYLE, style);
                let _ = SetWindowPlacement(self.hwnd, &placement);
                let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            } else {
                let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.hwnd, &mut placement);
                let style = GetWindowLongW(self.hwnd, GWL_STYLE);
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
                SetWindowLongW(self.hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as i32) | WS_POPUP.0 as i32);
                let r = mi.rcMonitor;
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED);
                self.saved = Some((placement, style));
            }
        }
    }

    /// 겹친 창(`overlay`)을 이 창의 소유 창으로 만든다. 소유 창은 늘 이 창 바로 위에 있고 작업 표시줄에 나오지 않는다.
    pub fn adopt(&self, overlay: HWND) {
        // SAFETY: 두 창 모두 살아 있다. GWLP_HWNDPARENT는 최상위 창의 소유자를 바꾼다.
        unsafe {
            SetWindowLongPtrW(overlay, GWLP_HWNDPARENT, self.hwnd.0 as isize);
        }
    }

    /// 겹친 창을 영상 창 클라이언트 영역에 맞춘다.
    pub fn place_overlay(&self, overlay: HWND) {
        let r = with_state(|s| s.client);
        // SAFETY: 살아 있는 창의 위치를 바꾼다.
        unsafe {
            let _ = SetWindowPos(
                overlay,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    pub fn client_size(&self) -> (i32, i32) {
        let r = with_state(|s| s.client);
        (r.right - r.left, r.bottom - r.top)
    }

    pub fn destroy(&self) {
        // SAFETY: 살아 있는 창을 닫는다.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe fn update_client(hwnd: HWND) {
    let mut r = RECT::default();
    let _ = GetClientRect(hwnd, &mut r);
    let mut tl = POINT { x: r.left, y: r.top };
    let _ = ClientToScreen(hwnd, &mut tl);
    let client = RECT { left: tl.x, top: tl.y, right: tl.x + r.right, bottom: tl.y + r.bottom };
    with_state(|s| {
        if s.client != client {
            s.client = client;
            s.moved = true;
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(g) = GLOBALS.get() else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_MOVE | WM_SIZE => {
            if msg == WM_SIZE {
                let minimized = wparam.0 == SIZE_MINIMIZED as usize;
                with_state(|s| s.minimized = minimized);
            }
            update_client(hwnd);
            (g.wake)();
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF != WA_INACTIVE as usize {
                with_state(|s| s.activated = true);
                (g.wake)();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            with_state(|s| s.close_requested = true);
            (g.wake)();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Media Engine이 알려 준 일
#[derive(Default)]
pub struct EngineEvents {
    pub loaded: bool,
    pub ended: bool,
    pub error: Option<String>,
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    events: Arc<Mutex<EngineEvents>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        let e = MF_MEDIA_ENGINE_EVENT(event as i32);
        {
            let mut ev = self.events.lock().unwrap();
            if e == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA {
                ev.loaded = true;
            } else if e == MF_MEDIA_ENGINE_EVENT_ENDED {
                ev.ended = true;
            } else if e == MF_MEDIA_ENGINE_EVENT_ERROR {
                ev.error = Some(engine_error(param1 as u16, param2));
            }
        }
        (self.wake)();
        Ok(())
    }
}

fn engine_error(code: u16, hr: u32) -> String {
    let what = match MF_MEDIA_ENGINE_ERR(code as i32) {
        MF_MEDIA_ENGINE_ERR_ABORTED => "재생이 중단됐습니다",
        MF_MEDIA_ENGINE_ERR_NETWORK => "파일을 읽을 수 없습니다",
        MF_MEDIA_ENGINE_ERR_DECODE => "영상을 디코딩할 수 없습니다",
        MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED => "지원하지 않는 형식입니다",
        _ => "재생 오류",
    };
    format!("{what} (0x{hr:08X})")
}

pub struct Engine {
    me: IMFMediaEngine,
    ex: IMFMediaEngineEx,
    pub events: Arc<Mutex<EngineEvents>>,
}

impl Engine {
    pub fn new(hwnd: HWND, wake: Box<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        crate::media_audio::init_thread();
        let events = Arc::new(Mutex::new(EngineEvents::default()));
        let notify: IMFMediaEngineNotify = Notify { events: events.clone(), wake }.into();
        let err = |e: windows::core::Error| format!("재생 엔진을 만들 수 없습니다 ({e})");
        // SAFETY: Media Engine 만들기. 속성 값은 이 함수 안에서 만든 올바른 값이다.
        unsafe {
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_ALL).map_err(err)?;
            // 하드웨어 디코딩과 창 출력에 쓸 D3D11 장치. Media Engine이 여러 스레드에서 쓰므로 다중 스레드 보호를 켠다.
            let mut device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .map_err(err)?;
            let device = device.ok_or("D3D11 장치를 만들 수 없습니다")?;
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(err)?;
            let manager = manager.ok_or("DXGI 장치 관리자를 만들 수 없습니다")?;
            manager.ResetDevice(&device, token).map_err(err)?;

            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4).map_err(err)?;
            let attrs = attrs.ok_or("속성을 만들 수 없습니다")?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify).map_err(err)?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager).map_err(err)?;
            attrs.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32).map_err(err)?;
            attrs.SetUINT64(&MF_MEDIA_ENGINE_PLAYBACK_HWND, hwnd.0 as u64).map_err(err)?;
            let me = factory.CreateInstance(0, &attrs).map_err(err)?;
            let ex: IMFMediaEngineEx = me.cast().map_err(err)?;
            Ok(Self { me, ex, events })
        }
    }

    pub fn open(&self, path: &str) -> Result<(), String> {
        *self.events.lock().unwrap() = EngineEvents::default();
        // SAFETY: Media Engine 호출
        unsafe {
            self.me.SetSource(&BSTR::from(path)).map_err(|e| format!("파일을 열 수 없습니다 ({e})"))?;
            self.me.SetAutoPlay(true).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn play(&self) {
        unsafe {
            let _ = self.me.Play();
        }
    }

    pub fn pause(&self) {
        unsafe {
            let _ = self.me.Pause();
        }
    }

    pub fn is_paused(&self) -> bool {
        unsafe { self.me.IsPaused().as_bool() }
    }

    pub fn is_ended(&self) -> bool {
        unsafe { self.me.IsEnded().as_bool() }
    }

    pub fn time(&self) -> f64 {
        unsafe { self.me.GetCurrentTime() }
    }

    pub fn duration(&self) -> f64 {
        let d = unsafe { self.me.GetDuration() };
        if d.is_finite() {
            d
        } else {
            0.0
        }
    }

    pub fn seek(&self, secs: f64) {
        let d = self.duration();
        let t = if d > 0.0 { secs.clamp(0.0, d - 0.05) } else { secs.max(0.0) };
        unsafe {
            let _ = self.ex.SetCurrentTimeEx(t, MF_MEDIA_ENGINE_SEEK_MODE_NORMAL);
        }
    }

    pub fn volume(&self) -> f64 {
        unsafe { self.me.GetVolume() }
    }

    pub fn set_volume(&self, v: f64) {
        unsafe {
            let _ = self.me.SetVolume(v.clamp(0.0, 1.0));
        }
    }

    pub fn video_size(&self) -> Option<(u32, u32)> {
        let (mut w, mut h) = (0u32, 0u32);
        unsafe {
            let _ = self.me.GetNativeVideoSize(Some(&mut w), Some(&mut h));
        }
        (w > 0 && h > 0).then_some((w, h))
    }

    /// 창 크기가 바뀌면 영상을 다시 맞춘다(가로세로 비는 엔진이 지킨다).
    pub fn resize(&self, width: i32, height: i32) {
        let dst = RECT { left: 0, top: 0, right: width, bottom: height };
        let black = MFARGB { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbAlpha: 255 };
        unsafe {
            let _ = self.ex.UpdateVideoStream(None, Some(&dst), Some(&black));
        }
    }

    pub fn shutdown(&self) {
        unsafe {
            let _ = self.me.Shutdown();
        }
    }
}

/// 파일 열기 대화상자. 고른 파일의 경로를 돌려준다(취소하면 None).
pub fn open_file_dialog(owner: HWND) -> Option<String> {
    use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
    // SAFETY: 셸 대화상자 호출. 필터 문자열은 호출 동안 살아 있는 정적 문자열이다.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("동영상"), pszSpec: w!("*.mp4;*.mkv;*.webm;*.avi;*.mov;*.m4v;*.wmv;*.ts;*.m2ts;*.mp3;*.m4a;*.flac;*.wav") },
            COMDLG_FILTERSPEC { pszName: w!("자막"), pszSpec: w!("*.srt;*.smi;*.sami;*.vtt;*.ass;*.ssa") },
            COMDLG_FILTERSPEC { pszName: w!("모든 파일"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetTitle(w!("동영상 또는 자막 열기"));
        if let Ok(o) = dialog.GetOptions() {
            let _ = dialog.SetOptions(o | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        }
        dialog.Show(Some(owner)).ok()?; // 취소하면 오류가 온다.
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}
