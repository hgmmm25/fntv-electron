//! MPV 異步 IPC 客戶端
//!
//! 透過 Unix Socket（macOS/Linux）或 Named Pipe（Windows）與 MPV 進行
//! JSON-RPC 通訊。使用 tokio 非同步 I/O。
//!
//! 協議格式（每條訊息以 `\n` 結尾）：
//! - 發送：`{"command": [...], "request_id": N}\n`
//! - 接收回應：`{"error": "success", "data": ..., "request_id": N}\n`
//! - 接收事件：`{"event": "property-change", "name": "...", "data": ...}\n`

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot, Mutex};

use super::protocol::*;

/// 待處理的請求映射類型
type PendingMap = Arc<
    Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>,
>;

// ─── IPC 客戶端 ────────────────────────────────────────────────────

/// 異步 IPC 客戶端，管理與 MPV 的 JSON-RPC 通訊
pub struct MpvIpcClient {
    /// 待處理的請求：request_id → oneshot sender
    pending: PendingMap,
    /// 事件廣播器（property-change 等）
    event_tx: broadcast::Sender<IpcEvent>,
    /// 請求 ID 計數器
    next_id: AtomicU64,
    /// 寫入端（供 send 使用）
    writer: Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>,
}

impl MpvIpcClient {
    /// 連接到 MPV IPC socket 並建立客戶端
    pub async fn connect(socket_path: &str) -> Result<Self, String> {
        let (reader, writer) = connect_platform(socket_path).await?;

        let (event_tx, _) = broadcast::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let client = Self {
            pending: pending.clone(),
            event_tx: event_tx.clone(),
            next_id: AtomicU64::new(1),
            writer: Arc::new(Mutex::new(writer)),
        };

        // 啟動背景讀取任務
        let pending_for_reader = pending.clone();
        tokio::spawn(async move {
            reader_loop(reader, pending_for_reader, event_tx).await;
        });

        Ok(client)
    }

    /// 發送命令並等待回應（帶 request_id 匹配）
    pub async fn send_command(
        &self,
        request: IpcRequest,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = oneshot::channel();
        let id = request.request_id;

        // 註冊 pending
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // 序列化並發送
        let mut msg = serde_json::to_string(&request)
            .map_err(|e| format!("序列化請求失敗: {e}"))?;
        msg.push('\n');

        {
            let mut writer = self.writer.lock().await;
            writer
                .write_all(msg.as_bytes())
                .await
                .map_err(|e| format!("寫入 socket 失敗: {e}"))?;
        }

        // 等待回應（帶超時）
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err("IPC 通道已關閉".to_string())
            }
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(format!("MPV 回應超時 (request_id={id})"))
            }
        }
    }

    /// 便利方法：發送命令列
    pub async fn command(
        &self,
        name: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send_command(IpcRequest::command(name, args, id))
            .await
    }

    /// 生成下一個唯一請求 ID（供外部呼叫以避免跨層級 ID 衝突）
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 訂閱事件（property-change 等）
    pub fn subscribe_events(&self) -> broadcast::Receiver<IpcEvent> {
        self.event_tx.subscribe()
    }

    /// 關閉客戶端
    pub async fn shutdown(&self) {
        let _ = self.command("quit", vec![]).await;
        let mut pending = self.pending.lock().await;
        pending.clear();
        let mut writer = self.writer.lock().await;
        let _ = writer.shutdown().await;
    }
}

// ─── 跨平台連接 ────────────────────────────────────────────────────

/// 連接到平台對應的 IPC 端點，回傳 (reader, writer)
async fn connect_platform(
    path: &str,
) -> Result<
    (
        Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    ),
    String,
> {
    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        let stream = UnixStream::connect(path)
            .await
            .map_err(|e| format!("無法連接 MPV socket ({path}): {e}"))?;
        let (rd, wr) = stream.into_split();
        Ok((Box::new(rd), Box::new(wr)))
    }

    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let client = ClientOptions::new()
            .open(path)
            .map_err(|e| format!("無法連接 MPV named pipe ({path}): {e}"))?;
        // NamedPipeClient 本身同時實作了 AsyncRead + AsyncWrite
        // 我們用 tokio::io::split 來拆分
        let (rd, wr) = tokio::io::split(client);
        Ok((
            Box::new(rd),
            Box::new(wr),
        ))
    }
}

// ─── 背景讀取任務 ───────────────────────────────────────────────────

/// 持續從 socket 讀取資料，解析 JSON 訊息，路由回應/事件
async fn reader_loop(
    reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    pending: PendingMap,
    event_tx: broadcast::Sender<IpcEvent>,
) {
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match IpcMessage::parse(trimmed) {
            Some(IpcMessage::Response(resp)) => {
                if let Some(id) = resp.request_id {
                    let mut pending = pending.lock().await;
                    if let Some(tx) = pending.remove(&id) {
                        let result = if resp.is_success() {
                            Ok(resp.data.unwrap_or(serde_json::Value::Null))
                        } else {
                            Err(resp.error)
                        };
                        let _ = tx.send(result);
                    }
                }
            }
            Some(IpcMessage::Event(evt)) => {
                let _ = event_tx.send(evt);
            }
            None => {
                log::warn!("無法解析 MPV IPC 訊息: {trimmed}");
            }
        }
    }

    log::info!("MPV IPC 讀取迴圈結束");
    // 清理所有 pending
    let mut pending = pending.lock().await;
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err("IPC 通道已關閉".to_string()));
    }
}
