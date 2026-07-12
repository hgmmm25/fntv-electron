//! 飛牛影視 Tauri v2 應用入口
//!
//! 職責：
//! 1. 初始化 Tauri 插件（shell 用於 sidecar）
//! 2. 啟動 Go proxy sidecar 並註冊守護程序（含防孤兒機制）
//! 3. 註冊 MPV 播放器 commands
//! 4. 註冊 Frontend Bridge commands（play_movie, get_play_button_config, log_message）
//! 5. 透過 initialization_script API 注入初始化腳本（src-tauri/inject/preload.iife.js）
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
mod fn_api;
mod mpv;
mod proxy_daemon;
mod tray;
mod winctrl;
mod snap_layout;

use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

/// 由 `build-inject.mjs` 打包的注入腳本（編譯時嵌入）
///
/// 使用 `include_str!` 而非 runtime 讀取，確保：
/// 1. 生產環境不需要 CARGO_MANIFEST_DIR hack
/// 2. Tauri dev 的文件監聽能偵測到 preload.iife.js 變更並觸發重編譯
/// 3. 每次 cargo build 都會重新讀取最新的打包結果
const INJECT_JS: &str = include_str!("../inject/preload.iife.js");

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

            // ── 注入初始化腳本 + Cookie 恢復 ─────────────────
            //
            // 將 Cookie 恢復腳本與注入腳本（由 include_str! 編譯時嵌入）
            // 合併為單一初始化腳本，透過 initialization_script API 注入。
            //
            // initialization_script 的優勢：
            // 1. 在頁面任何腳本之前執行（類似 HTML <head> 最前面的 <script>）
            // 2. 每次頁面導航/重新整理都會自動重新執行
            // 3. 無需手動呼叫 eval()，時序問題歸零
            let saved_config = config::read_config(&handle);
            let cookie_js = if let (Some(domain), Some(token)) =
                (saved_config.domain.clone(), saved_config.token.clone())
            {
                if !token.is_empty() && domain.starts_with("http") {
                    log::info!("偵測到已儲存的登入資訊，準備恢復 cookie: {domain}");
                    let is_https = domain.starts_with("https://");
                    let secure = if is_https { "secure; " } else { "" };
                    let same_site = if is_https { "none" } else { "lax" };
                    let token_escaped = token.replace('\'', "\\'");
                    format!(
                        "document.cookie='Trim-MC-token={token_escaped}; path=/; {secure}samesite={same_site}';\
                         document.cookie='mode=relay; path=/; {secure}samesite={same_site}';",
                    )
                } else {
                    String::new()
                }
            } else {
                log::info!("無已儲存的登入資訊，跳過 cookie 恢復");
                String::new()
            };

            let init_script = if cookie_js.is_empty() {
                INJECT_JS.to_string()
            } else {
                format!("{cookie_js}\n{INJECT_JS}")
            };

            log::info!(
                "注入腳本已準備 ({} bytes, 含{} cookie 恢復)",
                init_script.len(),
                if cookie_js.is_empty() { "無" } else { "" },
            );

            // ── 建立主視窗（程式化建立，非 tauri.conf.json）──────────
            //
            // 從 tauri.conf.json 的 build.dev_url / frontend_dist 決定 URL，
            // 並透過 initialization_script 將注入腳本掛載到 webview。
            //
            // 不在 tauri.conf.json 定義 windows，而是程式化建立，是因為
            // initialization_script() 必須在 builder 階段掛上——程式化建立
            // 讓我們可以在 setup() 裡動態組裝「cookie 恢復 + 注入腳本」後再注入。
            // WebviewUrl::App 會根據環境自動路由：
            // - dev 模式：Tauri 自動導向 dev_url（localhost:3000）
            // - build 模式：從 frontend_dist（resource/login）載入靜態資源
            let url = tauri::WebviewUrl::App("index.html".into());

            let _window = tauri::WebviewWindowBuilder::new(app, "main", url)
                .title("飞牛影视")
                .inner_size(1200.0, 800.0)
                .min_inner_size(800.0, 600.0)
                .resizable(true)
                .decorations(false)
                .maximized(true)
                .initialization_script(&init_script)
                .build()
                .expect("建立主視窗失敗");

            log::info!("主視窗已建立（含 initialization_script）");

            // ── Windows 11 Snap Layouts 支援 ──────────────────
            //
            // 無邊框模式下，透過 WM_NCHITTEST 攔截讓自訂最大化按鈕
            // 能觸發 Windows 11 的 Snap Layouts 懸停選單。
            // 僅在 Windows 平台生效，其他平台為 no-op。
            snap_layout::setup(app);

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
            proxy_daemon::get_proxy_base_url,
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
