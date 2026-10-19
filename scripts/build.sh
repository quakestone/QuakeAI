#!/usr/bin/env bash
# 一键构建：Rust → wasm32 → worker-build → 追加 ResourceStore DO 类。
# 产物：build/index.js + build/index_bg.wasm（部署必需的两个模块）。
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo >/dev/null || { echo "需要 Rust 工具链：https://rustup.rs"; exit 1; }
rustup target add wasm32-unknown-unknown

if ! command -v worker-build >/dev/null; then
  echo "== 安装 worker-build (^0.8) =="
  cargo install worker-build --version "^0.8"
fi

echo "== worker-build --release =="
worker-build --release

# 追加 Durable Object 类（workerd 静态解析 `export class ResourceStore extends
# DurableObject` 完成注册；wasm-bindgen 的 re-export + Proxy 识别不到）。
echo "== 追加 ResourceStore DO 类 =="
cat scripts/storage-do.js >> build/index.js

# worker-build 会生成 build/.gitignore（内容为 `*`），它会把部署必需的
# index.js / index_bg.wasm 一并忽略（深层 .gitignore 优先于上级放行规则）——
# 删掉它，让根 .gitignore 的 !/build/index.js 放行生效（云端 CI 免 Rust 的关键）。
rm -f build/.gitignore

echo "✅ 构建完成：build/index.js + build/index_bg.wasm"
