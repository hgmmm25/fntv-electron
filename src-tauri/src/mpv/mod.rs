//! MPV 播放器模組
//!
//! 完整移植自 `src/modules/players/impl/mpv.ts`，使用 Rust + tokio 重寫。
//!
//! - `protocol` — JSON-RPC IPC 協議類型
//! - `client`   — 異步 IPC 客戶端（Unix socket / Windows named pipe）
//! - `player`   — 高階播放器 API（啟動、控制、事件）
//! - `commands` — Tauri `#[tauri::command]` 函數

pub mod client;
pub mod commands;
pub mod player;
pub mod protocol;
