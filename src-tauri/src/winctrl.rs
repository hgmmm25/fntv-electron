//! 視窗控制命令
//!
//! 對應 Electron 的 `winctrl.ts`，提供窗口大小和狀態控制。
//! 這些 `#[tauri::command]` 可透過前端 `invoke()` 呼叫。

use tauri::Manager;

/// 設置窗口為半屏（1200×800 並居中）
///
/// 對應 Electron 的 `setHalfScreen()`。
#[tauri::command]
pub fn set_half_screen(app: tauri::AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("找不到主視窗")?;
    window
        .set_size(tauri::LogicalSize::new(1200.0, 800.0))
        .map_err(|e| e.to_string())?;
    window.center().map_err(|e| e.to_string())?;
    window.unmaximize().map_err(|e| e.to_string())?;
    Ok(())
}

/// 設置窗口為全屏（最大化）
///
/// 對應 Electron 的 `setFullScreen()`。
#[tauri::command]
pub fn set_full_screen(app: tauri::AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("找不到主視窗")?;
    window.maximize().map_err(|e| e.to_string())
}

/// 切換全屏（半屏 ↔ 全屏）
///
/// 在半屏（1200×800）和最大化之間切換，對應 Electron 的 F11 切換邏輯。
#[tauri::command]
pub fn toggle_fullscreen(app: tauri::AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("找不到主視窗")?;
    if window.is_maximized().unwrap_or(false) {
        // 當前最大化 → 恢復半屏
        window
            .set_size(tauri::LogicalSize::new(1200.0, 800.0))
            .map_err(|e| e.to_string())?;
        window.center().map_err(|e| e.to_string())?;
        window.unmaximize().map_err(|e| e.to_string())?;
    } else {
        // 當前非最大化 → 最大化
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(())
}
