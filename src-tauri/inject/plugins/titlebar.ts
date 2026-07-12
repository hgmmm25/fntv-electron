// src-tauri/inject/plugins/titlebar.ts
// 自定義標題列插件（移植自 src/preload/plugins/titlebar.ts）
//
// 變更：
// - ipcRenderer.send('window-minimize/maximize/close')
//   → Tauri plugin:window commands（透過 bridge.send）
// - 移除 Electron 特有 import

import { registerHook, HookType } from '../hooks';
import logger from '../logger';
import { windowMinimize, windowMaximize, windowClose } from '../bridge';

function injectTitleBar(): void {
    logger.info('Injecting custom title bar...');
    if (document.getElementById('custom-titlebar')) return;

    const bar = document.createElement('div');
    bar.id = 'custom-titlebar';
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
            <button id="min-btn" style="
                background:transparent;border:none;width:34px;height:32px;
                display:flex;align-items:center;justify-content:center;
                cursor:pointer;border-radius:4px;transition:all 0.2s ease;
            ">
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none">
                    <path d="M2 8H14" stroke="#888" stroke-width="1.5" stroke-linecap="round"/>
                </svg>
            </button>
            <button id="max-btn" style="
                background:transparent;border:none;width:34px;height:32px;
                display:flex;align-items:center;justify-content:center;
                cursor:pointer;border-radius:4px;transition:all 0.2s ease;
            ">
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none">
                    <rect x="3" y="3" width="10" height="10" rx="1.5" stroke="#888" stroke-width="1.5"/>
                </svg>
            </button>
            <button id="close-btn" style="
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

    // 按鈕互動效果
    const buttonIds: Array<{ id: string; selector: string }> = [
        { id: 'min-btn', selector: 'path, rect' },
        { id: 'max-btn', selector: 'path, rect' },
        { id: 'close-btn', selector: 'path' },
    ];

    for (const { id, selector } of buttonIds) {
        const btn = document.getElementById(id) as HTMLButtonElement;
        if (!btn) continue;

        btn.addEventListener('mouseenter', () => {
            if (id === 'close-btn') {
                btn.style.background = 'rgba(232, 17, 35, 0.2)';
            } else {
                btn.style.background = 'rgba(0, 0, 0, 0.06)';
            }
            const path = btn.querySelector(selector) as SVGElement;
            if (path) path.style.stroke = '#fff';
        });

        btn.addEventListener('mouseleave', () => {
            btn.style.background = 'transparent';
            const path = btn.querySelector(selector) as SVGElement;
            if (path) path.style.stroke = '#888';
        });
    }

    // 視窗控制
    const minBtn = document.getElementById('min-btn');
    const maxBtn = document.getElementById('max-btn');
    const closeBtn = document.getElementById('close-btn');

    if (minBtn) minBtn.addEventListener('click', () => windowMinimize());
    if (maxBtn) maxBtn.addEventListener('click', () => windowMaximize());
    if (closeBtn) closeBtn.addEventListener('click', () => windowClose());
}

// ─── 註冊 Hook ─────────────────────────────────────────────────────

registerHook(HookType.OnReady, injectTitleBar);
