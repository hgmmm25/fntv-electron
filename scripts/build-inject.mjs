// scripts/build-inject.mjs
// 將 src-tauri/inject/ 下的 TypeScript 注入腳本打包為單一 IIFE JS 檔案，
// 供 Tauri 透過 initialization_script() 注入到遠端網頁。
//
// 使用：
//   node scripts/build-inject.mjs          — 單次打包（production / beforeBuildCommand）
//   node scripts/build-inject.mjs --watch  — 監聽模式（development / beforeDevCommand）
//
// Watch Mode 使用 esbuild context API：
//   1. 啟動時先做一次完整打包
//   2. 之後監聽 inject/ 下所有 .ts 檔案變更，自動增量重建
//   3. 重建後 Tauri 的文件監聽器會偵測到 preload.iife.js 變更，
//      自動觸發 cargo 重編譯（因為 lib.rs 用 include_str! 嵌入此檔案）
//   4. App 自動重啟，載入新版注入腳本

import esbuild from 'esbuild';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, '..');

const entry = resolve(projectRoot, 'src-tauri/inject/index.ts');
const outfile = resolve(projectRoot, 'src-tauri/inject/preload.iife.js');

const isWatch = process.argv.includes('--watch');

/** 共用的 esbuild 配置（不含 minify/sourcemap，由各模式覆蓋） */
const baseOptions = {
    entryPoints: [entry],
    bundle: true,
    format: 'iife',
    target: 'es2022',
    platform: 'browser',
    outfile,
    logLevel: 'info',
    define: {
        'process.env.NODE_ENV': isWatch ? '"development"' : '"production"',
    },
};

if (isWatch) {
    // ── Watch Mode：開發時使用 ──────────────────────────
    // 不壓縮、帶 sourcemap，方便 devtools debug
    const ctx = await esbuild.context({
        ...baseOptions,
        minify: false,
        sourcemap: 'inline',
    });

    // 首次打包
    await ctx.rebuild();
    console.log(`✅ 注入腳本初始打包完成: ${outfile}`);

    // 啟動檔案監聽
    await ctx.watch();
    console.log('');
    console.log(`👀 Watch 模式已啟動，正在監聽 inject/ 目錄變更...`);
    console.log(`   入口:  ${entry}`);
    console.log(`   輸出:  ${outfile}`);
    console.log('');
    console.log('   修改 inject/ 下的 .ts 檔案後會自動重新打包。');
    console.log('   Tauri 會偵測到檔案變更並自動重啟 App。');
    console.log('');
} else {
    // ── 單次打包：production / beforeBuildCommand 使用 ──
    try {
        await esbuild.build({
            ...baseOptions,
            minify: true,
        });

        console.log(`✅ 注入腳本已打包: ${outfile}`);
    } catch (err) {
        console.error('❌ 打包注入腳本失敗:', err);
        process.exit(1);
    }
}
