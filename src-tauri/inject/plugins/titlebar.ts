// src-tauri/inject/plugins/titlebar.ts
// 自定義標題列插件（移植自 src/preload/plugins/titlebar.ts）
//
// 重要：視窗控制的 click 改為「事件代理」綁在 document（捕獲階段），
// 並在 OnDomChange 時重新注入標題列。原因是登入後的首頁為遠端 React SPA，
// React mount / 重渲染會擾動 document.body，導致原本直接綁在按鈕上的
// click listener 失效（按鈕可見但點擊無反應）。事件代理綁在 document 上，
// 不受按鈕節點被重建/搬移影響，可永久生效。

import { registerHook, HookType } from '../hooks';
import logger from '../logger';
import { windowMinimize, windowMaximize, windowClose, isTauri, _debugIsBlocked as isBlockedDebug } from '../bridge';

// ── DEBUG：模組載入標記 ────────────────────────────────────
console.log('%c[titlebar] MODULE LOADED', 'color:#0a0;font-weight:bold',
    '| url:', location.href,
    '| readyState:', document.readyState,
    '| __TAURI__:', typeof window.__TAURI__,
    '| invoke:', typeof window.__TAURI__?.core?.invoke);

const TITLEBAR_ID = 'custom-titlebar';

/** 按鈕 hover 效果對應的內部形狀選擇器 */
const HOVER_SHAPES: Record<string, { selector: string; isClose: boolean }> = {
    'min-btn': { selector: 'path, rect', isClose: false },
    'max-btn': { selector: 'path, rect', isClose: false },
    'close-btn': { selector: 'path', isClose: true },
};

/**
 * 判斷點擊目標是否為自訂標題列內的指定按鈕。
 *
 * 用 closest 向上查找，相容點擊落在 SVG / path / rect 上的情況；
 * 並要求按鈕位於 #custom-titlebar 內，避免與遠端頁面其他同 id 元素衝突。
 */
function getTitlebarButton(target: EventTarget | null, id: string): HTMLButtonElement | null {
    if (!(target instanceof Element)) return null;
    const btn = target.closest(`#${id}`) as HTMLButtonElement | null;
    if (btn && btn.closest(`#${TITLEBAR_ID}`)) return btn;
    return null;
}

function injectTitleBar(): void {
    console.log('[titlebar] injectTitleBar() called, url:', location.href);
    if (document.getElementById(TITLEBAR_ID)) {
        console.log('[titlebar] already exists, skip');
        return;
    }
    if (!document.body) {
        console.warn('[titlebar] document.body 不存在，跳過標題列注入');
        return;
    }

    const bar = document.createElement('div');
    bar.id = TITLEBAR_ID;
    bar.setAttribute('data-tauri-drag-region', '');
    bar.style.cssText = `
        height:32px;width:100vw;background:rgba(255,255,255,0)!important;
        backdrop-filter:blur(12px)!important;-webkit-app-region:drag;
        position:fixed;top:0;left:0;z-index:99999;
        display:flex;justify-content:flex-end;align-items:center;
        transition:background 0.3s ease;
    `;

    bar.innerHTML = `
        <div id="titlebar-btns" style="-webkit-app-region:no-drag;display:flex;gap:2px;padding-right:4px;">
            <button id="min-btn" type="button" style="
                background:transparent;border:none;width:34px;height:32px;
                display:flex;align-items:center;justify-content:center;
                cursor:pointer;border-radius:4px;transition:all 0.2s ease;
            ">
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none">
                    <path d="M2 8H14" stroke="#888" stroke-width="1.5" stroke-linecap="round"/>
                </svg>
            </button>
            <button id="max-btn" type="button" style="
                background:transparent;border:none;width:34px;height:32px;
                display:flex;align-items:center;justify-content:center;
                cursor:pointer;border-radius:4px;transition:all 0.2s ease;
            ">
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none">
                    <rect x="3" y="3" width="10" height="10" rx="1.5" stroke="#888" stroke-width="1.5"/>
                </svg>
            </button>
            <button id="close-btn" type="button" style="
                background:transparent;border:none;width:34px;height:32px;
                display:flex;align-items:center;justify-content:center;
                cursor:pointer;border-radius:4px;transition:all 0.2s ease;
            ">
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none">
                    <path d="M4 4L12 12M12 4L4 12" stroke="#888" stroke-width="1.5" stroke-linecap="round"/>
                </svg>
            </button>
        </div>
    `;

    document.body.style.paddingTop = '10px';
    document.documentElement.style.overflowY = 'hidden';
    document.body.appendChild(bar);

    bindHoverEffects();
}

/** 按鈕互動效果（直接綁定；每次（重新）注入時呼叫一次） */
function bindHoverEffects(): void {
    for (const id of Object.keys(HOVER_SHAPES)) {
        const btn = document.getElementById(id) as HTMLButtonElement | null;
        if (!btn) continue;
        const { selector, isClose } = HOVER_SHAPES[id];

        btn.addEventListener('mouseenter', () => {
            btn.style.background = isClose ? 'rgba(232, 17, 35, 0.2)' : 'rgba(0, 0, 0, 0.06)';
            const shape = btn.querySelector(selector) as SVGElement | null;
            if (shape) shape.style.stroke = '#fff';
        });

        btn.addEventListener('mouseleave', () => {
            btn.style.background = 'transparent';
            const shape = btn.querySelector(selector) as SVGElement | null;
            if (shape) shape.style.stroke = '#888';
        });
    }
}

/**
 * 視窗控制：以事件代理綁在 document 的捕獲階段。
 *
 * 監聽器掛在 document 而非按鈕節點上，即使遠端 SPA（React）重渲染、
 * 搬移或重建標題列按鈕，點擊仍能正確觸發視窗控制。
 */
let delegationBound = false;
function bindWindowControlDelegation(): void {
    if (delegationBound) return;
    delegationBound = true;
    console.log('[titlebar] 綁定視窗控制事件代理, isTauri():', isTauri());

    document.addEventListener(
        'click',
        (e: MouseEvent) => {
            const minBtn = getTitlebarButton(e.target, 'min-btn');
            const maxBtn = getTitlebarButton(e.target, 'max-btn');
            const closeBtn = getTitlebarButton(e.target, 'close-btn');

            if (minBtn) {
                e.preventDefault();
                e.stopPropagation();
                windowMinimize();
            } else if (maxBtn) {
                e.preventDefault();
                e.stopPropagation();
                windowMaximize();
            } else if (closeBtn) {
                e.preventDefault();
                e.stopPropagation();
                windowClose();
            }
        },
        true,
    );
}


// ══════════════════════════════════════════════════════════════════
// F11 / Esc 全螢幕快捷鍵
// ══════════════════════════════════════════════════════════════════

let shortcutsInitialized = false;

function initFullscreenShortcuts(): void {
    if (shortcutsInitialized) return;
    shortcutsInitialized = true;

    document.addEventListener('keydown', async (e: KeyboardEvent) => {
        if (!isTauri()) return;

        // F11 — 切換全螢幕
        if (e.key === 'F11') {
            e.preventDefault();
            e.stopPropagation();
            try {
                // @ts-expect-error __TAURI__.window is injected by withGlobalTauri
                const win = window.__TAURI__?.window?.getCurrentWindow();
                if (!win) return;
                const isFS = await win.isFullscreen();
                if (isFS) {
                    await win.setFullscreen(false);
                } else {
                    await win.setFullscreen(true);
                }
            } catch (err) {
                logger.error('[titlebar] F11 切換全螢幕失敗:', err);
            }
            return;
        }

        // Esc — 退出全螢幕
        if (e.key === 'Escape') {
            try {
                // @ts-expect-error __TAURI__.window is injected by withGlobalTauri
                const win = window.__TAURI__?.window?.getCurrentWindow();
                if (!win) return;
                const isFS = await win.isFullscreen();
                if (isFS) {
                    e.preventDefault();
                    e.stopPropagation();
                    await win.setFullscreen(false);
                }
            } catch (err) {
                logger.error('[titlebar] Esc 退出全螢幕失敗:', err);
            }
        }
    }, true);
}


// ══════════════════════════════════════════════════════════════════
// 模組載入 & Hook 註冊
// ══════════════════════════════════════════════════════════════════

bindWindowControlDelegation();

if (isTauri()) {
    try { initFullscreenShortcuts(); } catch (e) {
        console.error('[titlebar] initFullscreenShortcuts 失敗:', e);
    }
}

registerHook(HookType.OnReady, injectTitleBar);
registerHook(HookType.OnDomChange, injectTitleBar);
