//! Frontend Bridge Commands
//!
//! 這些 `#[tauri::command]` 對應被注入的 `inject/` 腳本透過
//! `window.__TAURI___.core.invoke(...)` 呼叫的指令。
//!
//! - `play_movie`：完整播放流程（FN API → 播放列表 → MPV）
//! - `get_play_button_config`：回傳播放按鈕顯示設定
//! - `log_message`：記錄前端日誌
//! - `window_*`：視窗控制

use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::config;
use crate::fn_api::{fn_request, HttpMethod};
use crate::mpv::commands::MpvPlayerState;
use crate::mpv::player::MpvConfig;
use crate::mpv::protocol::PlayListItem;
use crate::proxy_daemon::ProxyDaemon;

/// 前端透過 `playMovie({ id, token, sourceIndex })` 傳入的播放請求
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

// ═══════════════════════════════════════════════════════════════
// play_movie — 完整播放流程
// ═══════════════════════════════════════════════════════════════

/// 產生代理播放 URL（對應 Electron `createProxyPlaybackUrl`）
///
/// MPV 透過此 URL 向本地 Go proxy 拉流，Go proxy 依 session
/// 解析出 token/account/domain 後再向 FN 伺服器取資料。
fn create_proxy_playback_url(
    proxy_base: &str,
    session: &str,
    item_guid: &str,
    source_index: i64,
) -> String {
    let encoded_guid =
        percent_encoding::utf8_percent_encode(item_guid, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    if source_index > 0 {
        format!(
            "{proxy_base}/api/v1/playvideo/{encoded_guid}?session={session}&sourceIndex={source_index}"
        )
    } else {
        format!("{proxy_base}/api/v1/playvideo/{encoded_guid}?session={session}")
    }
}

/// 向本地 Go proxy 註冊播放 session（對應 Electron `registerPlaybackSession`）
async fn register_proxy_session(
    proxy_base: &str,
    secret: &str,
    token: &str,
    cfg: &config::AppConfig,
    item_guids: &[String],
) -> Result<String, String> {
    let body = serde_json::json!({
        "token": token,
        "account": cfg.account.as_deref().unwrap_or(""),
        "domain": cfg.domain.as_deref().unwrap_or(""),
        "accessCookie": crate::access_code::get_access_cookie_header(cfg.domain.as_deref().unwrap_or("")),
        "skipVerify": true, // Tauri 版統一跳過憑證驗證（與原 skipVerify=1 一致）
        "useNasLocal": cfg.nas_proxy_enabled.unwrap_or(false),
        "itemGuids": item_guids,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_base}/api/v1/session"))
        .header("Content-Type", "application/json")
        .header("X-FNTV-Proxy-Secret", secret)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Proxy 建立播放 session 失敗: {e}"))?;

    if resp.status() != reqwest::StatusCode::CREATED {
        return Err(format!(
            "Proxy 建立播放 session 失敗 (HTTP {})",
            resp.status().as_u16()
        ));
    }

    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Proxy 播放 session 回應解析失敗: {e}"))?;
    let session = payload
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let is_valid = session.len() == 64
        && session
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if !is_valid {
        return Err("Proxy 回傳了無效的播放 session".to_string());
    }
    Ok(session)
}

/// 從 JSON 值中安全提取字串欄位
fn json_str(val: &serde_json::Value, key: &str) -> String {
    val.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// 從 JSON 值中安全提取數值欄位（i64 → f64）
fn json_f64(val: &serde_json::Value, key: &str) -> f64 {
    val.get(key)
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// 從 JSON 值中安全提取 i64 欄位
fn json_i64(val: &serde_json::Value, key: &str) -> i64 {
    val.get(key)
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// 將 FN API 回應的項目轉為 MPV PlayListItem（對應 Electron 的 processEpisodeMedia）
/// play_link 由呼叫方在註冊 proxy session 後統一填充。
fn process_episode_media(info: &serde_json::Value) -> PlayListItem {
    let guid = json_str(info, "guid");
    PlayListItem {
        item_guid: guid.clone(),
        title: json_str(info, "title"),
        tv_title: json_str(info, "tv_title"),
        season_number: json_i64(info, "season_number") as u32,
        episode_number: json_i64(info, "episode_number") as u32,
        ts: json_f64(info, "ts"),
        duration: json_f64(info, "duration"),
        play_link: String::new(),
    }
}

/// 將單集播放資訊轉為 MPV PlayListItem（對應 Electron 的 processSingleMedia）
/// play_link 由呼叫方在註冊 proxy session 後統一填充。
fn process_single_media(play_info: &serde_json::Value) -> PlayListItem {
    let guid = json_str(play_info, "guid");
    let item = play_info.get("item").cloned().unwrap_or_default();
    PlayListItem {
        item_guid: guid.clone(),
        title: json_str(&item, "title"),
        tv_title: json_str(&item, "tv_title"),
        season_number: json_i64(&item, "season_number") as u32,
        episode_number: json_i64(&item, "episode_number") as u32,
        ts: json_f64(play_info, "ts"),
        duration: json_f64(&item, "duration"),
        play_link: String::new(),
    }
}

/// 尋找 MPV 播放器路徑
///
/// 優先使用 config.mpv_player_path，然後嘗試平台預設路徑。
fn find_mpv_player_path(cfg: &config::AppConfig) -> Result<String, String> {
    // 優先使用設定中的路徑
    if let Some(ref path) = cfg.mpv_player_path {
        if !path.is_empty() {
            return Ok(path.clone());
        }
    }

    let platform = std::env::consts::OS;

    if platform == "windows" {
        // Windows: 相對於 exe 目錄的第三方 MPV
        if let Ok(exe_dir) = std::env::current_exe() {
            if let Some(parent) = exe_dir.parent() {
                let local_path = parent.join("third_party").join("fntv-mpv").join("mpv.exe");
                if local_path.exists() {
                    return Ok(local_path.to_string_lossy().to_string());
                }
            }
        }
        // 嘗試相對於工作目錄
        let cwd_path = PathBuf::from("third_party").join("fntv-mpv").join("mpv.exe");
        if cwd_path.exists() {
            return Ok(cwd_path.to_string_lossy().to_string());
        }
        Err("找不到 MPV 播放器（請在設定中指定路徑或放置 third_party/fntv-mpv/mpv.exe）".to_string())
    } else if platform == "darwin" {
        let paths = [
            "/opt/homebrew/bin/mpv",
            "/usr/local/bin/mpv",
            "/Applications/mpv.app/Contents/MacOS/mpv",
        ];
        for p in &paths {
            if std::path::Path::new(p).exists() {
                return Ok(p.to_string());
            }
        }
        Err("macOS 未找到 mpv，請先安裝: brew install mpv".to_string())
    } else {
        let paths = [
            "/usr/bin/mpv",
            "/usr/local/bin/mpv",
            "/snap/bin/mpv",
        ];
        for p in &paths {
            if std::path::Path::new(p).exists() {
                return Ok(p.to_string());
            }
        }
        Err("Linux 未找到 mpv，請先安裝 mpv 播放器".to_string())
    }
}

/// `play_movie` command — 完整播放流程
///
/// 對應 Electron 的 `handlePlayMovie`，移植自
/// `src/main/handlers/plugins/media.ts:224-338`。
///
/// 流程：
/// 1. 讀取 config（domain / token）
/// 2. 呼叫 FN API getPlayInfo(id)
/// 3. 依 type 建構播放列表（Episode / Video / 單集）
/// 4. 啟動/複用 MPV
/// 5. 載入播放列表並跳轉到目標位置
#[tauri::command]
pub async fn play_movie(
    app: AppHandle,
    mpv_state: State<'_, MpvPlayerState>,
    proxy_daemon: State<'_, ProxyDaemon>,
    payload: PlayMoviePayload,
) -> Result<(), String> {
    log::info!(
        "[bridge] play_movie: id={}, source_index={}",
        payload.id,
        payload.source_index
    );
    log::debug!("[bridge] play_movie token 長度: {}", payload.token.len());

    let cfg = config::read_config(&app);
    let domain = cfg
        .domain
        .as_deref()
        .ok_or("無伺服器地址配置，請先登入")?;

    let token = &payload.token;
    let proxy_base = proxy_daemon.base_url();

    // ── Step 1: 取得播放資訊 ──────────────────────────────
    log::info!("[play_movie] 取得播放資訊: {}/v/api/v1/play/info", domain);

    let play_info_resp = fn_request(
        domain,
        "/v/api/v1/play/info",
        HttpMethod::Post,
        token,
        Some(serde_json::json!({ "item_guid": &payload.id })),
    )
    .await
    .map_err(|e| format!("取得播放資訊失敗: {e}"))?;

    let play_info = play_info_resp
        .data
        .ok_or("FN API 回應中缺少 data 欄位")?;

    let item_type = json_str(&play_info, "type");
    let parent_guid = json_str(&play_info, "parent_guid");
    let item_guid = json_str(&play_info, "guid");

    log::info!(
        "[play_movie] 類型: {}, parent_guid: {}, guid: {}",
        item_type,
        parent_guid,
        item_guid
    );

    // ── Step 2: 依類型建構播放列表 ────────────────────────
    let mut playlist: Vec<PlayListItem> = Vec::new();

    if item_type == "Episode" && !parent_guid.is_empty() {
        log::info!("[play_movie] 劇集模式，取得系列列表...");
        let ep_resp = fn_request(
            domain,
            &format!("/v/api/v1/episode/list/{}", parent_guid),
            HttpMethod::Get,
            token,
            None,
        )
        .await
        .map_err(|e| format!("取得劇集列表失敗: {e}"))?;

        if let Some(episodes) = ep_resp.data {
            if let Some(arr) = episodes.as_array() {
                for ep in arr {
                    let item = process_episode_media(ep);
                    log::debug!("[play_movie] + 劇集: {}", item.title);
                    playlist.push(item);
                }
            }
        }

        if playlist.is_empty() {
            // 嘗試用 item 本身作為單集
            playlist.push(process_episode_media(&play_info));
        }
    } else if item_type == "Video" && !parent_guid.is_empty() {
        log::info!("[play_movie] 其他影片模式，取得同級列表...");
        let item_list_resp = fn_request(
            domain,
            "/v/api/v1/item/list",
            HttpMethod::Post,
            token,
            Some(serde_json::json!({
                "parent_guid": parent_guid,
                "exclude_folder": 1,
                "sort_column": "sort_title",
                "sort_type": "ASC",
            })),
        )
        .await
        .map_err(|e| format!("取得影片列表失敗: {e}"))?;

        if let Some(items) = item_list_resp.data {
            if let Some(list) = items.get("list").and_then(|l| l.as_array()) {
                for item in list {
                    let pi = process_episode_media(item);
                    log::debug!("[play_movie] + 影片: {}", pi.title);
                    playlist.push(pi);
                }
            }
        }

        if playlist.is_empty() {
            playlist.push(process_single_media(&play_info));
        }
    } else {
        log::info!("[play_movie] 單集模式");
        playlist.push(process_single_media(&play_info));
    }

    if playlist.is_empty() {
        return Err("播放列表為空".to_string());
    }

    // ── Step 2.5: 註冊 proxy session 並填充播放 URL ──────────────
    // 對應上游：收集全部 itemGuids → POST /api/v1/session → playLink 改為 ?session=
    let session = {
        let secret = proxy_daemon.secret().to_string();
        let item_guids: Vec<String> = playlist.iter().map(|i| i.item_guid.clone()).collect();
        let session =
            register_proxy_session(&proxy_base, &secret, token, &cfg, &item_guids).await?;
        for item in &mut playlist {
            item.play_link =
                create_proxy_playback_url(&proxy_base, &session, &item.item_guid, 0);
        }
        session
    };

    // ── Step 3: 處理 sourceIndex ──────────────────────────
    // 找到當前播放項目在列表中的位置
    let current_index = playlist
        .iter()
        .position(|item| item.item_guid == item_guid)
        .unwrap_or(0);

    if payload.source_index > 0 {
        log::info!(
            "[play_movie] 使用指定播放源索引: {}",
            payload.source_index
        );
        // 用指定 sourceIndex 重建當前項目的 proxy URL（沿用同一個 session）
        playlist[current_index].play_link = create_proxy_playback_url(
            &proxy_base,
            &session,
            &playlist[current_index].item_guid,
            payload.source_index,
        );
    }

    log::info!(
        "[play_movie] 播放列表 {} 項，從第 {} 項開始",
        playlist.len(),
        current_index
    );

    // ── Step 4: 啟動/複用 MPV ─────────────────────────────
    {
        let mut player = mpv_state.player.write().await;
        let state = player.get_playback_state().await;
        let needs_launch = !state.is_playing;

        if needs_launch {
            let mpv_path = find_mpv_player_path(&cfg)?;
            let config = MpvConfig {
                player_path: mpv_path,
                extra_args: vec![
                    "--force-window=immediate".to_string(),
                    "--network-timeout=180".to_string(),
                ],
                debug: true,
                ..Default::default()
            };
            *player = crate::mpv::player::MpvPlayer::new(config);
            player.launch(app.clone()).await?;
            log::info!("[play_movie] MPV 啟動成功");
        } else {
            log::info!("[play_movie] MPV 已在運行，載入新播放列表");
        }
    }

    // ── Step 5: 載入播放列表 ──────────────────────────────
    {
        let mut player = mpv_state.player.write().await;
        player.load_playlist(playlist, current_index).await?;
    }

    log::info!("[play_movie] 播放列表已載入，開始播放");
    Ok(())
}

/// `get_play_button_config` command — 回傳播放按鈕顯示設定
#[tauri::command]
pub async fn get_play_button_config(app: tauri::AppHandle) -> Result<PlayButtonConfig, String> {
    let hide = crate::config::get_hide_original_play_button(app)?;
    Ok(PlayButtonConfig {
        hide_original_play_button: hide,
    })
}

/// `log_message` command — 記錄前端日誌
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
