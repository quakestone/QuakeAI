#!/usr/bin/env bash
# 一键构建：Rust → wasm32 → wasm-bindgen(--target web) → 新 UI 壳（src/ui/）组装
# 产物布局（相对路径，兼容 GitHub Pages 子路径托管）：
#   build/index.html            应用壳（src/ui/index.html + 版本号注入）
#   build/assets/css|js/*       UI 源码原样复制
#   build/assets/wasm/*         wasm-bindgen 胶水（index.js + index_bg.wasm）
#   build/sw.js|manifest|icon   PWA 资产
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo >/dev/null || { echo "需要 Rust 工具链：https://rustup.rs"; exit 1; }
rustup target add wasm32-unknown-unknown

echo "== cargo build --release (wasm32-unknown-unknown) =="
cargo build --release --target wasm32-unknown-unknown

echo "== wasm-bindgen (--target web) → build/assets/wasm/ =="
# 优先用与 Cargo.lock 锁定的 wasm-bindgen 0.2.129 一致的二进制；
# worker-build 缓存目录里的就是 0.2.129，而 PATH 上的可能偏旧导致 schema 不匹配。
WB="$(command -v wasm-bindgen)"
CACHED="$HOME/.cache/worker-build/wasm-bindgen-x86_64-unknown-linux-musl-0.2.129/wasm-bindgen"
if [ -x "$CACHED" ]; then WB="$CACHED"; fi
mkdir -p build/assets/wasm
"$WB" "target/wasm32-unknown-unknown/release/zhipu_wasm_gateway.wasm" \
  --out-dir build/assets/wasm --target web --out-name index

# 旧版产物位于 build/ 根（index.js / index_bg.wasm），新布局已迁到 assets/wasm/，
# 清掉根部的陈旧副本，避免旧引用混入生产包。worker-build 生成的 build/.gitignore
# （内容为 `*`）会连部署必需文件一起忽略，一并删除。
rm -f build/index.js build/index_bg.wasm build/index.d.ts build/index_bg.wasm.d.ts build/.gitignore

echo "== 组装 UI 壳（src/ui/ → build/，注入版本号 __BUILD_VERSION__）=="
PKG_VER="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
mkdir -p build/assets/css build/assets/js
cp -f src/ui/assets/css/*.css build/assets/css/
cp -f src/ui/assets/js/*.js build/assets/js/
python3 - "$PKG_VER" <<'PY'
import sys, pathlib
ver = sys.argv[1]
src = pathlib.Path("src/ui/index.html").read_text(encoding="utf-8")
out = src.replace('"__BUILD_VERSION__"', '"' + ver + '"')
pathlib.Path("build/index.html").write_text(out, encoding="utf-8")
print("   stamped index.html ->", ver)
PY

echo "== 复制 PWA 资产（sw.js / manifest / 图标）=="
cp -f src/sw.js build/sw.js
cp -f src/manifest.webmanifest build/manifest.webmanifest
cp -f src/icon.svg build/icon.svg

echo "== 复制内置通道清单（服务商页「一键导入」数据源）=="
# 源在仓库根（历史位置不变）；页面经相对路径 ./gateway-channels.json 拉取，
# 子路径托管（/QuakeAI/）时随 index.html 一同解析，故必须落在 build/ 根。
cp -f gateway-channels.json build/gateway-channels.json

echo "✅ 构建完成：build/index.html + build/assets/{css,js,wasm}（纯静态 PWA，相对路径）"
