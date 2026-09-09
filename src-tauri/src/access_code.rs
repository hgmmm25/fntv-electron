//! 访问码验证与网关授权模块
//!
//! 对应 Electron 的 `src/main/common/accessCodeSession.ts`（验证流程）
//! 与 `src/modules/fn_api/accessGrant.ts`（网关 Cookie 内存授权表）。
//!
//! 服务器启用访问码时，登录前先向网关 `{base}/access_code_verify` 请求会话
//! Cookie（Base64 访问码 + x-access-source: web），成功后把网关 Cookie 以
//! `origin → cookie` 形式保存在**内存**授权表（ACCESS_GRANTS）中，供 FN API
//! 请求与本地 Go Proxy 播放会话注册拼接 Cookie。网关会话 Cookie 只保留在
//! 内存，不会写入 config.json、日志或任何持久化存储。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

// ─── 访问码验证结果 ──────────────────────────────────────

/// 验证成功后的网关会话信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessCodeSession {
    /// 解析后的 base URL（origin，例如 `http://192.168.1.10:5666`）
    pub base_url: String,
    /// 序列化后的网关会话 Cookie（不含 mode / trim-mc-token）
    pub cookie: String,
}

/// 访问码验证错误（reason 对齐上游 `AccessCodeVerificationError`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessCodeError {
    /// 访问码被网关拒绝（HTTP 401/403/429）
    Rejected(String),
    /// 网络/服务端错误（无法连接、无效地址、跨主机重定向等）
    Network(String),
}

impl std::fmt::Display for AccessCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessCodeError::Rejected(msg) => write!(f, "{msg}"),
            AccessCodeError::Network(msg) => write!(f, "{msg}"),
        }
    }
}

// ─── 访问码验证常量（与上游一致）────────────────────────

const MAX_REDIRECTS: u32 = 5;
const REJECTED_STATUS_CODES: [u16; 3] = [401, 403, 429];
const EXCLUDED_COOKIE_NAMES: [&str; 2] = ["mode", "trim-mc-token"];

// ─── 内存授权表（origin → gateway cookie）────────────────

static ACCESS_GRANTS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 设置/清除某个 origin 的网关授权 Cookie（对应 `setAccessGrant`）
pub fn set_access_grant(origin: &str, cookie: &str) -> Result<(), String> {
    let normalized = normalize_origin(origin).ok_or_else(|| "无效的访问码会话地址".to_string())?;
    let mut grants = ACCESS_GRANTS.lock().unwrap();
    if cookie.is_empty() {
        grants.remove(&normalized);
    } else {
        grants.insert(normalized, cookie.to_string());
    }
    Ok(())
}

/// 清空全部网关授权（对应 `clearAccessGrants`，每次新登录前调用）
pub fn clear_access_grants() {
    ACCESS_GRANTS.lock().unwrap().clear();
}

/// 取回某 origin 的网关授权 Cookie 原文（对应 `getAccessCookieHeader`）
pub fn get_access_cookie_header(origin: &str) -> String {
    match normalize_origin(origin) {
        Some(normalized) => ACCESS_GRANTS
            .lock()
            .unwrap()
            .get(&normalized)
            .cloned()
            .unwrap_or_default(),
        None => String::new(),
    }
}

// ─── origin 工具 ─────────────────────────────────────────

/// 规范化 origin（scheme://authority），非 http(s) 返回 None
pub fn normalize_origin(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let scheme_len = if trimmed.starts_with("https://") {
        8
    } else if trimmed.starts_with("http://") {
        7
    } else {
        return None;
    };
    let rest = &trimmed[scheme_len..];
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(rest.split(['/', '?', '#']).next().unwrap_or(""));
    if authority.is_empty() {
        return None;
    }
    let scheme = if scheme_len == 8 { "https" } else { "http" };
    Some(format!("{scheme}://{authority}"))
}

/// 取 origin 的主机名（不含端口，用于跨主机跳转校验）
fn origin_hostname(origin: &str) -> String {
    normalize_origin(origin)
        .map(|o| {
            let authority = o.split("://").nth(1).unwrap_or("");
            authority.split(':').next().unwrap_or("").to_string()
        })
        .unwrap_or_default()
}

// ─── 重定向解析与校验 ────────────────────────────────────

/// 解析跳转目标并校验（同主机 + 禁止 https → http 降级）
///
/// 对应上游 `resolveAccessCodeRedirect`：允许 http/https 跳转、
/// hostname 必须一致、不得从 https 降级到 http。返回最终完整 URL。
fn resolve_access_code_redirect(current: &str, location: &str) -> Result<String, AccessCodeError> {
    let src_origin =
        normalize_origin(current).ok_or_else(|| AccessCodeError::Network("访问码验证地址无效".to_string()))?;

    // 拼接目标完整 URL
    let dst_full = if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if location.starts_with('/') {
        format!("{src_origin}{location}")
    } else {
        // 相对路径：以 origin 根目录解析（上游 URL.join 等价行为）
        format!("{src_origin}/{location}")
    };

    let dst_origin = normalize_origin(&dst_full)
        .ok_or_else(|| AccessCodeError::Network("访问码验证地址无效".to_string()))?;

    // 跨主机校验
    let src_host = origin_hostname(&src_origin);
    let dst_host = origin_hostname(&dst_origin);
    if src_host != dst_host {
        return Err(AccessCodeError::Network(
            "访问码验证拒绝跨主机或不安全重定向".to_string(),
        ));
    }

    // 禁止 https → http 降级
    if src_origin.starts_with("https://") && dst_origin.starts_with("http://") {
        return Err(AccessCodeError::Network(
            "访问码验证拒绝跨主机或不安全重定向".to_string(),
        ));
    }

    Ok(dst_full)
}

/// 从 Set-Cookie 头中提取 name=value（忽略属性段）
fn parse_set_cookie(value: &str) -> Option<String> {
    let first = value.split(';').next().unwrap_or("").trim();
    let sep = first.find('=')?;
    if sep <= 0 {
        return None;
    }
    let name = first[..sep].trim();
    if name.is_empty() {
        return None;
    }
    Some(format!("{}={}", name, first[sep + 1..].trim()))
}

// ─── 序列化与合成 ────────────────────────────────────────

/// 序列化 Cookie 列表（排除 mode / trim-mc-token，按名排序）
///
/// 对应上游 `serializeCookies`。
fn serialize_cookies(pairs: Vec<String>) -> String {
    let mut kept: Vec<(String, String)> = Vec::new();
    for pair in pairs {
        let name = pair.split('=').next().unwrap_or("").to_string();
        if name.is_empty() || EXCLUDED_COOKIE_NAMES.contains(&name.to_lowercase().as_str()) {
            continue;
        }
        let value = pair.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default();
        kept.push((name, value));
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    kept
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 合成 Cookie header（对应上游 `composeCookieHeader`）
///
/// 合并多个来源的 Cookie 片段，按名去重、剔除 mode / trim-mc-token、
/// 尾部统一追加 `mode=relay`。
pub fn compose_cookie_header(values: &[&str]) -> String {
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut names = std::collections::HashSet::new();

    for value in values {
        for raw in value.split(';') {
            let part = raw.trim();
            let Some(sep) = part.find('=') else { continue };
            if sep <= 0 {
                continue;
            }
            let name = part[..sep].trim().to_string();
            let normalized = name.to_lowercase();
            if name.is_empty()
                || normalized == "mode"
                || normalized == "trim-mc-token"
                || names.contains(&normalized)
            {
                continue;
            }
            names.insert(normalized);
            parts.push((name, part[sep + 1..].trim().to_string()));
        }
    }

    parts.push(("mode".to_string(), "relay".to_string()));
    parts
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

// ─── Base64 编码（对应 `encodeAccessCode`）──────────────

pub fn encode_access_code(access_code: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    STANDARD.encode(access_code.as_bytes())
}

// ─── 访问码验证（对应 `establishAccessCodeSession`）──────

/// 建立访问码网关会话
///
/// - access_code 为空：清空授权表并直接返回（不触发验证，等同未启用访问码）
/// - access_code 非空：GET {base}/access_code_verify 携带
///   `x-access-code: base64(code)` 与 `x-access-source: web`，手动跟随重定向
///   （最多 5 跳、同主机、禁止降级），收集各跳 Set-Cookie 作为网关会话
///   Cookie，成功后写入内存授权表。
pub async fn verify_access_code(
    base_url: &str,
    access_code: &str,
) -> Result<AccessCodeSession, AccessCodeError> {
    let initial_origin = normalize_origin(base_url)
        .ok_or_else(|| AccessCodeError::Network("访问码验证地址无效".to_string()))?;
    let normalized_code = access_code.trim().to_string();

    // 每次登录只有一个活动授权：清空历史 grant（含早期端口跳转 origin）
    clear_access_grants();

    if normalized_code.is_empty() {
        return Ok(AccessCodeSession {
            base_url: initial_origin.clone(),
            cookie: String::new(),
        });
    }

    let encoded = encode_access_code(&normalized_code);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AccessCodeError::Network(format!("构建 HTTP 客户端失败: {e}")))?;

    let mut current = format!("{initial_origin}/access_code_verify");
    let mut redirects: u32 = 0;
    let mut collected_cookies: Vec<String> = Vec::new();

    loop {
        let resp = client
            .get(&current)
            .header("x-access-code", &encoded)
            .header("x-access-source", "web")
            .send()
            .await
            .map_err(|e| {
                AccessCodeError::Network(format!("无法连接到访问码验证服务: {e}"))
            })?;

        // 收集本跳 Set-Cookie（网关会话可能在任何一跳下发）
        for cookie in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(raw) = cookie.to_str() {
                if let Some(pair) = parse_set_cookie(raw) {
                    collected_cookies.push(pair);
                }
            }
        }

        let status = resp.status().as_u16();

        // 手动跟随重定向
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if location.is_empty() {
                return Err(AccessCodeError::Network(
                    "访问码验证服务返回无效重定向".to_string(),
                ));
            }
            redirects += 1;
            if redirects > MAX_REDIRECTS {
                return Err(AccessCodeError::Network(
                    "访问码验证重定向次数过多".to_string(),
                ));
            }
            current = resolve_access_code_redirect(&current, &location)?;
            continue;
        }

        // 非重定向：丢弃响应体（仅关心状态与 Set-Cookie）
        drop(resp);

        if REJECTED_STATUS_CODES.contains(&status) {
            return Err(AccessCodeError::Rejected("访问码错误".to_string()));
        }
        if !(200..300).contains(&status) {
            return Err(AccessCodeError::Network(format!(
                "访问码验证服务返回 HTTP {status}"
            )));
        }
        break;
    }

    let resolved_origin =
        normalize_origin(&current).unwrap_or(initial_origin.clone());
    let cookie = serialize_cookies(collected_cookies);
    if cookie.is_empty() {
        return Err(AccessCodeError::Network(
            "访问码验证成功但未建立网关会话".to_string(),
        ));
    }

    set_access_grant(&resolved_origin, &cookie)
        .map_err(AccessCodeError::Network)?;

    Ok(AccessCodeSession {
        base_url: resolved_origin,
        cookie,
    })
}

// ─── 单元测试 ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_origin_handles_scheme_path_and_userinfo() {
        assert_eq!(
            normalize_origin("http://192.168.1.10:5666/v/api/v1/login").unwrap(),
            "http://192.168.1.10:5666"
        );
        assert_eq!(normalize_origin("https://fn.example.com").unwrap(), "https://fn.example.com");
        assert_eq!(normalize_origin("http://user:pass@host:8000/x").unwrap(), "http://host:8000");
        assert!(normalize_origin("ftp://host").is_none());
        assert!(normalize_origin("").is_none());
    }

    #[test]
    fn encode_access_code_uses_standard_base64() {
        // Buffer.from('hello', 'utf8').toString('base64') = aGVsbG8=
        assert_eq!(encode_access_code("hello"), "aGVsbG8=");
        // 中文（UTF-8 多字节）
        assert_eq!(encode_access_code("访问码"), "6K6/6Zeu56CB");
    }

    #[test]
    fn redirect_allows_same_host_and_port_change() {
        let target = resolve_access_code_redirect(
            "http://192.168.1.10:5666/access_code_verify",
            "http://192.168.1.10:5667/login",
        )
        .unwrap();
        assert_eq!(target, "http://192.168.1.10:5667/login");
    }

    #[test]
    fn redirect_allows_relative_location() {
        let target = resolve_access_code_redirect(
            "http://host:5666/access_code_verify",
            "/fnos/login",
        )
        .unwrap();
        assert_eq!(target, "http://host:5666/fnos/login");
    }

    #[test]
    fn redirect_rejects_cross_host() {
        let err = resolve_access_code_redirect(
            "http://192.168.1.10:5666/access_code_verify",
            "http://evil.example.com/steal",
        )
        .unwrap_err();
        assert!(matches!(err, AccessCodeError::Network(_)));
    }

    #[test]
    fn redirect_rejects_https_downgrade() {
        let err = resolve_access_code_redirect(
            "https://host:5666/access_code_verify",
            "https://host:5666/a",
        )
        .unwrap();
        assert!(err.starts_with("https://"));
        let err2 = resolve_access_code_redirect(
            "https://host:5666/access_code_verify",
            "http://host:5666/a",
        )
        .unwrap_err();
        assert!(matches!(err2, AccessCodeError::Network(_)));
    }

    #[test]
    fn serialize_excludes_mode_and_trim_mc_token_sorted() {
        let out = serialize_cookies(vec![
            "b=2".to_string(),
            "Trim-MC-token=abc".to_string(),
            "mode=relay".to_string(),
            "a=1".to_string(),
        ]);
        assert_eq!(out, "a=1; b=2");
    }

    #[test]
    fn compose_header_dedupes_and_appends_mode_relay() {
        let out = compose_cookie_header(&["a=1; mode=relay", "b=2; a=3; trim-mc-token=x"]);
        assert_eq!(out, "a=1; b=2; mode=relay");
    }

    #[test]
    fn grants_are_keyed_by_normalized_origin() {
        clear_access_grants();
        set_access_grant("http://host:5666/", "sid=xyz").unwrap();
        assert_eq!(get_access_cookie_header("http://host:5666/v"), "sid=xyz");
        assert_eq!(get_access_cookie_header("https://host:5666"), "");
        clear_access_grants();
        assert_eq!(get_access_cookie_header("http://host:5666"), "");
    }
}
