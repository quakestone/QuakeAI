#!/usr/bin/env bash
# 构建浏览器侧工作流引擎内核：Rust → wasm32 → wasm-opt -Oz → src/engine_wasm.bin。
# 产物由 src/lib.rs 的 include_bytes! 编入网关 Worker，随 /wf/kernel.wasm 分发。
#
# 何时需要重跑：改了 engine/src/lib.rs 之后。纯前端（admin_ui.html）改动不需要。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo build (wasm32) =="
cargo build --release --target wasm32-unknown-unknown --manifest-path engine/Cargo.toml

WASM=engine/target/wasm32-unknown-unknown/release/workflow_engine.wasm
OUT=src/engine_wasm.bin

# wasm-opt：优先 PATH，其次 worker-build 缓存（CI 免 Rust 环境不跑本脚本，无碍）
WO="$(command -v wasm-opt || true)"
if [ -z "$WO" ]; then
  for c in /root/.cache/worker-build/wasm-opt-*/bin/wasm-opt; do
    [ -x "$c" ] && WO="$c" && break
  done
fi
if [ -n "$WO" ]; then
  echo "== wasm-opt -Oz =="
  "$WO" -Oz --enable-bulk-memory --strip-debug "$WASM" -o "$OUT.tmp"
  mv "$OUT.tmp" "$OUT"
else
  echo "（未找到 wasm-opt，用未压缩产物）"
  cp "$WASM" "$OUT"
fi

echo "✅ $OUT  $(stat -c%s "$OUT") bytes"
