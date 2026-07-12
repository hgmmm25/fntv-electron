// src-tauri/inject/index.ts
// Tauri 注入腳本入口
//
// 對應 src/preload/index.ts 的職責：
// 1. 暴露 logger 到 window（供遠端網頁內聯腳本使用）
// 2. 建立 window.electronAPI 兼容層（供依賴 Electron preload 的頁面使用）
// 3. 明確載入所有插件（Tauri 無 fs.readdirSync，需靜態 import）
// 4. 在 DOM Ready 時執行 OnReady hooks
// 5. 透過 MutationObserver 監聽 DOM 變化，執行 OnDomChange hooks
//
// 本檔案經 esbuild 打包為單一 IIFE，由 Tauri 的 initialization_script 注入。
//
// ## 關於 @tauri-apps/api 與 withGlobalTauri
//
// 本注入腳本使用 `window.__TAURI__.core.invoke()`（由 withGlobalTauri: true 注入的
// 官方公開 API），而非更低階的 `window.__TAURI_INTERNALS__.invoke()`。
// withGlobalTauri: true 會將 @tauri-apps/api 掛載到 window.__TAURI__，提供：
// - window.__TAURI__.core.invoke()     — IPC 呼叫
// - window.__TAURI__.event.listen()    — 事件監聽
// 這些是官方穩定的公開介面，版本相容性有保障。

// ─── 載入核心模組 ──────────────────────────────────────────────────
import { HookType, registerHook, runHooks } from './hooks';
import { setupElectronAPIShim } from './bridge';
import logger from './logger';

// ─── 明確載入所有插件 ──────────────────────────────────────────────
// （Eelectron 版本用 fs.readdirSync 自動掃描 plugins/ 目錄；
//   Tauri 注入環境無 Node.js 檔案系統 API，改為靜態 import）
import './plugins/playButton';
import './plugins/playMaskButton';
import './plugins/titlebar';
import './plugins/scrollFix';

// 暴露 logger 到 window（與 Electron 版本行為一致）
declare global {
    interface Window {
        log: typeof logger;
        logger: typeof logger;
    }
}

window.log = logger;
window.logger = logger;

// ─── 建立 window.electronAPI 兼容層 ────────────────────────────────
//
// 必須在插件載入後、DOM Ready 前建立，讓遠端頁面代碼能立即使用 electronAPI。
setupElectronAPIShim();

// ─── DOM 初始化 hook 觸發 ──────────────────────────────────────────

// ─── 隱藏詳情頁播放按鈕 ─────────────────────────────────────────────
// 詳情頁的播放按鈕（semi-button-primary + !min-w-[150px]）不起作用，
// 透過注入 CSS 將其隱藏。
function hideDetailPlayButton(): void {
    const style = document.createElement('style');
    style.id = 'fn-hide-detail-play-btn';
    style.textContent = `
        button.semi-button.semi-button-primary[class*="!min-w-[150px]"] {
            display: none !important;
        }
        /* 隐藏顶部功能栏 */
        div.box-border.flex.w-full.px-4.flex-col {
            display: none !important;
        }
        /* 隐藏半透明分割线 */
        div.semi-divider.semi-divider-horizontal {
            display: none !important;
        }
    `;
    document.head.appendChild(style);
}

function initInjector(): void {
    window.log = logger;
    window.logger = logger;

    // 註冊隱藏詳情頁播放按鈕
    registerHook(HookType.OnReady, hideDetailPlayButton);

    if (document.readyState !== 'loading') {
        runHooks(HookType.OnReady);
        observeDomChanges();
    } else {
        document.addEventListener('DOMContentLoaded', () => {
            runHooks(HookType.OnReady);
            observeDomChanges();
        });
    }
}

function observeDomChanges(): void {
    const observer = new MutationObserver(() => runHooks(HookType.OnDomChange));
    observer.observe(document.body, { childList: true, subtree: true });
}

initInjector();
