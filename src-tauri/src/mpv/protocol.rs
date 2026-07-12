//! MPV JSON-RPC IPC 協議類型定義
//!
//! MPV 的 IPC 協議基於 JSON，每條訊息以 `\n` 結尾：
//! - **Request**: `{"command": [...], "request_id": N}\n`
//! - **Response**: `{"error": "success", "data": ..., "request_id": N}\n`
//! - **Event**:    `{"event": "property-change", "id": N, "name": "...", "data": ...}\n`

use serde::{Deserialize, Serialize};

// ─── 請求 ───────────────────────────────────────────────────────────

/// 發送給 MPV 的 JSON-RPC 請求
#[derive(Debug, Clone, Serialize)]
pub struct IpcRequest {
    /// 命令列，例如 `["loadfile", "http://...", "replace"]`
    pub command: Vec<serde_json::Value>,
    /// 唯一請求 ID，用於匹配回應
    pub request_id: u64,
}

impl IpcRequest {
    /// 建立命令請求（ command 名稱 + 引數列）
    pub fn command(name: &str, args: Vec<serde_json::Value>, request_id: u64) -> Self {
        let mut command = vec![serde_json::Value::String(name.to_string())];
        command.extend(args);
        Self { command, request_id }
    }

    /// `set_property <name> <value>`
    pub fn set_property(name: &str, value: serde_json::Value, request_id: u64) -> Self {
        Self::command(
            "set_property",
            vec![
                serde_json::Value::String(name.to_string()),
                value,
            ],
            request_id,
        )
    }

    /// `get_property <name>`
    pub fn get_property(name: &str, request_id: u64) -> Self {
        Self::command(
            "get_property",
            vec![serde_json::Value::String(name.to_string())],
            request_id,
        )
    }

    /// `observe_property <id> <name>`
    #[allow(dead_code)]
    pub fn observe_property(id: u64, name: &str, request_id: u64) -> Self {
        Self::command(
            "observe_property",
            vec![
                serde_json::json!(id),
                serde_json::Value::String(name.to_string()),
            ],
            request_id,
        )
    }

    /// `loadfile <url> <mode>`
    pub fn loadfile(url: &str, mode: &str, request_id: u64) -> Self {
        Self::command(
            "loadfile",
            vec![
                serde_json::Value::String(url.to_string()),
                serde_json::Value::String(mode.to_string()),
            ],
            request_id,
        )
    }

    /// `loadlist <path> <mode>`
    pub fn loadlist(path: &str, mode: &str, request_id: u64) -> Self {
        Self::command(
            "loadlist",
            vec![
                serde_json::Value::String(path.to_string()),
                serde_json::Value::String(mode.to_string()),
            ],
            request_id,
        )
    }

    /// `seek <seconds> absolute`
    pub fn seek(seconds: f64, request_id: u64) -> Self {
        Self::command(
            "seek",
            vec![
                serde_json::json!(seconds),
                serde_json::Value::String("absolute".to_string()),
            ],
            request_id,
        )
    }

    /// `stop`
    pub fn stop(request_id: u64) -> Self {
        Self::command("stop", vec![], request_id)
    }

    /// `quit`
    #[allow(dead_code)]
    pub fn quit(request_id: u64) -> Self {
        Self::command("quit", vec![], request_id)
    }
}

// ─── 回應 ───────────────────────────────────────────────────────────

/// MPV 回應的原始 JSON 結構
#[derive(Debug, Clone, Deserialize)]
pub struct IpcResponse {
    /// 錯誤狀態："success" 表示成功
    pub error: String,
    /// 回傳資料（僅部分命令有）
    pub data: Option<serde_json::Value>,
    /// 對應的請求 ID
    pub request_id: Option<u64>,
}

impl IpcResponse {
    pub fn is_success(&self) -> bool {
        self.error == "success"
    }
}

// ─── 事件 ───────────────────────────────────────────────────────────

/// MPV 主動推送的事件
#[derive(Debug, Clone, Deserialize)]
pub struct IpcEvent {
    /// 事件類型，例如 "property-change", "end-file", "start-file", "log-message"
    pub event: String,
    /// property-change 事件的監聽 ID
    #[allow(dead_code)]
    pub id: Option<u64>,
    /// 變化的屬性名稱
    pub name: Option<String>,
    /// 新的屬性值
    pub data: Option<serde_json::Value>,
    /// end-file 事件的原因
    pub reason: Option<String>,
}

// ─── 訊息判別 ──────────────────────────────────────────────────────

/// 從 socket 收到的訊息（可能是回應或事件）
#[derive(Debug, Clone)]
pub enum IpcMessage {
    Response(IpcResponse),
    Event(IpcEvent),
}

impl IpcMessage {
    /// 嘗試從原始 JSON 字串解析
    pub fn parse(json_str: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(json_str).ok()?;

        // 有 request_id → 回應
        if value.get("request_id").and_then(|v| v.as_u64()).is_some() {
            let resp: IpcResponse = serde_json::from_value(value).ok()?;
            return Some(Self::Response(resp));
        }

        // 有 event 欄位 → 事件
        if value.get("event").is_some() {
            let evt: IpcEvent = serde_json::from_value(value).ok()?;
            return Some(Self::Event(evt));
        }

        None
    }
}

// ─── 播放列表 ──────────────────────────────────────────────────────

/// 前端傳入的播放列表項目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayListItem {
    pub item_guid: String,
    pub title: String,
    #[serde(default)]
    pub tv_title: String,
    #[serde(default)]
    pub season_number: u32,
    #[serde(default)]
    pub episode_number: u32,
    /// 已播放秒數（從上次中斷點恢復）
    #[serde(default)]
    pub ts: f64,
    /// 總時長（秒）
    #[serde(default)]
    pub duration: f64,
    /// 播放 URL
    pub play_link: String,
}

/// 當前播放狀態（回傳給前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub is_playing: bool,
    pub item_guid: String,
    pub ts: f64,
    pub duration: f64,
    pub percentage: f64,
    pub volume: f64,
    pub is_muted: bool,
    pub pause: bool,
}
