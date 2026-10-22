#!/usr/bin/env bash
# UI 重建 B2 门禁一键验证：构建 → 子路径静态托管 → mock 上游 → Playwright 实测
# 用法：bash scripts/test-ui-g2.sh
set -euo pipefail
cd "$(dirname "$0")/.."

PORT_UI=8932
PORT_MOCK=8788
ROOT=/tmp/ui-g2-root

echo "== 1. 构建 =="
bash scripts/build.sh

echo "== 2. 子路径静态托管（$ROOT/QuakeAI/ → :$PORT_UI）=="
rm -rf "$ROOT"; mkdir -p "$ROOT/QuakeAI"
cp -r build/. "$ROOT/QuakeAI/"
pkill -f "[h]ttp.server $PORT_UI" 2>/dev/null || true
nohup python3 -m http.server "$PORT_UI" --bind 127.0.0.1 --directory "$ROOT" >/tmp/ui-g2-http.log 2>&1 &
for i in $(seq 1 20); do curl -sf --max-time 5 "http://127.0.0.1:$PORT_UI/QuakeAI/index.html" >/dev/null 2>&1 && break; sleep 0.2; done
curl -sf "http://127.0.0.1:$PORT_UI/QuakeAI/index.html" >/dev/null && echo "   静态服务就绪" || { echo "静态服务未就绪"; exit 1; }

echo "== 3. mock 上游（:$PORT_MOCK，清空旧接收日志） =="
pkill -f "[m]ock-zhipu" 2>/dev/null || true
sleep 0.3
: > mock-received.log
nohup node scripts/mock-zhipu.mjs >/tmp/ui-g2-mock.log 2>&1 &
for i in $(seq 1 20); do curl -sf --max-time 5 "http://127.0.0.1:$PORT_MOCK/models" >/dev/null 2>&1 && break; sleep 0.2; done
curl -sf "http://127.0.0.1:$PORT_MOCK/models" >/dev/null && echo "   mock 就绪" || { echo "mock 未就绪"; exit 1; }

echo "== 4. Playwright G2 实测（B2 服务商与模型管理） =="
python3 scripts/g2-verify.py "http://127.0.0.1:$PORT_UI/QuakeAI/"
