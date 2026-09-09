//! 系統托盤模組
//!
//! 建立跨平台系統托盤圖標及右鍵菜單（顯示、隱藏、結束程式）。
//!
//! - **Windows**：雙擊托盤圖標恢復窗口；左鍵/右鍵顯示菜單
//! - **macOS**：單擊托盤圖標恢復窗口；右鍵顯示菜單；使用 template 圖標
//! - **Linux**：雙擊托盤圖標恢復窗口
//!
//! 額外提供「使用 MPV 播放」勾選項（F3：MPV 預設播放與播放偏好）。
//! 對應上游托盘 tray.ts 的 setHideOriginalPlayButton 切換；勾選後寫入
//! config.hide_original_play_button = true，並重新載入主窗口使注入層
//! （inject）立即按新偏好決定是否接管播放按鈕。

use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
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

    // F3：播放偏好 — MPV 播放接管开关（对应上游 playbackPreference）
    // 默认值跟随 config.get_hide_original_play_button（本地默认 true = MPV）
    let mpv_checked = crate::config::get_hide_original_play_button(app.clone())
        .unwrap_or(true);
    let mpv_item = CheckMenuItemBuilder::new("使用 MPV 播放")
        .id("tray_toggle_mpv")
        .checked(mpv_checked)
        .build(app)?;

    let quit_item = MenuItemBuilder::new("退出")
        .id("tray_quit")
        .build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show_item)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&hide_item)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&mpv_item)
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
    // 注意：需要操作「使用 MPV 播放」勾选状态，因此把 mpv_item 句柄
    // move 进闭包（Tauri 2.11 的 TrayIcon 无 menu() getter）。
    let mpv_item_for_event = mpv_item.clone();
    builder = builder.on_menu_event(move |app, event| match event.id().as_ref() {
        "tray_show" => show_main_window(app),
        "tray_hide" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        // F3：切换播放偏好（MPV ⇄ 网页）。勾选状态变更后写回配置并 reload
        // 主窗口，使 inject 立即按新偏好决定是否接管播放按钮。
        "tray_toggle_mpv" => {
            let new_checked = !mpv_item_for_event.is_checked().unwrap_or(false);
            let _ = mpv_item_for_event.set_checked(new_checked);
            let _ = crate::config::set_hide_original_play_button(
                app.clone(),
                new_checked,
            );
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.location.reload();");
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
