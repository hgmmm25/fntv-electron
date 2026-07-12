#!/usr/bin/env bash
# =============================================================================
# build-proxy-tauri.sh
# 编译 Go proxy 模块并以 Tauri sidecar 命名约定放置到 src-tauri/binaries/
#
# 用法:
#   ./scripts/build-proxy-tauri.sh [target]
#
# target 可选值:
#   win-x64     (默认)   Windows x64
#   mac-x64              macOS x64 (Intel)
#   mac-arm64            macOS ARM64 (Apple Silicon)
#   mac-universal        macOS Universal (需要 lipo)
#   linux-x64            Linux x64
#   all                  全部平台交叉编译
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
PROXY_SRC="$PROJECT_ROOT/src/modules/proxy"
BINARIES_DIR="$PROJECT_ROOT/src-tauri/binaries"

# ─── Go 工具链检查 ─────────────────────────────────────────────────
if ! command -v go &>/dev/null; then
  echo "⚠️  Go 未安装或不在 PATH 中，跳过 proxy 编译"
  echo "   请安装 Go: https://go.dev/dl/"
  echo "   当前使用 src-tauri/binaries/ 中的占位文件"
  echo ""
  echo "📦  src-tauri/binaries/ 目录内容:"
  ls -lh "$BINARIES_DIR"
  exit 0
fi

# Go 源码目录校验
if [ ! -d "$PROXY_SRC" ]; then
  echo "❌  Go proxy 源码目录不存在: $PROXY_SRC"
  exit 1
fi

mkdir -p "$BINARIES_DIR"

# ─── 目标平台定义 ───────────────────────────────────────────────────
build_win_x64() {
  echo "🔨  编译 proxy → Windows x64"
  cd "$PROXY_SRC"
  GOOS=windows GOARCH=amd64 go build -trimpath -ldflags="-s -w" \
    -o "$BINARIES_DIR/proxy-x86_64-pc-windows-msvc.exe"
  echo "✅  $BINARIES_DIR/proxy-x86_64-pc-windows-msvc.exe"
}

build_mac_x64() {
  echo "🔨  编译 proxy → macOS x64"
  cd "$PROXY_SRC"
  GOOS=darwin GOARCH=amd64 go build -trimpath -ldflags="-s -w" \
    -o "$BINARIES_DIR/proxy-x86_64-apple-darwin"
  echo "✅  $BINARIES_DIR/proxy-x86_64-apple-darwin"
}

build_mac_arm64() {
  echo "🔨  编译 proxy → macOS ARM64"
  cd "$PROXY_SRC"
  GOOS=darwin GOARCH=arm64 go build -trimpath -ldflags="-s -w" \
    -o "$BINARIES_DIR/proxy-aarch64-apple-darwin"
  echo "✅  $BINARIES_DIR/proxy-aarch64-apple-darwin"
}

build_mac_universal() {
  echo "🔗  合并 proxy → macOS Universal"
  build_mac_x64
  build_mac_arm64
  if command -v lipo &>/dev/null; then
    lipo -create \
      "$BINARIES_DIR/proxy-x86_64-apple-darwin" \
      "$BINARIES_DIR/proxy-aarch64-apple-darwin" \
      -output "$BINARIES_DIR/proxy-universal-apple-darwin"
    echo "✅  Universal binary 已生成"
  else
    echo "⚠️  lipo 不可用，跳过 Universal binary 合并"
    echo "   请在 macOS 上运行，或分别使用 x64/arm64 版本"
  fi
}

build_linux_x64() {
  echo "🔨  编译 proxy → Linux x64"
  cd "$PROXY_SRC"
  GOOS=linux GOARCH=amd64 go build -trimpath -ldflags="-s -w" \
    -o "$BINARIES_DIR/proxy-x86_64-unknown-linux-gnu"
  echo "✅  $BINARIES_DIR/proxy-x86_64-unknown-linux-gnu"
}

# ─── 执行 ───────────────────────────────────────────────────────────
TARGET="${1:-win-x64}"

case "$TARGET" in
  win-x64)
    build_win_x64
    ;;
  mac-x64)
    build_mac_x64
    ;;
  mac-arm64)
    build_mac_arm64
    ;;
  mac-universal)
    build_mac_universal
    ;;
  linux-x64)
    build_linux_x64
    ;;
  all)
    build_win_x64
    build_mac_universal
    build_linux_x64
    ;;
  *)
    echo "❌  未知目标: $TARGET"
    echo ""
    echo "可用目标:"
    echo "  win-x64       Windows x64 (默认)"
    echo "  mac-x64       macOS x64 (Intel)"
    echo "  mac-arm64     macOS ARM64 (Apple Silicon)"
    echo "  mac-universal macOS Universal"
    echo "  linux-x64     Linux x64"
    echo "  all           全部平台"
    exit 1
    ;;
esac

echo ""
echo "📦  src-tauri/binaries/ 目录内容:"
ls -lh "$BINARIES_DIR"
