// src-tauri/inject/plugins/scrollFix.ts
// 首页轮播翻页按钮修复补丁
//
// 网站自身 JS 的 React ref 未正确绑定到 .ms-container 滚动容器，
// 导致点右翻箭头时报 "Cannot read properties of null (reading 'scrollLeft')"。
//
// 本补丁拦截轮播区域的左/右翻页按钮点击，自行找到最近的可横向滚动容器
// 并执行 scrollBy，同时阻止网站原始 handler 执行（stopImmediatePropagation）。

import { registerHook, HookType } from '../hooks';
import logger from '../logger';

/** 单次滚动像素量 */
const SCROLL_AMOUNT = 400;

/**
 * 从元素向上查找最近的可横向滚动祖先容器
 */
function findScrollableContainer(start: Element): HTMLElement | null {
    let el: Element | null = start;
    while (el && el !== document.body) {
        const html = el as HTMLElement;
        const style = window.getComputedStyle(html);
        const overflowX = style.overflowX;
        if (
            (overflowX === 'auto' || overflowX === 'scroll') &&
            html.scrollWidth > html.clientWidth
        ) {
            return html;
        }
        el = el.parentElement;
    }
    return null;
}

/**
 * 检测元素是否为轮播翻页箭头按钮
 *
 * 目标结构：
 *   <div class="... rounded-full ...">
 *     <svg><path d="M..."/></svg>   ← 左或右箭头
 *   </div>
 *
 * 判断条件：
 * 1. 是 rounded-full 的 div（圆形按钮）
 * 2. 内含 SVG 且 path 的 d 属性像箭头（M 开头 + 大量 L/Q/C 指令）
 * 3. 上级容器内有可横向滚动的区域（排除其他圆形 SVG 按钮）
 */
function isCarouselArrow(target: EventTarget): { el: HTMLDivElement; direction: 'left' | 'right' } | null {
    // 点击可能落在 SVG 或 path 上，向上找最近的 rounded-full div
    let el: Element | null = target as Element;
    let arrowEl: HTMLDivElement | null = null;

    for (let i = 0; i < 5 && el && el !== document.body; i++) {
        if (
            el instanceof HTMLDivElement &&
            el.classList.contains('rounded-full') &&
            el.classList.contains('cursor-pointer')
        ) {
            arrowEl = el;
            break;
        }
        el = el.parentElement;
    }

    if (!arrowEl) return null;

    // 必须包含 SVG
    const svg = arrowEl.querySelector('svg');
    if (!svg) return null;

    // 必须是小尺寸圆形按钮（轮播箭头的典型特征：size-8 / size-10 等）
    const rect = arrowEl.getBoundingClientRect();
    if (rect.width < 20 || rect.width > 60 || rect.height < 20 || rect.height > 60) {
        return null;
    }

    // 最后验证：这个按钮附近确实有可横向滚动容器（避免误伤其他圆形按钮）
    const container = findScrollableContainer(arrowEl);
    if (!container) return null;

    // ── 位置校验：排除容器内部的底部按钮 ──────────────────────
    //
    // 真正的轮播箭头满足：
    // 1. 不是滚动容器的后代（是容器的兄弟或覆盖层）
    // 2. 水平中心在容器左/右边缘 80px 以内
    //
    // 底部导航按钮（播放/详情/收藏等）是容器的后代，会被过滤掉。
    const containerRect = container.getBoundingClientRect();
    const btnCenterX = rect.left + rect.width / 2;
    const EDGE_THRESHOLD = 80;

    const isDescendant = container.contains(arrowEl);
    if (isDescendant) return null;

    const nearLeftEdge = Math.abs(btnCenterX - containerRect.left) < EDGE_THRESHOLD;
    const nearRightEdge = Math.abs(btnCenterX - containerRect.right) < EDGE_THRESHOLD;
    if (!nearLeftEdge && !nearRightEdge) return null;

    // 位置即方向：左边 → 左翻，右边 → 右翻
    const direction: 'left' | 'right' = nearLeftEdge ? 'left' : 'right';

    return { el: arrowEl, direction };
}

/**
 * 注册拦截器
 */
function interceptCarouselArrows(): void {
    // 使用捕获阶段拦截，在网站 handler 之前执行
    document.addEventListener(
        'click',
        (e: MouseEvent) => {
            const result = isCarouselArrow(e.target);
            if (!result) return;

            const { el, direction } = result;
            const container = findScrollableContainer(el);
            if (!container) return;

            // 阻止网站原始 handler（它会因 ref 为 null 而报错）
            e.preventDefault();
            e.stopPropagation();
            e.stopImmediatePropagation();

            const delta = direction === 'right' ? SCROLL_AMOUNT : -SCROLL_AMOUNT;
            container.scrollBy({ left: delta, behavior: 'smooth' });

            logger.info(`[scrollFix] ${direction} scroll → ${delta}px`);
        },
        true, // 捕获阶段
    );
}

// ─── 註冊 Hook ─────────────────────────────────────────────────────

registerHook(HookType.OnReady, interceptCarouselArrows);
