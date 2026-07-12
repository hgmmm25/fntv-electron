//! 飛牛影視 Tauri v2 應用入口
//!
//! 職責：
//! 1. 初始化 Tauri 插件（shell 用於 sidecar）
//! 2. 啟動 Go proxy sidecar 並註冊守護程序（含防孤兒機制）
//! 3. 註冊 MPV 播放器 commands
//! 4. 註冊 Frontend Bridge commands（play_movie, get_play_button_config, log_message）
//! 5. 在主視窗載入前注入初始化腳本（src-tauri/inject/preload.iife.js）
//! 6. 處理所有退出路徑（正常退出、托盤退出、關閉按鈕、異常退出），
//!    確保 Go sidecar 被徹底 kill，不留孤兒進程。
//!
//! ## 防孤兒進程設計（第二輪重構）
//!
//! 核心改動：`proxy_daemon::ChildCell` 在 `setup()` 最前面就被 `manage()`，
//! 然後才啟動 sidecar。這樣不論什麼時候收到退出請求，都能可靠地取得子進程句柄。
//!
//! 退出路徑覆蓋：
//! - `RunEvent::ExitRequested` — 用戶點 X / tray 退出 / app.exit() 時觸發
//! - `RunEvent::Exit` — 進程真正退出前的最後一道保障

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

    // 預先建立共享子進程容器（防孤兒機制的核心）
    let child_cell = proxy_daemon::new_child_cell();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            // ── 共享子進程容器（最先 manage，確保退出處理隨時可取）────
            app.manage(child_cell.clone());

            let handle = app.handle().clone();

            // ── 管理狀態 ──────────────────────────────────────

            // MPV 播放器狀態
            handle.manage(mpv::commands::MpvPlayerState {
                player: Arc::new(RwLock::new(mpv_player)),
            });

            // ── Proxy 守護程序 ────────────────────────────────
            //
            // 注意：這裡在 setup() 中同步啟動 proxy（透過 async_runtime::spawn），
            // 但 child_cell 已經在上面 manage()，所以即使 sidecar 啟動、
            // daemon 還沒 manage 完，退出處理也能透過 ChildCell kill 子進程。
            let proxy_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match proxy_daemon::start_proxy_daemon(&proxy_handle, child_cell.clone()).await {
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

    // ── 退出處理 ─────────────────────────────────────────────────
    //
    // 覆蓋兩種退出事件，確保不論哪條路徑退出都能清理子進程：
    //
    // `ExitRequested`：用戶點 X、托盤選退出、程式內部呼叫 app.exit() 時觸發。
    //   這裡做「軟清理」——通知 MPV 退出、kill sidecar。
    //
    // `Exit`：進程真正退出前的最後一道保障。
    //   這裡做「硬 kill」——不論前面是否已清理，再次嘗試 kill sidecar。
    //   這涵蓋了：ExitRequested 被 skip（e.g., prevent_close）、
    //   block_on 阻塞、runtime 拆除導致的異常退出等邊界情況。
    //
    // 兩個 handler 都是「最多做一次 kill」——因為 ChildCell 用 Option + Mutex，
    // 第一次 take() 取出後 cell 為 None，第二次 take() 直接拿到 None，無副作用。
    app.run(|app_handle, event| {
        match event {
            tauri::RunEvent::ExitRequested { .. } => {
                log::info!("收到 ExitRequested，執行資源清理...");

                // 1. 關閉 MPV 播放器（嘗試 tokio，失敗則略過——MPV 有 kill_on_drop）
                let _ = tauri::async_runtime::block_on(async {
                    if let Some(mpv_state) =
                        app_handle.try_state::<mpv::commands::MpvPlayerState>()
                    {
                        let player = mpv_state.player.clone();
                        let mut p = player.write().await;
                        p.shutdown().await;
                    }
                });

                // 2. 同步 kill Go sidecar（不依賴 tokio runtime）
                proxy_daemon::kill_sidecar_sync(app_handle);
            }
            tauri::RunEvent::Exit => {
                log::info!("收到 Exit 事件，執行最終清理...");

                // 最後一道保障：再次嘗試 kill sidecar
                // （如果 ExitRequested 已經 kill 過，這裡 take() 返回 None，無副作用）
                proxy_daemon::kill_sidecar_sync(app_handle);

                log::info!("最終清理完成");
            }
            _ => {}
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
