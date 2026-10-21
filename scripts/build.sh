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

echo "✅ 构建完成：build/index.js + build/index_bg.wasm（runtime=browser-wasm，已脱钩 CF）"
