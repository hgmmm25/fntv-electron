//! FN API 通用客戶端模組
//!
//! 從 `auth.rs` 提取並增強的 FN API 請求工具，
//! 支援 GET/POST 兩種方法，供登入、播放、進度上報等模組共用。
//!
//! 對應 `src/modules/fn_api/request.ts` 的完整請求邏輯。

use std::time::{SystemTime, UNIX_EPOCH};

// ─── FN API 常數 ──────────────────────────────────────────

/// FN API 簽名金鑰
const API_KEY: &str = "NDzZTVxnRKP8Z0jXg1VAMonaG8akvh";
/// FN API 簽名密鑰
const API_SECRET: &str = "16CCEB3D-AB42-077D-36A1-F355324E4237";

// ─── 請求方法 ─────────────────────────────────────────────

/// HTTP 請求方法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

// ─── 簽名工具 ─────────────────────────────────────────────

/// MD5 雜湊（hex 輸出）
pub fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// 產生隨機數字字串（與 Electron 的 generateRandomDigits 相容）
pub fn random_digits(start: u64, end: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    ((nanos % (end - start)) + start).to_string()
}

/// 產生 Authx header（對應 Electron 的 genFnAuthx）
pub fn gen_fn_authx(url: &str, data: Option<&serde_json::Value>) -> String {
    let nonce = random_digits(100_000, 1_000_000);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();

    let data_json = data
        .map(|d| serde_json::to_string(d).unwrap_or_default())
        .unwrap_or_default();
    let data_json_md5 = md5_hex(&data_json);

    let sign_str = format!(
        "{}_{}_{}_{}_{}_{}",
        API_KEY, url, nonce, timestamp, data_json_md5, API_SECRET
    );
    let sign = md5_hex(&sign_str);

    format!("nonce={}&timestamp={}&sign={}", nonce, timestamp, sign)
}

// ─── 回應結構 ─────────────────────────────────────────────

/// FN API 回應的外層結構（對應 FnApiResponseData）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FnApiResponse {
    pub code: i64,
    pub msg: String,
    pub data: Option<serde_json::Value>,
}

// ─── 通用請求 ─────────────────────────────────────────────

/// 通用 FN API 請求
///
/// 支援 GET 與 POST 兩種方法。
/// GET：data 為空、不注入 nonce、path 直接作為簽名 URL
/// POST：data 加入 nonce、對 data 做 JSON 簽名
///
/// 對應 `src/modules/fn_api/request.ts` 的 `request()` 函數。
pub async fn fn_request(
    base_url: &str,
    path: &str,
    method: HttpMethod,
    token: &str,
    data: Option<serde_json::Value>,
) -> Result<FnApiResponse, String> {
    let full_url = format!("{}{}", base_url, path);

    let mut request_data = data.clone();

    // POST 請求自動加入 nonce（GET 不加）
    if method == HttpMethod::Post {
        if let Some(ref mut d) = request_data {
            d.as_object_mut()
                .unwrap()
                .insert(
                    "nonce".into(),
                    serde_json::Value::String(random_digits(100_000, 1_000_000)),
                );
        }
    }

    let authx = gen_fn_authx(path, request_data.as_ref());

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("建立 HTTP client 失敗: {e}"))?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|_| "Invalid Content-Type header".to_string())?,
    );
    headers.insert(
        "Authorization",
        token
            .parse()
            .map_err(|_| "Invalid Authorization header".to_string())?,
    );
    headers.insert(
        "Cookie",
        "mode=relay"
            .parse()
            .map_err(|_| "Invalid Cookie header".to_string())?,
    );
    headers.insert(
        "Authx",
        authx
            .parse()
            .map_err(|_| "Invalid Authx header".to_string())?,
    );

    // 最多重試 5 次（處理簽名錯誤）
    let max_retries = 5;
    for attempt in 0..=max_retries {
        let resp = match method {
            HttpMethod::Get => client
                .get(&full_url)
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| format!("FN API 請求失敗: {e}"))?,
            HttpMethod::Post => {
                let mut req = client
                    .post(&full_url)
                    .headers(headers.clone());
                if let Some(ref body) = request_data {
                    req = req.json(body);
                }
                req.send()
                    .await
                    .map_err(|e| format!("FN API 請求失敗: {e}"))?
            }
        };

        // 處理重定向
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if !location.is_empty() {
                log::warn!("FN API 檢測到重定向 -> {location}");
                if location.starts_with("http") {
                    if let Some(slash_pos) = location[8..].find('/') {
                        let new_base = &location[..8 + slash_pos];
                        let new_path = &location[8 + slash_pos..];
                        return Box::pin(fn_request(
                            new_base,
                            new_path,
                            method,
                            token,
                            request_data.clone(),
                        ))
                        .await;
                    } else {
                        return Box::pin(fn_request(
                            &location,
                            path,
                            method,
                            token,
                            request_data.clone(),
                        ))
                        .await;
                    }
                }
            }
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let status = resp.status();

        if !status.is_success() && !status.is_redirection() {
            let body_text = resp.text().await.unwrap_or_default();
            log::error!("FN API HTTP {status}: {body_text}");
            return Err(format!("FN API 回應異常: HTTP {status}"));
        }

        if !content_type.contains("application/json") {
            return Ok(FnApiResponse {
                code: 0,
                msg: "ok".to_string(),
                data: None,
            });
        }

        let body: FnApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("FN API 回應解析失敗: {e} (HTTP {status})"))?;

        if body.code == 5000 && body.msg == "invalid sign" {
            if attempt >= max_retries {
                return Err("FN API 簽名錯誤，重試次數已用盡".to_string());
            }
            log::warn!(
                "FN API 簽名錯誤，重試中 attempt = {}",
                attempt + 1
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }

        if body.code != 0 {
            return Err(body.msg);
        }

        return Ok(body);
    }

    Err("FN API 請求失敗：超出重試次數".to_string())
}
