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

    // 判断箭头方向：通过 SVG path 的大致形状
    const path = svg.querySelector('path');
    if (!path) return null;
    const d = path.getAttribute('d') || '';

    // 右箭头：path 中 x 坐标递增的趋势（包含 L + 较大 x 值）
    // 左箭头：path 中 x 坐标递减的趋势
    // 简单判断：看 path 后半段的 x 值是否大于前半段
    const numbers = d.match(/[\d.]+/g);
    if (!numbers || numbers.length < 4) return null;

    const mid = Math.floor(numbers.length / 2);
    const firstHalfX = numbers.slice(0, mid).reduce((s, n) => s + parseFloat(n), 0) / mid;
    const secondHalfX = numbers.slice(mid).reduce((s, n) => s + parseFloat(n), 0) / (numbers.length - mid);

    const direction = secondHalfX > firstHalfX ? 'right' : 'left';

    // 最后验证：这个按钮附近确实有可滚动容器（避免误伤其他圆形按钮）
    const container = findScrollableContainer(arrowEl);
    if (!container) return null;

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
