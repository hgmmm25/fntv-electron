// src-tauri/inject/index.ts
// Tauri 注入腳本入口
//
// 對應 src/preload/index.ts 的職責：
// 1. 暴露 logger 到 window（供遠端網頁內聯腳本使用）
// 2. 明確載入所有插件（Tauri 無 fs.readdirSync，需靜態 import）
// 3. 在 DOM Ready 時執行 OnReady hooks
// 4. 透過 MutationObserver 監聽 DOM 變化，執行 OnDomChange hooks
//
// 本檔案經 esbuild 打包為單一 IIFE，由 Tauri 的 initialization_script 注入。

// ─── 載入核心模組 ──────────────────────────────────────────────────
import { HookType, runHooks } from './hooks';
import logger from './logger';

// ─── 明確載入所有插件 ──────────────────────────────────────────────
// （Eelectron 版本用 fs.readdirSync 自動掃描 plugins/ 目錄；
//   Tauri 注入環境無 Node.js 檔案系統 API，改為靜態 import）
import './plugins/playButton';
import './plugins/playMaskButton';
import './plugins/titlebar';

// 暴露 logger 到 window（與 Electron 版本行為一致）
declare global {
    interface Window {
        log: typeof logger;
        logger: typeof logger;
    }
}

window.log = logger;
window.logger = logger;

// ─── DOM 初始化 hook 觸發 ──────────────────────────────────────────

function initInjector(): void {
    window.log = logger;
    window.logger = logger;

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
