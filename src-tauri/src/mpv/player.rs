//! MPV 高階播放器 API
//!
//! 封裝 MPV 進程的啟動、IPC 連接、播放控制、屬性監聽與狀態管理。
//! 對應 TypeScript 版 `src/modules/players/impl/mpv.ts` 的功能。
//!
//! ## 架構：獨立進程模式（Independent Window）
//!
//! MPV 作為**完全獨立的外部進程**運行，透過 IPC（JSON-RPC over Unix socket /
//! Windows named pipe）與 Tauri 應用通訊。這意味著：
//!
//! - MPV 擁有自己的原生視窗（由 mpv 自行建立和管理）
//! - Tauri 主視窗與 MPV 視窗是兩個獨立的 OS 視窗
//! - 兩者之間**沒有** HWND 綁定或視窗嵌入
//!
//! ### 為什麼選擇獨立進程而非嵌入？
//!
//! 1. **穩定性**：MPV 嵌入 HWND 可能導致渲染衝突（尤其是 GPU 加速時）
//! 2. **跨平台**：HWND 嵌入僅 Windows 可靠（macOS 的 NSView 嵌入複雜度高）
//! 3. **靈活性**：獨立視窗可自由移動、多顯示器支持、系統 Alt+Tab 正常
//! 4. **對齊 Electron 版**：Electron 版也是 spawn 獨立 mpv 進程
//!
//! ### 主視窗與 MPV 視窗的聯動（目前未實作）
//!
//! 若要實現視窗聯動（同步縮放、移動、置頂），需要：
//!
//! - **Windows**：使用 `--wid=<HWND>` 將 MPV 嵌入 Tauri webview 的原生視窗，
//!   或透過 `FindWindow` 取得 MPV 視窗句柄後用 `SetWindowPos` 同步位置。
//! - **macOS**：需要透過 NSWindow API 取得 MPV 視窗（`mpv_get_property("window-handle")`），
//!   然後用 Core Graphics API 同步位置。
//! - **Linux**：透過 X11 `XFindWindow` 或 Wayland 協議取得視窗句柄。
//!
//! 由於當前架構為「MPV 全屏獨立播放」（對應 Electron 版的行為），
//! 視窗聯動的需求較低。如需此功能，建議在 `mpv::commands` 模組新增
//! `mpv_get_window_handle` 和 `mpv_sync_window_position` 兩個 command。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::process::{Child, Command};

use super::client::MpvIpcClient;
use super::protocol::*;

// ─── 配置 ───────────────────────────────────────────────────────────

/// 播放器啟動配置
#[derive(Debug, Clone, Default)]
pub struct MpvConfig {
    /// MPV 可執行檔路徑（空字串時使用系統 PATH 中的 mpv）
    pub player_path: String,
    /// HTTP 請求標頭（key → value）
    pub headers: std::collections::HashMap<String, String>,
    /// 額外的 MPV 命令列參數
    pub extra_args: Vec<String>,
    /// 是否輸出 debug 日誌
    pub debug: bool,
}

// ─── 播放狀態 ──────────────────────────────────────────────────────

/// 共享播放狀態（由事件監聽器更新，由 commands 讀取）
#[derive(Debug, Clone, Default)]
pub struct PlayerState {
    pub is_playing: bool,
    pub item_guid: String,
    pub ts: f64,
    pub duration: f64,
    pub percentage: f64,
    pub volume: f64,
    pub is_muted: bool,
    pub pause: bool,
    pub playlist_count: u32,
    pub playlist_pos: u32,
    pub current_path: String,
}

// ─── 播放器主體 ────────────────────────────────────────────────────

/// MPV 播放器
pub struct MpvPlayer {
    /// IPC 客戶端
    client: Option<MpvIpcClient>,
    /// MPV 進程句柄
    child: Option<Child>,
    /// 當前配置
    config: MpvConfig,
    /// 共享播放狀態
    state: Arc<RwLock<PlayerState>>,
    /// 事件廣播（供前端訂閱）
    event_tx: broadcast::Sender<PlayerEvent>,
    /// 當前播放列表項目
    playlist_items: Vec<PlayListItem>,
    /// 播放列表臨時檔路徑
    playlist_path: Option<PathBuf>,
    /// MPV IPC socket 路徑
    socket_path: String,
}

/// 播放器事件（供前端訂閱）
#[derive(Debug, Clone, serde::Serialize)]
pub enum PlayerEvent {
    Progress {
        item_guid: String,
        ts: f64,
        duration: f64,
        percentage: f64,
    },
    #[allow(dead_code)]
    Error {
        message: String,
    },
    Exit {
        code: i32,
    },
}

impl MpvPlayer {
    /// 建立新的播放器實例
    pub fn new(config: MpvConfig) -> Self {
        let (event_tx, _) = broadcast::channel(32);
        let socket_path = platform_socket_path();

        Self {
            client: None,
            child: None,
            config,
            state: Arc::new(RwLock::new(PlayerState::default())),
            event_tx,
            playlist_items: Vec::new(),
            playlist_path: None,
            socket_path,
        }
    }

    /// 啟動 MPV 並連接 IPC
    pub async fn launch(&mut self) -> Result<(), String> {
        // 構建命令列參數
        let mut args = vec![
            "--idle".to_string(),
            "--msg-level=all=no,ipc=v".to_string(),
            format!("--input-ipc-server={}", self.socket_path),
        ];

        // HTTP 標頭
        if !self.config.headers.is_empty() {
            let header_str: Vec<String> = self
                .config
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            args.push(format!("--http-header-fields={}", header_str.join(",")));
        }

        // 額外參數
        args.extend(self.config.extra_args.clone());

        // 決定 MPV 可執行檔路徑
        let mpv_bin = if self.config.player_path.is_empty() {
            "mpv".to_string()
        } else {
            self.config.player_path.clone()
        };

        log::info!("啟動 MPV: {mpv_bin} {}", args.join(" "));

        // 啟動進程
        let child = Command::new(&mpv_bin)
            .args(&args)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("啟動 MPV 失敗: {e}"))?;

        self.child = Some(child);

        // 等待 socket 就緒（最多 5 秒）
        self.wait_for_socket(5000).await?;

        // 連接 IPC
        let client = MpvIpcClient::connect(&self.socket_path).await?;
        self.client = Some(client);

        // 啟動事件監聽
        self.start_event_listener().await;

        log::info!("MPV 啟動成功，IPC 已連接");
        Ok(())
    }

    /// 等待 MPV socket 可連接
    async fn wait_for_socket(&self, timeout_ms: u64) -> Result<(), String> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "等待 MPV socket 超時 ({timeout_ms}ms): {}",
                    self.socket_path
                ));
            }

            match try_connect_socket(&self.socket_path).await {
                Ok(true) => return Ok(()),
                Ok(false) => { /* 有 socket 但不是 MPV */ }
                Err(_) => { /* 無 socket */ }
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// 啟動事件監聽（背景任務）
    async fn start_event_listener(&self) {
        let client = match self.client.as_ref() {
            Some(c) => c,
            None => return,
        };

        let mut rx = client.subscribe_events();
        let state = self.state.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            while let Ok(evt) = rx.recv().await {
                match evt.event.as_str() {
                    "property-change" => {
                        if let (Some(name), Some(data)) = (evt.name.as_deref(), &evt.data) {
                            let mut state = state.write().await;
                            update_property(&mut state, name, data);

                            // 進度事件
                            if name == "time-pos" {
                                if let Some(ts) = data.as_f64() {
                                    let _ = event_tx.send(PlayerEvent::Progress {
                                        item_guid: state.item_guid.clone(),
                                        ts,
                                        duration: state.duration,
                                        percentage: state.percentage,
                                    });
                                }
                            }
                        }
                    }
                    "end-file" => {
                        let reason = evt.reason.as_deref().unwrap_or("unknown");
                        log::info!("MPV end-file: {reason}");
                        if reason == "quit" {
                            let _ = event_tx.send(PlayerEvent::Exit { code: 0 });
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    // ─── 播放控制 ───────────────────────────────────────────────────

    /// 播放單個 URL
    pub async fn play(&self, url: &str) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::loadfile(url, "replace", next_id()))
            .await?;
        let mut state = self.state.write().await;
        state.is_playing = true;
        state.current_path = url.to_string();
        Ok(())
    }

    /// 載入播放列表並跳轉到指定位置
    pub async fn load_playlist(
        &mut self,
        items: Vec<PlayListItem>,
        pos: usize,
    ) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;

        // 生成 M3U8 檔案
        let content = generate_m3u8(&items);
        let path = std::env::temp_dir().join(format!("fntv_playlist_{}.m3u8", next_id()));
        tokio::fs::write(&path, &content)
            .await
            .map_err(|e| format!("寫入播放列表失敗: {e}"))?;

        // 載入播放列表
        client
            .send_command(IpcRequest::loadlist(
                path.to_str().unwrap_or(""),
                "replace",
                next_id(),
            ))
            .await?;

        // 跳轉到指定位置
        if pos > 0 {
            let _ = client
                .command("playlist-pos", vec![serde_json::json!(pos)])
                .await;
        }

        // 設定各項目的屬性
        if pos < items.len() {
            let item = &items[pos];
            let title = format_title(item);
            let _ = client
                .send_command(IpcRequest::set_property(
                    "force-media-title",
                    serde_json::Value::String(title),
                    next_id(),
                ))
                .await;

            // 嘗試恢復播放進度
            if item.ts > 0.0 && item.ts <= 0.98 * item.duration {
                self.seek_with_retry(item.ts).await;
            }

            // 更新狀態
            let mut state = self.state.write().await;
            state.item_guid = item.item_guid.clone();
            state.ts = item.ts;
            state.duration = item.duration;
        }

        self.playlist_items = items;
        self.playlist_path = Some(path);
        Ok(())
    }

    /// 暫停播放
    pub async fn pause(&self) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::set_property(
                "pause",
                serde_json::Value::Bool(true),
                next_id(),
            ))
            .await?;
        self.state.write().await.pause = true;
        Ok(())
    }

    /// 繼續播放
    pub async fn resume(&self) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::set_property(
                "pause",
                serde_json::Value::Bool(false),
                next_id(),
            ))
            .await?;
        self.state.write().await.pause = false;
        Ok(())
    }

    /// 停止播放
    pub async fn stop(&self) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::stop(next_id()))
            .await?;
        self.state.write().await.is_playing = false;
        Ok(())
    }

    /// 跳轉到指定位置（秒）
    pub async fn seek(&self, seconds: f64) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::seek(seconds, next_id()))
            .await?;
        Ok(())
    }

    /// 設定音量 (0-100)
    pub async fn set_volume(&self, volume: f64) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::set_property(
                "volume",
                serde_json::json!(volume),
                next_id(),
            ))
            .await?;
        self.state.write().await.volume = volume;
        Ok(())
    }

    /// 設定靜音
    pub async fn set_mute(&self, mute: bool) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::set_property(
                "mute",
                serde_json::Value::Bool(mute),
                next_id(),
            ))
            .await?;
        self.state.write().await.is_muted = mute;
        Ok(())
    }

    /// 取得屬性值
    pub async fn get_property(&self, name: &str) -> Result<serde_json::Value, String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::get_property(name, next_id()))
            .await
    }

    /// 設定屬性值
    pub async fn set_property(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("MPV 未啟動")?;
        client
            .send_command(IpcRequest::set_property(name, value, next_id()))
            .await?;
        Ok(())
    }

    /// 訂閱播放器事件
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.event_tx.subscribe()
    }

    /// 取得當前播放狀態（供 Tauri command 回傳）
    pub async fn get_playback_state(&self) -> PlaybackState {
        let st = self.state.read().await;
        PlaybackState {
            is_playing: st.is_playing,
            item_guid: st.item_guid.clone(),
            ts: st.ts,
            duration: st.duration,
            percentage: st.percentage,
            volume: st.volume,
            is_muted: st.is_muted,
            pause: st.pause,
        }
    }

    // ─── 關閉 ───────────────────────────────────────────────────────

    /// 關閉播放器並清理資源
    pub async fn shutdown(&mut self) {
        // 關閉 IPC 客戶端（會發送 quit）
        if let Some(client) = self.client.take() {
            client.shutdown().await;
        }

        // 等待進程退出
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }

        // 清理播放列表臨時檔
        if let Some(path) = self.playlist_path.take() {
            let _ = tokio::fs::remove_file(&path).await;
        }

        log::info!("MPV 播放器已關閉");
    }

    /// 帶重試的跳轉（對應 TypeScript 版 seekWithRetry）
    async fn seek_with_retry(&self, position: f64) {
        let client = match self.client.as_ref() {
            Some(c) => c,
            None => return,
        };

        for attempt in 0..10 {
            match client
                .send_command(IpcRequest::seek(position, next_id()))
                .await
            {
                Ok(_) => {
                    if self.config.debug {
                        log::info!("跳轉成功: {position}s (嘗試 {attempt})");
                    }
                    return;
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }

        if self.config.debug {
            log::warn!("跳轉失敗，已達最大重試次數 (位置: {position}s)");
        }
    }
}

// ─── 輔助函數 ──────────────────────────────────────────────────────

/// 全域請求 ID 計數器（僅用於播放器層級的便利呼叫）
fn next_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(10000); // 從 10000 開始，避免與 client 內部 ID 衝突
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// 平台對應的 socket 路徑
fn platform_socket_path() -> String {
    let pid = std::process::id();
    #[cfg(unix)]
    {
        format!("/tmp/fntv-mpv-{pid}.sock")
    }
    #[cfg(windows)]
    {
        format!("\\\\.\\pipe\\fntv-mpv-{pid}")
    }
}

/// 嘗試連接 socket（用於等待 MPV 就緒）
async fn try_connect_socket(path: &str) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        match UnixStream::connect(path).await {
            Ok(mut stream) => {
                use tokio::io::AsyncWriteExt;
                // 發送一個簡單命令測試
                let msg = r#"{"command": ["get_property", "mpv-version"], "request_id": 1}"#;
                let _ = stream.write_all(format!("{msg}\n").as_bytes()).await;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        match ClientOptions::new().open(path) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

/// 更新屬性到狀態
fn update_property(state: &mut PlayerState, name: &str, value: &serde_json::Value) {
    match name {
        "pause" => {
            state.pause = value.as_bool().unwrap_or(false);
        }
        "volume" => {
            state.volume = value.as_f64().unwrap_or(0.0);
        }
        "mute" => {
            state.is_muted = value.as_bool().unwrap_or(false);
        }
        "duration" => {
            state.duration = value.as_f64().unwrap_or(0.0);
        }
        "playlist-pos" => {
            state.playlist_pos = value.as_u64().unwrap_or(0) as u32;
        }
        "playlist-count" => {
            state.playlist_count = value.as_u64().unwrap_or(0) as u32;
        }
        "path" => {
            if let Some(s) = value.as_str() {
                state.current_path = s.to_string();
                // 從 URL 路徑末段提取 item_guid
                if let Some(guid) = s.rsplit('/').next() {
                    if !guid.is_empty() {
                        state.item_guid = guid.to_string();
                    }
                }
            }
        }
        "time-pos" => {
            if let Some(ts) = value.as_f64() {
                state.ts = ts;
                if state.duration > 0.0 {
                    state.percentage = (ts / state.duration * 100.0).floor();
                }
            }
        }
        _ => {}
    }
}

/// 生成 M3U8 播放列表內容
fn generate_m3u8(items: &[PlayListItem]) -> String {
    let mut content = "#EXTM3U\n".to_string();
    for item in items {
        let title = format_title(item);
        let duration = if item.duration > 0.0 {
            item.duration as i64
        } else {
            -1
        };
        content.push_str(&format!("#EXTINF:{duration},{title}\n"));
        content.push_str(&format!("{}\n", item.play_link));
    }
    content
}

/// 格式化標題（對應 TypeScript 版 getTitle）
fn format_title(item: &PlayListItem) -> String {
    if !item.tv_title.is_empty() {
        format!(
            "{} - S{:02}E{:02}: {}",
            item.tv_title, item.season_number, item.episode_number, item.title
        )
    } else {
        item.title.clone()
    }
}
