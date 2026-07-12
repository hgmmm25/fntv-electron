// src-tauri/inject/bridge.ts
// Tauri 腳本注入橋接器
//
// 在被注入的遠端網頁環境中，透過 window.__TAURI__.core.invoke() 與
// Rust 後端安全地通訊，取代 Electron 的 ipcRenderer.send / ipcRenderer.invoke。
//
// 安全性：
// 1. 所有 invoke 都走 __TAURI__.core（官方公開 API），Tauri 核心會驗證 capabilities
// 2. 載荷包成 { payload } 單一參數，避免 command 引數展開問題
// 3. invoke 失敗時 reject，呼叫端可自行處理（不吞錯）
//
// ## window.electronAPI 兼容層
//
// 為了與依賴 Electron preload API 的遠端頁面保持兼容，本模組在最後
// 建立 `window.electronAPI` 物件，映射 Electron 的 IPC 介面：
//   - send(channel, ...args)  → invoke('channel', { payload: args })
//   - invoke(channel, ...args) → invoke('channel', { payload: args })
//   - on(channel, callback)   → Tauri event listen (需配合 Rust 端 emit)
//   - once(channel, callback)  → Tauri event once
//
// 注意：Tauri 的事件系統（emit/listen）與 Electron 的 IPC 通道不同，
// on/once 僅適用於 Rust 端透過 `app.emit()` 發送的事件。
//
// ## 關於 __TAURI__ 與 __TAURI_INTERNALS__
//
// 本模組使用 `window.__TAURI__.core.invoke()`（官方公開 API），而非
// `window.__TAURI_INTERNALS__.invoke()`（內部 API）。原因：
// 1. __TAURI__ 是官方穩定的公開介面，版本相容性有保障
// 2. __TAURI_INTERNALS__ 是內部實現細節，可能在未來版本中變更或移除
// 3. withGlobalTauri: true 確保 __TAURI__ 在初始化腳本執行時已就緒

declare global {
    interface Window {
        __TAURI__?: {
            core?: {
                invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
            };
            event?: {
                listen: (event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>;
            };
        };
        electronAPI?: {
            send: (channel: string, ...args: unknown[]) => void;
            invoke: (channel: string, ...args: unknown[]) => Promise<unknown>;
            on: (channel: string, callback: (...args: unknown[]) => void) => () => void;
            once: (channel: string, callback: (...args: unknown[]) => void) => () => void;
        };
    }
}

/** 檢查 Tauri 官方 API 是否可用 */
export function isTauri(): boolean {
    return typeof window !== 'undefined'
        && typeof window.__TAURI__?.core?.invoke === 'function';
}

// ─── invoke 可用性快取 ──────────────────────────────────────
// 當 invoke 因 ACL 被拒時，記住該 command 並不再重試，
// 避免播放影片時產生數千次無效 IPC 請求導致卡頓。
const blockedCommands = new Set<string>();

function isBlocked(command: string): boolean {
    return blockedCommands.has(command);
}

/**
 * fire-and-forget（替代 ipcRenderer.send）
 *
 * @param command Rust 端的 #[tauri::command] 名稱（snake_case）
 * @param data    要傳遞的資料，會包成 { payload: data }
 */
export async function send(command: string, data?: unknown): Promise<void> {
    if (!isTauri() || isBlocked(command)) return;
    try {
        const args = data !== undefined ? { payload: data } : {};
        await window.__TAURI__!.core!.invoke(command, args);
    } catch (e) {
        // 若因 ACL 被拒，記住並靜默
        if (String(e).includes('not allowed') || String(e).includes('Plugin not found')) {
            blockedCommands.add(command);
        } else {
            console.error(`[bridge] send("${command}") 失敗:`, e);
        }
    }
}

/**
 * request-reply（替代 ipcRenderer.invoke）
 *
 * @param command Rust 端的 #[tauri::command] 名稱
 * @param args    命名引數物件，會直接傳給 invoke
 * @returns       Rust command 的回傳值
 */
export async function invoke<T = unknown>(
    command: string,
    args?: Record<string, unknown>,
): Promise<T> {
    if (!isTauri()) {
        throw new Error(`Tauri 不可用，無法 invoke "${command}"`);
    }
    if (isBlocked(command)) {
        throw new Error(`[bridge] ${command} 已被快取跳過（ACL 限制）`);
    }
    return window.__TAURI__!.core!.invoke(command, args ?? {}) as Promise<T>;
}

// ─── 通道抽象 ─────────────────────────────────────────────────────
// 將原本分散在各插件的 ipcRenderer.send / .on 邏輯集中成語意化 API，
// 讓插件不需要知道通道名稱的對應關係。

/** 播放電影（替代 ipcRenderer.send('play-movie', playData)） */
export function playMovie(data: { id: string; token: string; sourceIndex: number }): Promise<void> {
    return send('play_movie', data);
}

/**
 * 視窗控制（替代 ipcRenderer.send('window-minimize' / 'window-maximize' / 'window-close')）
 *
 * 基於安全考量，不直接讓遠端網頁呼叫 Tauri 的 window plugin，
 * 而是走自訂的 Rust command（window_minimize / window_toggle_maximize / window_close），
 * 由 Rust 端統一操作視窗。這樣遠端頁面只能透過我們定義的窄介面控制視窗。
 */
export function windowMinimize(): Promise<void> {
    return send('window_minimize');
}
export function windowMaximize(): Promise<void> {
    return send('window_toggle_maximize');
}
export function windowClose(): Promise<void> {
    return send('window_close');
}

/**
 * 取得播放按鈕配置（替代 get-play-button-config + play-button-config-info 的來回）
 *
 * 原本 Electron 用兩個通道（send 請求 + on 監聽回覆 + setTimeout 超時），
 * Tauri 改為單一 invoke 即可同步取回，更安全也更簡潔。
 * 保留 timeout 語意以維持原行為（2 秒後回退到預設值）。
 */
export async function getPlayButtonConfig(): Promise<{ hideOriginalPlayButton: boolean }> {
    const fallback = { hideOriginalPlayButton: true };
    if (!isTauri() || isBlocked('get_play_button_config')) return fallback;

    try {
        const timeout = new Promise<{ hideOriginalPlayButton: boolean }>((resolve) =>
            setTimeout(() => resolve(fallback), 2000),
        );
        const result = (await Promise.race([
            invoke<{ hideOriginalPlayButton: boolean }>('get_play_button_config'),
            timeout,
        ])) as { hideOriginalPlayButton: boolean } | undefined;
        return result ?? fallback;
    } catch (e) {
        // 若因 ACL 被拒，靜默記住並停止重試
        if (String(e).includes('not allowed') || String(e).includes('Plugin not found') || String(e).includes('ACL')) {
            blockedCommands.add('get_play_button_config');
        }
        return fallback;
    }
}

/** 記錄前端日誌（替代 ipcRenderer.invoke('log-message', level, ...args)） */
export function logMessage(level: string, args: unknown[]): void {
    if (!isTauri() || isBlocked('log_message')) return;
    // 截斷每個參數，防止超大物件（如 fetch 回應體）導致 JSON.stringify 溢位
    const truncated = args.map((a) => safeStringify(a, 4096));
    invoke('log_message', { level, args: truncated }).catch((e) => {
        // 若因 ACL 被拒，靜默記住並停止重試
        if (String(e).includes('not allowed') || String(e).includes('Plugin not found') || String(e).includes('ACL')) {
            blockedCommands.add('log_message');
        }
    });
}

// ─── 設定檔管理 Bridge ──────────────────────────────────────

/**
 * 取得設定資料（config + history）
 *
 * 對應 Electron 的 `ipcRenderer.send('get-config')` + `ipcRenderer.on('config-data', ...)`
 * Tauri 改為單一 invoke 同步回傳。
 */
export function getConfig(): Promise<{ config: Record<string, unknown>; history: Array<Record<string, unknown>> }> {
    return invoke('get_config');
}

/**
 * 儲存登入設定
 *
 * 對應 Electron 的 `saveConfig({ account, domain, token, useHttps })`
 */
export function saveLoginConfig(data: {
    account: string;
    domain: string;
    token: string;
    useHttps?: boolean;
}): Promise<void> {
    return invoke('save_login_config', {
        account: data.account,
        domain: data.domain,
        token: data.token,
        useHttps: data.useHttps,
    });
}

/**
 * 取得歷史記錄
 */
export function getHistory(): Promise<Array<{
    domain: string;
    account: string;
    password: string;
    useHttps?: boolean;
}>> {
    return invoke('get_history');
}

/**
 * 新增歷史記錄
 */
export function addHistory(data: {
    domain: string;
    account: string;
    password: string;
    useHttps?: boolean;
}): Promise<void> {
    return invoke('add_history', {
        domain: data.domain,
        account: data.account,
        password: data.password,
        useHttps: data.useHttps,
    });
}

/**
 * 清空歷史記錄
 */
export function clearHistory(): Promise<void> {
    return invoke('clear_history');
}

/**
 * 刪除單筆歷史記錄
 */
export function deleteHistoryItem(data: { domain: string; account: string }): Promise<boolean> {
    return invoke('delete_history_item', {
        domain: data.domain,
        account: data.account,
    });
}

// ─── 登入認證 Bridge ─────────────────────────────────────────

/**
 * 登入
 *
 * 對應 Electron 的 `ipcRenderer.send('login', loginData)`
 * Tauri 改為 invoke 回傳結果。
 */
export function login(data: {
    domain: string;
    username: string;
    password: string;
    useHttps?: boolean;
}): Promise<{ success: boolean; domain?: string; token?: string; message?: string }> {
    return invoke('login', {
        payload: {
            domain: data.domain,
            username: data.username,
            password: data.password,
            useHttps: data.useHttps,
        },
    });
}

/**
 * 恢復 Cookie
 *
 * 對應 Electron 的 `restoreCookies(domain, token)`
 * 透過 webview eval 設定 Trim-MC-token 和 mode=relay cookie。
 */
export function restoreCookies(domain: string, token: string): Promise<void> {
    return invoke('restore_cookies', { domain, token });
}

// ─── 下載代理設定 Bridge ─────────────────────────────────────

/**
 * 取得下載代理設定
 */
export function getDownloadProxyConfig(): Promise<{ enabled: boolean; proxyUrl: string }> {
    return invoke('get_download_proxy_config');
}

/**
 * 設定下載代理
 */
export function setDownloadProxyConfig(data: {
    enabled?: boolean;
    proxyUrl?: string;
}): Promise<void> {
    return invoke('set_download_proxy_config', {
        enabled: data.enabled,
        proxyUrl: data.proxyUrl,
    });
}

/**
 * 設定「隱藏原有播放按鈕」
 */
export function setHideOriginalPlayButton(hide: boolean): Promise<void> {
    return invoke('set_hide_original_play_button', { hide });
}

// ─── window.electronAPI 兼容層 ──────────────────────────────

/**
 * 建立 `window.electronAPI` 兼容物件。
 *
 * 這讓依賴 Electron preload API 的遠端頁面代碼能透明運行。
 * 映射關係：
 *   - electronAPI.send(channel, ...args)    →  Tauri invoke(channel, { payload: args })
 *   - electronAPI.invoke(channel, ...args)  →  Tauri invoke(channel, { payload: args })
 *   - electronAPI.on(channel, callback)     →  Tauri event listener（需配合 Rust emit）
 *   - electronAPI.once(channel, callback)   →  Tauri 一次性 event listener
 *
 * 安全注意：
 * - 只有已知的 channel 允許通過（白名單機制）
 * - 未知 channel 會被攔截並記錄警告
 */
export function setupElectronAPIShim(): void {
    if (typeof window === 'undefined') return;
    if (window.electronAPI) return; // 避免重複設定

    /** 已知的安全 channel 白名單 */
    const KNOWN_CHANNELS = new Set([
        'play-movie',
        'get-play-button-config',
        'log-message',
        'window-minimize',
        'window-maximize',
        'window-close',
        'get-config',
        'save-config',
        'get-history',
        'add-history',
        'clear-history',
        'delete-history-item',
        'login',
        'restore-cookies',
        'get-download-proxy-config',
        'set-download-proxy-config',
        'set-hide-original-play-button',
    ]);

    /**
     * MPV 事件名稱映射：前端 channel 名 → Rust emit 事件名
     *
     * Rust 端透過 `app_handle.emit("mpv-progress", ...)` 發送事件，
     * 前端透過 `electronAPI.on('progress', ...)` 接收。此映射連接兩端。
     */
    const EVENT_NAME_MAP: Record<string, string> = {
        'progress': 'mpv-progress',
        'exit': 'mpv-exit',
        'error': 'mpv-error',
    };

    /**
     * Electron channel 名稱 → Tauri command 名稱映射
     * Electron 使用 kebab-case，Tauri 使用 snake_case
     */
    const channelToCommand: Record<string, string> = {
        'play-movie': 'play_movie',
        'get-play-button-config': 'get_play_button_config',
        'log-message': 'log_message',
        'window-minimize': 'window_minimize',
        'window-maximize': 'window_toggle_maximize',
        'window-close': 'window_close',
        'get-config': 'get_config',
        'save-config': 'save_login_config',
        'get-history': 'get_history',
        'add-history': 'add_history',
        'clear-history': 'clear_history',
        'delete-history-item': 'delete_history_item',
        'login': 'login',
        'restore-cookies': 'restore_cookies',
        'get-download-proxy-config': 'get_download_proxy_config',
        'set-download-proxy-config': 'set_download_proxy_config',
        'set-hide-original-play-button': 'set_hide_original_play_button',
    };

    function mapChannel(channel: string): string | null {
        const cmd = channelToCommand[channel];
        if (cmd) return cmd;
        if (!KNOWN_CHANNELS.has(channel)) {
            console.warn(`[electronAPI] 未知的 channel: "${channel}"，已攔截`);
        }
        return null;
    }

    window.electronAPI = {
        /**
         * fire-and-forget（對應 ipcRenderer.send）
         * 所有參數包進 payload 傳給 Tauri command
         */
        send(channel: string, ...args: unknown[]): void {
            const cmd = mapChannel(channel);
            if (!cmd) return;
            const payload = args.length === 1 ? args[0] : args;
            sendToRust(cmd, payload);
        },

        /**
         * request-reply（對應 ipcRenderer.invoke）
         * 回傳 Promise，可被 await
         */
        invoke(channel: string, ...args: unknown[]): Promise<unknown> {
            const cmd = mapChannel(channel);
            if (!cmd) return Promise.reject(new Error(`未知的 channel: ${channel}`));
            const payload = args.length === 1 ? args[0] : args;
            return invokeFromRust(cmd, typeof payload === 'object' && payload !== null
                ? payload as Record<string, unknown>
                : { payload });
        },

        /**
         * 事件監聽（對應 ipcRenderer.on）
         * 使用 Tauri 的 listen API（如果可用）
         */
        on(channel: string, callback: (...args: unknown[]) => void): () => void {
            if (!window.__TAURI__?.event?.listen) {
                console.warn('[electronAPI] Tauri listen API 不可用');
                return () => {};
            }

            // MPV 事件：透過 EVENT_NAME_MAP 映射到 Rust emit 名稱；
            // 其他 channel 保持原有內部前綴行為。
            const eventName = EVENT_NAME_MAP[channel] ?? `app://internal/${channel}`;
            let unlistenFn: (() => void) | null = null;

            window.__TAURI__!.event!.listen(eventName, (event) => {
                callback(event.payload);
            }).then((unlisten) => {
                unlistenFn = unlisten;
            }).catch((e) => {
                console.warn(`[electronAPI] listen("${channel}") 失敗:`, e);
            });

            return () => {
                unlistenFn?.();
            };
        },

        /**
         * 一次性事件監聽（對應 ipcRenderer.once）
         */
        once(channel: string, callback: (...args: unknown[]) => void): () => void {
            if (!window.__TAURI__?.event?.listen) {
                return () => {};
            }

            const eventName = EVENT_NAME_MAP[channel] ?? `app://internal/${channel}`;
            let unlistenFn: (() => void) | null = null;

            window.__TAURI__!.event!.listen(eventName, (event) => {
                callback(event.payload);
                unlistenFn?.();
            }).then((unlisten) => {
                unlistenFn = unlisten;
            }).catch((e) => {
                console.warn(`[electronAPI] once("${channel}") 失敗:`, e);
            });

            return () => {
                unlistenFn?.();
            };
        },
    };

    console.info('[bridge] window.electronAPI 已建立（Electron 兼容層）');

    // 內部 helper——避免與上方模組級 send/invoke 函數名稱衝突
    function sendToRust(command: string, data?: unknown): void {
        if (!isTauri()) return;
        const args = data !== undefined ? { payload: data } : {};
        window.__TAURI__!.core!.invoke(command, args).catch((e) => {
            console.error(`[electronAPI] send("${command}") 失敗:`, e);
        });
    }

    function invokeFromRust(command: string, args?: Record<string, unknown>): Promise<unknown> {
        if (!isTauri()) return Promise.reject(new Error('Tauri 不可用'));
        return window.__TAURI__!.core!.invoke(command, args ?? {});
    }
}

function safeStringify(value: unknown, maxLen = 0): string {
    if (typeof value === 'string') {
        return maxLen > 0 && value.length > maxLen ? value.slice(0, maxLen) + '…[truncated]' : value;
    }
    try {
        const s = JSON.stringify(value);
        if (maxLen > 0 && s.length > maxLen) {
            return s.slice(0, maxLen) + '…[truncated]';
        }
        return s;
    } catch {
        return String(value).slice(0, maxLen || 2048);
    }
}
