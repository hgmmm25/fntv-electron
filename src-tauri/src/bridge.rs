//! Frontend Bridge Commands
//!
//! 這些 `#[tauri::command]` 對應被注入的 `inject/` 腳本透過
//! `window.__TAURI_INTERNALS__.invoke(...)` 呼叫的指令，取代原本 Electron
//! 透過 `ipcRenderer` 處理的 `play-movie` / `get-play-button-config` /
//! `log-message` 等通道。
//!
//! - `play_movie`：接收前端播放請求，記錄後（後續）轉發給 MPV player
//! - `get_play_button_config`：回傳播放按鈕顯示設定
//! - `log_message`：記錄前端日誌到 Rust log
//!
//! 注意：`play_movie` 目前為 stub，完整的 FN API 呼叫 + 播放列表建構
//! 邏輯（對應 Electron 的 `src/main/handlers/plugins/media.ts`）尚待移植。

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::mpv::commands::MpvPlayerState;

/// 前端透過 `playMovie({ id, token, sourceIndex })` 傳入的播放請求
///
/// 透過 `invoke('play_movie', { payload: { ... } })` 的單一 payload 參數接收，
/// 所以這裡用 `#[serde(rename_all = "camelCase")]` 對應 JS 端的 camelCase 欄位。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayMoviePayload {
    pub id: String,
    pub token: String,
    pub source_index: i64,
}

/// 播放按鈕設定（對應前端的 `{ hideOriginalPlayButton }`）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayButtonConfig {
    pub hide_original_play_button: bool,
}

/// `play_movie` command — 接收前端播放請求
///
/// 對應 Electron 的 `ipcMain.on('play-movie', handlePlayMovie)`。
/// TODO: 移植完整的播放流程（FN API 取得播放資訊 → 建構播放列表 → MPV 播放）。
#[tauri::command]
pub async fn play_movie(
    _state: State<'_, MpvPlayerState>,
    payload: PlayMoviePayload,
) -> Result<(), String> {
    log::info!(
        "[bridge] play_movie: id={}, source_index={}",
        payload.id,
        payload.source_index
    );
    // token 不記錄到日誌，避免洩漏
    log::debug!("[bridge] play_movie token 長度: {}", payload.token.len());

    // TODO: 實作完整播放流程：
    // 1. 從設定檔讀取 server domain
    // 2. 呼叫 FN API getPlayInfo(id)
    // 3. 依 type (Episode/Video/單集) 建構播放列表
    // 4. 取得/啟動 MPV player 並 load_playlist
    // 5. 透過 proxy URL 注入 token / sourceIndex

    Ok(())
}

/// `get_play_button_config` command — 回傳播放按鈕顯示設定
///
/// 對應 Electron 的 `get-play-button-config` + `play-button-config-info` 來回。
/// Tauri 改為單一 invoke 同步回傳。
/// 從 config.json 讀取實際值（對應 fnConfig.getHideOriginalPlayButton()）。
#[tauri::command]
pub async fn get_play_button_config(app: tauri::AppHandle) -> Result<PlayButtonConfig, String> {
    let hide = crate::config::get_hide_original_play_button(app)?;
    Ok(PlayButtonConfig {
        hide_original_play_button: hide,
    })
}

/// `log_message` command — 記錄前端日誌
///
/// 對應 Electron 的 `ipcMain.handle('log-message', (e, level, ...args))`。
/// 前端將任意數量的引數序列化為字串陣列後傳入。
#[tauri::command]
pub async fn log_message(level: String, args: Vec<String>) -> Result<(), String> {
    let joined = args.join(" ");
    match level.as_str() {
        "debug" => log::debug!("[frontend] {joined}"),
        "info" => log::info!("[frontend] {joined}"),
        "warn" => log::warn!("[frontend] {joined}"),
        "error" => log::error!("[frontend] {joined}"),
        _ => log::info!("[frontend] {joined}"),
    }
    Ok(())
}

/// `window_minimize` command — 最小化主視窗
///
/// 遠端頁面不能直接呼叫 Tauri window plugin，而是透過此窄介面操作視窗。
#[tauri::command]
pub async fn window_minimize(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.minimize().map_err(|e| e.to_string())
    } else {
        Err("找不到主視窗".to_string())
    }
}

/// `window_toggle_maximize` command — 切換最大化
#[tauri::command]
pub async fn window_toggle_maximize(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_maximized().unwrap_or(false) {
            window.unmaximize().map_err(|e| e.to_string())
        } else {
            window.maximize().map_err(|e| e.to_string())
        }
    } else {
        Err("找不到主視窗".to_string())
    }
}

/// `window_close` command — 關閉主視窗
#[tauri::command]
pub async fn window_close(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.close().map_err(|e| e.to_string())
    } else {
        Err("找不到主視窗".to_string())
    }
}
