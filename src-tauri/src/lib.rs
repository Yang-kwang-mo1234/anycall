mod stt;

use tauri::{AppHandle, State};

#[tauri::command]
fn start_listening(app: AppHandle, stt: State<'_, stt::Stt>) {
    stt.start(app);
}

#[tauri::command]
fn stop_listening(stt: State<'_, stt::Stt>) {
    stt.stop();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(stt::Stt::default())
        .invoke_handler(tauri::generate_handler![start_listening, stop_listening])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
