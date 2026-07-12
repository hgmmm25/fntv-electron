//! Tauri Commands — MPV 播放控制
//!
//! 前端透過 `invoke('command_name', { args })` 呼叫這些函數。

use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

use super::player::{MpvConfig, MpvPlayer};
use super::protocol::{PlayListItem, PlaybackState};

/// 播放器狀態容器（存放於 Tauri managed state）
pub struct MpvPlayerState {
    pub player: Arc<RwLock<MpvPlayer>>,
}

// ─── Commands ───────────────────────────────────────────────────────

/// 啟動 MPV 播放器
#[tauri::command]
pub async fn mpv_launch(
    state: State<'_, MpvPlayerState>,
    player_path: Option<String>,
    extra_args: Option<Vec<String>>,
    debug: Option<bool>,
) -> Result<(), String> {
    let mut player = state.player.write().await;
    let config = MpvConfig {
        player_path: player_path.unwrap_or_default(),
        extra_args: extra_args.unwrap_or_default(),
        debug: debug.unwrap_or(false),
        ..Default::default()
    };
    *player = MpvPlayer::new(config);
    player.launch().await
}

/// 播放單個 URL
#[tauri::command]
pub async fn mpv_play(
    state: State<'_, MpvPlayerState>,
    url: String,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.play(&url).await
}

/// 載入播放列表並播放
#[tauri::command]
pub async fn mpv_load_playlist(
    state: State<'_, MpvPlayerState>,
    items: Vec<PlayListItem>,
    pos: usize,
) -> Result<(), String> {
    let mut player = state.player.write().await;
    player.load_playlist(items, pos).await
}

/// 暫停播放
#[tauri::command]
pub async fn mpv_pause(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.pause().await
}

/// 繼續播放
#[tauri::command]
pub async fn mpv_resume(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.resume().await
}

/// 停止播放
#[tauri::command]
pub async fn mpv_stop(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.stop().await
}

/// 跳轉到指定位置（秒）
#[tauri::command]
pub async fn mpv_seek(
    state: State<'_, MpvPlayerState>,
    position: f64,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.seek(position).await
}

/// 設定音量 (0-100)
#[tauri::command]
pub async fn mpv_set_volume(
    state: State<'_, MpvPlayerState>,
    volume: f64,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_volume(volume).await
}

/// 設定靜音
#[tauri::command]
pub async fn mpv_set_mute(
    state: State<'_, MpvPlayerState>,
    mute: bool,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_mute(mute).await
}

/// 取得當前播放狀態
#[tauri::command]
pub async fn mpv_get_state(
    state: State<'_, MpvPlayerState>,
) -> Result<PlaybackState, String> {
    let player = state.player.read().await;
    Ok(player.get_playback_state().await)
}

/// 設定屬性值
#[tauri::command]
pub async fn mpv_set_property(
    state: State<'_, MpvPlayerState>,
    name: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_property(&name, value).await
}

/// 取得屬性值
#[tauri::command]
pub async fn mpv_get_property(
    state: State<'_, MpvPlayerState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let player = state.player.read().await;
    player.get_property(&name).await
}

/// 關閉播放器
#[tauri::command]
pub async fn mpv_shutdown(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let mut player = state.player.write().await;
    player.shutdown().await;
    Ok(())
}

/// 播放下一個播放列表項目
#[tauri::command]
pub async fn mpv_playlist_next(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_property("playlist-next", serde_json::json!("weak")).await
}

/// 播放上一個播放列表項目
#[tauri::command]
pub async fn mpv_playlist_prev(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_property("playlist-prev", serde_json::json!("weak")).await
}

/// 跳轉到播放列表指定位置
#[tauri::command]
pub async fn mpv_playlist_pos(
    state: State<'_, MpvPlayerState>,
    index: u32,
) -> Result<(), String> {
    let player = state.player.read().await;
    player
        .set_property("playlist-pos", serde_json::json!(index))
        .await
}

/// 切換全螢幕
#[tauri::command]
pub async fn mpv_toggle_fullscreen(
    state: State<'_, MpvPlayerState>,
) -> Result<(), String> {
    let player = state.player.read().await;
    player.set_property("fullscreen", serde_json::json!("toggle")).await
}
