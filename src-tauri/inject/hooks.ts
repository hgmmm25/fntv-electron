// src-tauri/inject/hooks.ts
// 簡化版 Hook 系統（移植自 src/preload/core/hooks.ts，純 JS 實作）

export enum HookType {
    OnReady = 'onReady',
    OnDomChange = 'onDomChange',
}

type HookFn = (...args: unknown[]) => void;

const hooks: Record<HookType, HookFn[]> = {
    [HookType.OnReady]: [],
    [HookType.OnDomChange]: [],
};

export function registerHook(type: HookType, fn: HookFn): void {
    hooks[type].push(fn);
}

export function runHooks(type: HookType, ...args: unknown[]): void {
    hooks[type].forEach((fn) => {
        try {
            fn(...args);
        } catch (e) {
            console.error('[inject] hook 執行失敗:', e);
        }
    });
}
