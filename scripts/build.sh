#!/usr/bin/env bash
# 一键构建：Rust → wasm32 → wasm-bindgen(--target web) → build/index.js + build/index_bg.wasm
# 产物直接由浏览器（admin_ui.html）以 ES module 方式 import，不再依赖 Cloudflare Worker。
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo >/dev/null || { echo "需要 Rust 工具链：https://rustup.rs"; exit 1; }
rustup target add wasm32-unknown-unknown

echo "== cargo build --release (wasm32-unknown-unknown) =="
cargo build --release --target wasm32-unknown-unknown

echo "== wasm-bindgen (--target web) =="
# 优先用与 Cargo.lock 锁定的 wasm-bindgen 0.2.129 一致的二进制；
# worker-build 缓存目录里的就是 0.2.129，而 PATH 上的可能偏旧导致 schema 不匹配。
WB="$(command -v wasm-bindgen)"
CACHED="$HOME/.cache/worker-build/wasm-bindgen-x86_64-unknown-linux-musl-0.2.129/wasm-bindgen"
if [ -x "$CACHED" ]; then WB="$CACHED"; fi
"$WB" "target/wasm32-unknown-unknown/release/zhipu_wasm_gateway.wasm" \
  --out-dir build --target web --out-name index

# worker-build 会生成 build/.gitignore（内容为 `*`），它会把部署必需的
# index.js / index_bg.wasm 一并忽略（深层 .gitignore 优先于上级放行规则）——
# 删掉它，让根 .gitignore 的 !/build/index.js 放行生效。
rm -f build/.gitignore

echo "== 生成静态入口 index.html（注入版本号 __BUILD_VERSION__）=="
PKG_VER="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
python3 - "$PKG_VER" <<'PY'
import sys, re, pathlib
ver = sys.argv[1]
src = pathlib.Path("src/admin_ui.html").read_text(encoding="utf-8")
# 版本占位：BUILD_MARK = "__BUILD_VERSION__" → 注入真实版本，缓存自检才有意义
out = src.replace('"__BUILD_VERSION__"', '"' + ver + '"')
out = out.replace("__BUILD_VERSION__", ver)
pathlib.Path("build/index.html").write_text(out, encoding="utf-8")
print("   stamped index.html ->", ver)
PY

echo "== 复制 PWA 资产（sw.js / manifest / 图标）=="
cp -f src/sw.js build/sw.js 2>/dev/null || true
cp -f src/manifest.webmanifest build/manifest.webmanifest 2>/dev/null || true
cp -f src/icon.svg build/icon.svg 2>/dev/null || true

echo "✅ 构建完成：build/index.html + build/index.js + build/index_bg.wasm（runtime=browser-wasm，已脱钩 CF，纯静态 PWA）"
