//! 飛牛影視 Tauri v2 應用入口
//!
//! 職責：
//! 1. 初始化 Tauri 插件（shell 用於 sidecar）
//! 2. 啟動 Go proxy sidecar 並註冊守護程序
//! 3. 註冊 MPV 播放器 commands
//! 4. 註冊 Frontend Bridge commands（play_movie, get_play_button_config, log_message）
//! 5. 在主視窗載入前注入初始化腳本（src-tauri/inject/preload.iife.js）

mod auth;
mod bridge;
mod config;
mod mpv;
mod proxy_daemon;
mod tray;
mod winctrl;

use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

/// Tauri 應用入口
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 建立預設的 MpvPlayer（尚未啟動，需前端呼叫 mpv_launch）
    let mpv_player = mpv::player::MpvPlayer::new(mpv::player::MpvConfig::default());

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // ── 管理狀態 ──────────────────────────────────────
            let handle = app.handle().clone();

            // MPV 播放器狀態
            handle.manage(mpv::commands::MpvPlayerState {
                player: Arc::new(RwLock::new(mpv_player)),
            });

            // ── Proxy 守護程序 ────────────────────────────────
            let proxy_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match proxy_daemon::start_proxy_daemon(&proxy_handle).await {
                    Ok(daemon) => {
                        proxy_handle.manage(daemon);
                        log::info!("Proxy 守護程序已就緒");
                    }
                    Err(e) => {
                        log::error!("Proxy 守護程序啟動失敗: {e}");
                    }
                }
            });

            // ── 系統托盤 ───────────────────────────────────
            if let Err(e) = tray::setup_tray(app.handle()) {
                log::error!("系統托盤建立失敗: {e}");
            }

            // ── 注入初始化腳本 ────────────────────────────────
            //
            // 讀取由 `scripts/build-inject.mjs` 打包的 IIFE JS，
            // 透過 Tauri 的 initialization_script API 在頁面 DOMReady 前注入。
            //
            // 使用 runtime 讀取而非 include_str!，這樣：
            // 1. 開發時不需要每次改 Rust 就重編譯
            // 2. 檔案不存在時降級為空字串（app 仍可運行，只是沒有注入腳本）
            let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
                .unwrap_or_else(|_| ".".to_string());
            let inject_path = std::path::Path::new(&manifest_dir)
                .join("inject")
                .join("preload.iife.js");

            let inject_js = match std::fs::read_to_string(&inject_path) {
                Ok(content) => {
                    log::info!(
                        "成功載入注入腳本: {} ({} bytes)",
                        inject_path.display(),
                        content.len()
                    );
                    content
                }
                Err(e) => {
                    log::warn!(
                        "無法讀取注入腳本 {}: {} (功能降級，無注入腳本)",
                        inject_path.display(),
                        e
                    );
                    String::new()
                }
            };

            // ── 設定檔 Cookie 恢復 ──────────────────────────
            //
            // 啟動時讀取設定檔，若已有 token + domain，在頁面載入後
            // 透過 eval 注入 cookie 設定腳本（對應 Electron 的 setupCookieRestore）。
            let saved_config = config::read_config(&handle);
            if let (Some(domain), Some(token)) =
                (saved_config.domain.clone(), saved_config.token.clone())
            {
                if !token.is_empty() && domain.starts_with("http") {
                    log::info!("偵測到已儲存的登入資訊，準備恢復 cookie: {domain}");
                    let is_https = domain.starts_with("https://");
                    let secure = if is_https { "secure; " } else { "" };
                    let same_site = if is_https { "none" } else { "lax" };
                    let token_escaped = token.replace('\'', "\\'");
                    let cookie_js = format!(
                        "document.cookie='Trim-MC-token={token_escaped}; path=/; {secure}samesite={same_site}';\
                         document.cookie='mode=relay; path=/; {secure}samesite={same_site}';",
                        token_escaped = token_escaped,
                        secure = secure,
                        same_site = same_site,
                    );
                    // 與注入腳本合併，一起 eval
                    // （注入腳本本身不負責 cookie，這裡獨立 eval 一次以確保 cookie 先設定）
                    let combined = format!("{}\n{}", cookie_js, inject_js);

                    if let Some(window) = handle.get_webview_window("main") {
                        if let Err(e) = window.eval(&combined) {
                            log::error!("注入 cookie 腳本失敗: {e}");
                        } else {
                            log::info!("Cookie 恢復腳本已注入");
                        }
                    }
                } else {
                    self_eval_inject(&handle, &inject_js);
                }
            } else {
                log::info!("無已儲存的登入資訊，跳過 cookie 恢復");
                self_eval_inject(&handle, &inject_js);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // MPV 播放控制
            mpv::commands::mpv_launch,
            mpv::commands::mpv_play,
            mpv::commands::mpv_load_playlist,
            mpv::commands::mpv_pause,
            mpv::commands::mpv_resume,
            mpv::commands::mpv_stop,
            mpv::commands::mpv_seek,
            mpv::commands::mpv_set_volume,
            mpv::commands::mpv_set_mute,
            mpv::commands::mpv_get_state,
            mpv::commands::mpv_set_property,
            mpv::commands::mpv_get_property,
            mpv::commands::mpv_shutdown,
            mpv::commands::mpv_playlist_next,
            mpv::commands::mpv_playlist_prev,
            mpv::commands::mpv_playlist_pos,
            mpv::commands::mpv_toggle_fullscreen,
            // Frontend Bridge（對應注入腳本的 invoke 呼叫）
            bridge::play_movie,
            bridge::get_play_button_config,
            bridge::log_message,
            bridge::window_minimize,
            bridge::window_toggle_maximize,
            bridge::window_close,
            // 視窗控制
            winctrl::set_half_screen,
            winctrl::set_full_screen,
            winctrl::toggle_fullscreen,
            // 設定檔管理（對應 Electron fn_config）
            config::get_config,
            config::save_login_config,
            config::get_history,
            config::add_history,
            config::clear_history,
            config::delete_history_item,
            config::get_download_proxy_config,
            config::set_download_proxy_config,
            config::get_hide_original_play_button,
            config::set_hide_original_play_button,
            config::get_nas_proxy_enabled,
            config::set_nas_proxy_enabled,
            config::get_exit_mode,
            config::set_exit_mode,
            config::get_mpv_player_path,
            config::set_mpv_player_path,
            // 登入認證（對應 Electron auth plugin）
            auth::login,
            auth::restore_cookies,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    // 透過 App::run 註冊事件回調，處理退出前的資源關閉
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            log::info!("收到退出請求，關閉資源...");

            // 關閉 MPV 播放器
            if let Some(mpv_state) =
                app_handle.try_state::<mpv::commands::MpvPlayerState>()
            {
                let player = mpv_state.player.clone();
                tauri::async_runtime::block_on(async move {
                    let mut p = player.write().await;
                    p.shutdown().await;
                });
            }

            // 關閉 Proxy 守護程序
            tauri::async_runtime::block_on(
                proxy_daemon::shutdown_proxy_daemon(app_handle),
            );
        }
    });
}

/// 輔助函數：將注入腳本 eval 到主視窗
fn self_eval_inject(handle: &tauri::AppHandle, inject_js: &str) {
    if inject_js.is_empty() {
        return;
    }
    if let Some(window) = handle.get_webview_window("main") {
        if let Err(e) = window.eval(inject_js) {
            log::error!("注入初始化腳本失敗: {e}");
        } else {
            log::info!("初始化腳本已注入到主視窗");
        }
    } else {
        log::warn!("找不到主視窗 (label='main')，跳過初始化腳本注入");
    }
}
