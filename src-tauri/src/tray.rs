//! 系統托盤模組
//!
//! 建立跨平台系統托盤圖標及右鍵菜單（顯示、隱藏、結束程式）。
//!
//! - **Windows**：雙擊托盤圖標恢復窗口；左鍵/右鍵顯示菜單
//! - **macOS**：單擊托盤圖標恢復窗口；右鍵顯示菜單；使用 template 圖標
//! - **Linux**：雙擊托盤圖標恢復窗口

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// 顯示主窗口（從托盤恢復）
///
/// 如果窗口被最小化則先還原，然後顯示並聚焦。
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 建立系統托盤並註冊事件處理
pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // ── 建立右鍵菜單 ──────────────────────────────────
    let show_item = MenuItemBuilder::new("顯示主窗口")
        .id("tray_show")
        .build(app)?;
    let hide_item = MenuItemBuilder::new("隱藏窗口")
        .id("tray_hide")
        .build(app)?;
    let quit_item = MenuItemBuilder::new("退出")
        .id("tray_quit")
        .build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show_item)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&hide_item)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&quit_item)
        .build()?;

    // ── 根據平台選擇托盤圖標 ──────────────────────────
    //
    // macOS  使用 template 圖標（iconTemplate.png），系統自動適配深色/淺色模式
    // Windows 使用默認窗口圖標（由 bundle.icon 中的 .ico 提供）
    // Linux   使用默認窗口圖標（由 bundle.icon 中的 .png 提供）

    #[cfg(target_os = "macos")]
    let icon = app
        .path()
        .resource_dir()
        .ok()
        .and_then(|dir| std::fs::read(dir.join("icons/iconTemplate.png")).ok())
        .and_then(|bytes| tauri::image::Image::from_bytes(&bytes).ok());

    #[cfg(not(target_os = "macos"))]
    let icon = app.default_window_icon().cloned();

    // ── 構建托盤 ──────────────────────────────────────
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("飛牛影視")
        .menu(&menu);

    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }

    // macOS: 啟用 template 模式 + 左鍵不彈菜單（改為顯示窗口）
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
        builder.set_show_menu_on_left_click(false);
    }

    // ── 菜單事件處理 ──────────────────────────────────
    builder = builder.on_menu_event(|app, event| match event.id().as_ref() {
        "tray_show" => show_main_window(app),
        "tray_hide" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        "tray_quit" => app.exit(0),
        _ => {}
    });

    // ── 托盤圖標點擊事件 ──────────────────────────────
    //
    // macOS: 處理單擊（Click）→ 顯示窗口
    // Windows/Linux: 處理雙擊（DoubleClick）→ 顯示窗口
    #[cfg(target_os = "macos")]
    {
        builder = builder.on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                show_main_window(tray.app_handle());
            }
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_main_window(tray.app_handle());
            }
        });
    }

    builder.build(app)?;
    log::info!("系統托盤建立成功");
    Ok(())
}
