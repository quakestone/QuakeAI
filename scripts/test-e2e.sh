#!/usr/bin/env bash
# 端到端验证：mock 智谱 + workerd(本地 Workers 运行时) + WASM 网关
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# workerd 路径：优先本地 .tools/workerd，其次环境变量 WORKERD_BIN
if [ -n "${WORKERD_BIN:-}" ]; then WORKERD="$WORKERD_BIN";
elif [ -x "$ROOT/.tools/workerd" ]; then WORKERD="$ROOT/.tools/workerd";
else WORKERD=/root/.codebuddy/artifact/workerd/workerd; fi
MOCK_LOG="$ROOT/mock-received.log"
PASS=0; FAIL=0

check() { # name, condition(0/1), detail
  if [ "$2" -eq 0 ]; then PASS=$((PASS+1)); echo "  [PASS] $1"; else FAIL=$((FAIL+1)); echo "  [FAIL] $1 -- $3"; fi
}

start_workerd() { # $1=capnp 文件（项目内相对路径） $2=keep（非空则保留 .wd-storage 不清）
  # workerd.config.capnp 的 DO 用 localDisk（.wd-storage/）持久化；其余 capnp 为
  # inMemory。默认清掉它保持"每段 fresh DO"的语义；持久化验证段（12.10）传 keep。
  if [ -z "${2:-}" ]; then rm -rf "$ROOT/.wd-storage"; fi
  mkdir -p "$ROOT/.wd-storage"
  (setsid "$WORKERD" serve "$1" > "$ROOT/workerd.log" 2>&1 &)
  for i in $(seq 1 30); do curl -sf http://127.0.0.1:8787/health >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
stop_workerd() {
  # 按 8787 端口反查真实 workerd pid：setsid 可能 fork，$! 未必是 workerd 本身；
  # 杀不干净会让下一段 start 的新实例绑定失败，而 readiness 却打到旧实例上，
  # 导致后续段实际跑的是上一段配置。
  pids=$(ss -tlnpH 2>/dev/null | grep ':8787 ' | grep -oP 'pid=\K[0-9]+' | sort -u)
  [ -n "$pids" ] && kill $pids 2>/dev/null
  rm -f /tmp/wd.pid
  # 等端口真正释放（不只是 health 不通——健康检查也可能命中正在退出的旧实例）
  for i in $(seq 1 30); do
    ss -tlnpH 2>/dev/null | grep -q ':8787 ' || return 0
    kill $pids 2>/dev/null
    sleep 0.2
  done
}

echo "== 1. 启动 mock 智谱 (127.0.0.1:8788) =="
pkill -f "mock-zhipu\.mjs" 2>/dev/null
for i in $(seq 1 30); do curl -sf http://127.0.0.1:8788/models >/dev/null 2>&1 || break; sleep 0.2; done
# setsid：脱离本脚本会话，避免脚本被子进程清理逻辑带走（保持与 workerd 一致）
(setsid node "$ROOT/scripts/mock-zhipu.mjs" > "$ROOT/mock.log" 2>&1 &)
MOCK_PID=$(pgrep -f "mock-zhipu\.mjs" | head -1)
rm -f "$MOCK_LOG"; touch "$MOCK_LOG"
for i in $(seq 1 20); do curl -sf http://127.0.0.1:8788/models >/dev/null 2>&1 && break; sleep 0.2; done
curl -sf http://127.0.0.1:8788/models >/dev/null; check "mock 已就绪" $? "mock not reachable"

echo "== 2. 启动 workerd（默认单通道） =="
stop_workerd; sleep 0.3
start_workerd workerd.config.capnp; check "workerd 已就绪" $? "worker not reachable"

echo "== 3. GET /health =="
BODY=$(curl -s http://127.0.0.1:8787/health)
echo "$BODY" | grep -q '"status":"ok"'; check "health 返回 ok" $? "$BODY"
echo "$BODY" | grep -q 'stream-retry-first-packet'; check "health 标志流式重试能力" $? "$BODY"
echo "$BODY" | grep -q 'multipart-audio-video-upload'; check "health 标志 multipart 能力" $? "$BODY"

echo "== 4. 非流式（透传 + 默认模型） =="
RESP=$(curl -sD /tmp/hdr.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"你好"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "默认模型注入 glm-4-flash" $? "$RESP"
echo "$RESP" | grep -q '非流式经由 WASM Worker'; check "上游回复内容透传" $? "$RESP"
AUTH=$(tail -1 "$MOCK_LOG" | python3 -c 'import json,sys; print(json.load(sys.stdin)["authorization"])')
check "上游收到 Authorization" $([ "$AUTH" = "Bearer test-key-local" ] && echo 0 || echo 1) "$AUTH"

echo "== 5. 流式 SSE 透传 =="
SSE=$(curl -sN http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4.7-flash","stream":true,"messages":[{"role":"user","content":"hi"}]}')
echo "$SSE" | grep -q 'data:'; check "SSE data 帧到达" $? "no data frames"
echo "$SSE" | grep -q 'data: \[DONE\]'; check "SSE 结束帧 [DONE]" $? "no [DONE]"
echo "$SSE" | grep -q 'WASM 流式透传验证'; check "SSE 内容完整" $? "content mismatch"

echo "== 6. 429 透传（降权信号） =="
# 临时动态渠道：提供 glm-test-429（无同类回退）→ 429 原样透传，不自动换模型
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"tmp-429","provider":"zhipu","name":"tmp-429","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","models":["glm-test-429"],"kind":"chat","priority":300}' > /dev/null
CODE=$(curl -s -o /tmp/err.json -w '%{http_code}' http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}')
check "429 原样透传" $([ "$CODE" = "429" ] && echo 0 || echo 1) "got $CODE"
grep -q '速率限制' /tmp/err.json; check "429 上游错误体透传" $? "$(cat /tmp/err.json)"
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/tmp-429 > /dev/null
stop_workerd

echo "== 7. 多通道：首包前失败(502) -> 自动换通道 =="
cat > "$ROOT/workerd.retry.capnp" << 'CAPNP'
using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [ (name = "main", worker = .w), ( name = "internet", network = ( allow = ["public","private"], tlsOptions = (trustBrowserCas = true) ) ) ],
  sockets = [ ( name = "http", address = "*:8787", http = (), service = "main" ) ],
);
const w :Workerd.Worker = (
  modules = [ (name = "index.js", esModule = embed "build/index.js"), (name = "index_bg.wasm", wasm = embed "build/index_bg.wasm") ],
  compatibilityDate = "2026-10-01",
  durableObjectNamespaces = [ (className = "ResourceStore", uniqueKey = "gw-resource-store-key") ],
  durableObjectStorage = (inMemory = void),
  bindings = [
    (name = "ZHIPU_API_KEY", text = "test-key-local"),
    (name = "ZHIPU_CHANNELS", text = "[{\"name\":\"ch-fail\",\"url\":\"http://127.0.0.1:8788/fail\"},{\"name\":\"ch-ok\",\"url\":\"http://127.0.0.1:8788\"}]"),
    (name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore"),
    (name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore"),
  ],
);
CAPNP
start_workerd workerd.retry.capnp; check "workerd(重试配置) 已就绪" $? "retry worker not reachable"
RESP=$(curl -sD /tmp/h2.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "502 后换通道成功拿到响应" $? "$RESP"
grep -qi 'x-zhipu-channel: ch-ok' /tmp/h2.txt; check "最终通道 = ch-ok" $? "$(cat /tmp/h2.txt | tr -d '\r')"
grep -qi 'x-zhipu-attempts: ch-fail=HTTP502' /tmp/h2.txt; check "attempts 记录 502 失败" $? "$(cat /tmp/h2.txt | tr -d '\r')"
stop_workerd

echo "== 8. 首帧前断连(200+0帧) -> 自动换通道 =="
sed 's|/fail|/flaky-empty|g; s|ch-fail|ch-blank|g' "$ROOT/workerd.retry.capnp" > "$ROOT/workerd.retry2.capnp"
start_workerd workerd.retry2.capnp; check "workerd(断连配置) 已就绪" $? "retry2 worker not reachable"
SSE=$(curl -sN http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"stream":true,"messages":[{"role":"user","content":"hi"}]}')
echo "$SSE" | grep -q 'data: \[DONE\]'; check "首包前断连后重试拿到完整流" $? "$SSE"
CH=$(curl -s -o /dev/null -D - http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"stream":true,"messages":[{"role":"user","content":"hi"}]}' | tr -d '\r' | grep -i 'x-zhipu-attempts' || true)
echo "  attempts: $CH"
echo "$CH" | grep -qE 'conn-reset-before-first|conn-failed'; check "attempts 记录首帧前失败" $? "$CH"
echo "$CH" | grep -q 'ch-blank'; check "失败通道记录为 ch-blank" $? "$CH"
stop_workerd

echo "== 9. 已吐字不切换：断流只收尾，不重投其他通道 =="
cat > "$ROOT/workerd.flaky.capnp" << 'CAPNP'
using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [ (name = "main", worker = .w), ( name = "internet", network = ( allow = ["public","private"], tlsOptions = (trustBrowserCas = true) ) ) ],
  sockets = [ ( name = "http", address = "*:8787", http = (), service = "main" ) ],
);
const w :Workerd.Worker = (
  modules = [ (name = "index.js", esModule = embed "build/index.js"), (name = "index_bg.wasm", wasm = embed "build/index_bg.wasm") ],
  compatibilityDate = "2026-10-01",
  durableObjectNamespaces = [ (className = "ResourceStore", uniqueKey = "gw-resource-store-key") ],
  durableObjectStorage = (inMemory = void),
  bindings = [
    (name = "ZHIPU_API_KEY", text = "test-key-local"),
    (name = "ZHIPU_CHANNELS", text = "[{\"name\":\"ch-flaky\",\"url\":\"http://127.0.0.1:8788/flaky\"}]"),
    (name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore"),
    (name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore"),
  ],
);
CAPNP
BEFORE=$(grep -c '/flaky/chat/completions' "$MOCK_LOG" 2>/dev/null)
start_workerd workerd.flaky.capnp; check "workerd(flaky配置) 已就绪" $? "flaky worker not reachable"
SSE=$(curl -sN --max-time 10 http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"stream":true,"messages":[{"role":"user","content":"hi"}]}' 2>/dev/null || true)
echo "$SSE" | grep -q '第一帧'; check "已吐字的帧送达客户端" $? "$SSE"
check "断流后无 [DONE]（流被干净收尾）" $([ -z "$(echo "$SSE" | grep 'data: \[DONE\]')" ] && echo 0 || echo 1) "$SSE"
AFTER=$(grep -c '/flaky/chat/completions' "$MOCK_LOG" 2>/dev/null)
check "上游仅被请求一次（未发生切换）" $([ "$((AFTER-BEFORE))" = "1" ] && echo 0 || echo 1) "before=$BEFORE after=$AFTER"
stop_workerd

echo "== 10. multipart 音频上传（WASM 网关转发到上游） =="
head -c 512 /dev/urandom > /tmp/mock.wav
start_workerd workerd.config.capnp; check "workerd(默认) 重新就绪" $? "worker not reachable"
RESP=$(curl -s -w '\n%{http_code}' http://127.0.0.1:8787/v1/audio/transcriptions -F 'file=@/tmp/mock.wav;type=audio/wav' -F 'model=glm-4-voice')
CODE=$(echo "$RESP" | tail -1)
check "音频上传返回 200" $([ "$CODE" = "200" ] && echo 0 || echo 1) "$RESP"
echo "$RESP" | grep -q 'received 512 bytes'; check "上游收到完整 512 字节音频" $? "$RESP"
echo "$RESP" | grep -q 'model=glm-asr-2512'; check "旧模型名 glm-4-voice 被改写为 glm-asr-2512 透传上游" $? "$RESP"
MP=$(grep 'multipart-received' "$MOCK_LOG" | tail -1)
echo "  mock multipart: $MP"
echo "$MP" | grep -q '"fileBytes":512'; check "mock 侧解析 gateway 重建的 multipart 字节数正确" $? "$MP"
echo "$MP" | grep -q 'mock.wav'; check "filename 透传" $? "$MP"

echo "== 11. multipart 视频上传（同一路径，验证通用二进制） =="
head -c 64 /dev/urandom > /tmp/mock.mp4
RESP=$(curl -s -w '\n%{http_code}' http://127.0.0.1:8787/v1/audio/transcriptions -F 'file=@/tmp/mock.mp4;type=video/mp4')
CODE=$(echo "$RESP" | tail -1)
check "视频 bytes 上传返回 200" $([ "$CODE" = "200" ] && echo 0 || echo 1) "$RESP"
echo "$RESP" | grep -q 'received 64 bytes'; check "上游收到完整 64 字节视频" $? "$RESP"

echo "== 12. 资源管理（DO 账本：登记/轮换/探测/事件/seed）=="
stop_workerd; sleep 0.3
start_workerd workerd.config.capnp; check "workerd(资源管理) 已就绪" $? "admin worker not reachable"

# 12.1 管理页 UI
UI=$(curl -s http://127.0.0.1:8787/admin)
echo "$UI" | grep -q '资源管理'; check "管理页含标题" $? "no title"
echo "$UI" | grep -q 'id="preset-cn"'; check "管理页含国内预设目录" $? "no preset list"
echo "$UI" | grep -q 'id="channel-rows"'; check "管理页含渠道表格" $? "no channel table"
echo "$UI" | grep -q '一键载入默认'; check "管理页含 seed 按钮（国内+海外）" $? "no seed button"
# v0.7.4 防回归：侧栏版本号曾是 HTML 硬编码 v0.6.0——接口一失败就显示
# 过期版本（用户实测「前端版本变成 6.0」）。HTML 里禁止再出现任何硬编码版本。
if echo "$UI" | grep -qE 'id="side-version">v[0-9]'; then check "HTML 无硬编码版本号（防过期显示）" 1 "$(echo "$UI" | grep -oE 'id="side-version">[^<]*' | head -1)"; else check "HTML 无硬编码版本号（防过期显示）" 0 ""; fi
echo "$UI" | grep -q 'refreshVersion'; check "版本号有 /health 兜底刷新" $? "no refreshVersion fn"
# 版本单一事实源：/admin/api/channels 与 /health 的 version 必须一致
# （曾经 admin.rs 的 VERSION 常量停在 0.7.2 没升级，侧栏一直显示旧版本）。
HV=$(curl -s http://127.0.0.1:8787/health | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')
CV=$(curl -s http://127.0.0.1:8787/admin/api/channels | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version"))')
PV=$(grep -m1 '^version = ' "$ROOT/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/')
check "health/channels/Cargo.toml 三处版本一致" $([ "$HV" = "$CV" ] && [ "$CV" = "$PV" ] && echo 0 || echo 1) "health=$HV channels=$CV cargo=$PV"

# 12.2 一键载入默认（国内 3 + 海外 3，均为 disabled 待登记）
SEED=$(curl -s -X POST http://127.0.0.1:8787/admin/api/seed)
echo "$SEED" | grep -q '"added":6'; check "seed 载入 6 个默认渠道（国内 3 + 海外 3）" $? "$SEED"
echo "$SEED" | grep -q 'seed-zhipu-chat'; check "seed 含智谱主线" $? "$SEED"
echo "$SEED" | grep -q 'seed-siliconflow-chat'; check "seed 含硅基流动" $? "$SEED"
echo "$SEED" | grep -q 'seed-openrouter-chat'; check "seed 含海外 OpenRouter" $? "$SEED"
echo "$SEED" | grep -q 'seed-groq-chat'; check "seed 含海外 Groq" $? "$SEED"
echo "$SEED" | grep -q 'seed-nvidia-chat'; check "seed 含海外 NVIDIA NIM" $? "$SEED"

# 12.3 登记两个动态渠道（显式 id，便于断言）
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"dyn-a","provider":"zhipu","name":"dyn-a","base_url":"http://127.0.0.1:8788/fail","api_key":"test-key-local","models":["glm-4-flash"],"kind":"chat","tier":"T2","priority":100}' > /dev/null
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"dyn-b","provider":"siliconflow","name":"dyn-b","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","models":["glm-4-flash"],"kind":"chat","tier":"T1","priority":90}' > /dev/null
LIST=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LIST" | grep -q 'dyn-b'; check "渠道列表含已登记 dyn-b" $? "$LIST"
echo "$LIST" | grep -q 'unsupported\|Internal error'; if [ $? -eq 0 ]; then check "渠道列表无错误" 1 "$LIST"; else check "渠道列表无错误" 0; fi

# 12.4 Key 掩码：响应绝不含明文
echo "$LIST" | grep -q 'test-key-local'; if [ $? -ne 0 ]; then check "API Key 只回掩码（无明文 test-key-local）" 0; else check "API Key 只回掩码（无明文 test-key-local）" 1 "$LIST"; fi
echo "$LIST" | grep -q 'tes\*\*\*ocal'; check "掩码格式 前3后4" $? "$LIST"

# 12.5 chat 走动态渠道：dyn-a(502) → dyn-b(ok)
RESP=$(curl -sD /tmp/h4.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "动态渠道拿到上游响应" $? "$RESP"
grep -qi 'x-gateway-channel: dyn-b' /tmp/h4.txt; check "最终命中 dyn-b" $? "$(tr -d '\r' < /tmp/h4.txt | grep -i gateway)"
grep -qi 'x-gateway-attempts: dyn-a=HTTP502' /tmp/h4.txt; check "attempts 记录 dyn-a=HTTP502" $? "$(tr -d '\r' < /tmp/h4.txt | grep -i gateway)"
grep -qi 'x-gateway-provider: siliconflow' /tmp/h4.txt; check "诊断头带 provider=siliconflow" $? "$(tr -d '\r' < /tmp/h4.txt)"

# 12.6 事件账本
EV=$(curl -s http://127.0.0.1:8787/admin/api/events)
echo "$EV" | grep -q 'dyn-a=HTTP502'; check "事件账本记录降权尝试" $? "$EV"
echo "$EV" | grep -q '"result":"failed"'; check "事件含 failed 状态" $? "$EV"

# 12.7 Key 轮换：明文替换；掩码值=不改变
ROT=$(curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/dyn-b -H 'Content-Type: application/json' -d '{"api_key":"rotated-key-9999"}')
echo "$ROT" | grep -q 'rot\*\*\*9999'; check "轮换后掩码更新 rot***9999" $? "$ROT"
echo "$ROT" | grep -q 'rotated-key-9999'; if [ $? -ne 0 ]; then check "轮换响应不含明文" 0; else check "轮换响应不含明文" 1 "$ROT"; fi
curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/dyn-b -H 'Content-Type: application/json' -d '{"api_key":"rot\*\*\*9999"}' > /dev/null
LIST2=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LIST2" | grep -q 'rot\*\*\*9999'; check "掩码占位不改变 Key" $? "$LIST2"
# 换回 mock 认识的 key 供后续路由测试
curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/dyn-b -H 'Content-Type: application/json' -d '{"api_key":"test-key-local"}' > /dev/null

# 12.8 删除渠道
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/dyn-a > /dev/null
LIST3=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LIST3" | grep -q '"id":"dyn-a"'; if [ $? -ne 0 ]; then check "删除 dyn-a 生效" 0; else check "删除 dyn-a 生效" 1 "$LIST3"; fi

# 12.9 智谱 JWT 鉴权：provider=zhipu + Key 为 {id}.{secret} → 网关自动换 JWT
# （v4 端点对原始 Key 支持不稳定；JWT = HS256 + sign_type=SIGN，TTL 1h）
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"dyn-zhipu","provider":"zhipu","name":"智谱-JWT","base_url":"http://127.0.0.1:8788","api_key":"test-jwt-id.test-jwt-secret","models":["glm-4-flash"],"kind":"chat","tier":"T0","priority":400}' > /dev/null
RESP=$(curl -sD /tmp/h8.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "智谱渠道经 JWT 鉴权拿到响应" $? "$RESP"
# 诊断头只允许 ASCII：中文渠道名经 percent-encoding 后上线（浏览器按 Latin-1
# 解头，裸中文会变乱码），前端 decodeURIComponent 还原
grep -qi 'x-gateway-channel: %E6%99%BA%E8%B0%B1-JWT' /tmp/h8.txt; check "命中智谱 JWT 渠道（头内 ASCII 编码）" $? "$(tr -d '\r' < /tmp/h8.txt | grep -i gateway)"
# 往返验证：头值 decode 后必须还原为中文渠道名（模拟前端 decodeHeader）
DECODED=$(tr -d '\r' < /tmp/h8.txt | grep -i 'x-gateway-channel:' | sed 's/.*: *//' | python3 -c 'import sys,urllib.parse; print(urllib.parse.unquote(sys.stdin.read().strip()))')
check "头值解码还原为「智谱-JWT」" $([ "$DECODED" = "智谱-JWT" ] && echo 0 || echo 1) "decoded=$DECODED"
# 上游收到的 Authorization 必须是 JWT（eyJ 开头三段式）而非原始 Key
LAST_AUTH=$(tail -1 "$MOCK_LOG" | python3 -c 'import json,sys; print(json.load(sys.stdin)["authorization"])')
echo "  上游 Authorization: ${LAST_AUTH:0:40}…"
echo "$LAST_AUTH" | grep -qE '^Bearer eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$'; check "上游收到 JWT（非原始 Key）" $? "$LAST_AUTH"
# JWT payload 结构：api_key=id、exp-timestamp=3600000（1 小时）
JWT_OK=$(tail -1 "$MOCK_LOG" | python3 -c '
import json,sys,base64
auth = json.load(sys.stdin)["authorization"]
h,p,_ = auth[len("Bearer "):].split(".")
dec = lambda s: json.loads(base64.urlsafe_b64decode(s + "=" * (-len(s) % 4)))
hdr, pl = dec(h), dec(p)
ok = hdr.get("alg")=="HS256" and hdr.get("sign_type")=="SIGN" and pl.get("api_key")=="test-jwt-id" and pl.get("exp")-pl.get("timestamp")==3600000
print("ok" if ok else f"bad: {hdr} {pl}")')
check "JWT 结构合规（alg/sign_type/api_key/1h TTL）" $([ "$JWT_OK" = "ok" ] && echo 0 || echo 1) "$JWT_OK"
# mock 侧同步记录了 jwt-verified
grep -q '"op":"jwt-verified"' "$ROOT/mock.log" 2>/dev/null; check "mock 验证到合规 JWT" $? "no jwt-verified in mock.log"
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/dyn-zhipu > /dev/null

# 12.10 本地 DO 持久化（localDisk）：重启进程后渠道/Key 不丢
# —— "后端一升级 key 全没了"的修复验证（in-memory DO 重启即失）
stop_workerd
start_workerd workerd.config.capnp keep; check "workerd(持久化验证) 已就绪" $? "persist worker not reachable"
LISTP=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LISTP" | grep -q '"id":"dyn-b"'; check "重启后动态渠道仍在（DO 持久化）" $? "$LISTP"
echo "$LISTP" | grep -q 'tes\*\*\*ocal'; check "重启后 Key 掩码仍在" $? "$LISTP"
# 重启后仍可正常路由（渠道可用性不因重启丢失）
RESP=$(curl -sD /tmp/h9.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "重启后路由仍命中动态渠道" $? "$RESP"
grep -qi 'x-gateway-channel: dyn-b' /tmp/h9.txt; check "重启后渠道 = dyn-b" $? "$(tr -d '\r' < /tmp/h9.txt | grep -i gateway)"

echo "== 13. 下一个国内资源：硅基流动 + 429 冷却 =="
# 先删 dyn-b，让 dyn-429 成为唯一 chat 动态渠道（429 需要透传而非切换）
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/dyn-b > /dev/null
# 13.1 429 透传 + 冷却写入（唯一 chat 渠道提供该模型 → 429 原样透传，无回退）
RESP=$(curl -s -o /tmp/e429.json -w '%{http_code}' -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' -d '{"id":"dyn-429","provider":"zhipu","name":"dyn-429","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","models":["glm-test-429"],"kind":"chat","priority":200}')
CODE=$(curl -s -o /tmp/err2.json -w '%{http_code}' http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}')
check "动态渠道 429 原样透传" $([ "$CODE" = "429" ] && echo 0 || echo 1) "got $CODE"
grep -q '速率限制' /tmp/err2.json; check "429 上游错误体透传" $? "$(cat /tmp/err2.json)"
EV2=$(curl -s http://127.0.0.1:8787/admin/api/events)
echo "$EV2" | grep -q 'dyn-429=HTTP429'; check "事件账本记录 429 降权" $? "$EV2"
LIST4=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LIST4" | grep -q 'cooldown_until_ms":[0-9]'; check "429 后写入冷却窗口" $? "$LIST4"
# 冷却期间该渠道被跳过（其余 chat 渠道已删 → 回退静态 default，请求仍成功）
sleep 0.2
RESP=$(curl -sD /tmp/h7.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "冷却渠道被跳过，请求仍成功（回退静态）" $? "$RESP"
grep -qi 'x-gateway-channel: default' /tmp/h7.txt; check "回退静态 default 通道" $? "$(tr -d '\r' < /tmp/h7.txt | grep -i gateway)"
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/dyn-429 > /dev/null

# 13.2 硅基流动：provider=siliconflow 的 OpenAI 兼容渠道（下一国内资源）
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"sf-cn","provider":"siliconflow","name":"硅基流动-Qwen","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","models":["Qwen/Qwen2.5-7B-Instruct"],"kind":"chat","tier":"T2","priority":300}' > /dev/null
RESP=$(curl -sD /tmp/h5.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"Qwen/Qwen2.5-7B-Instruct","messages":[{"role":"user","content":"hi"}]}')
echo "$RESP" | grep -q 'Qwen/Qwen2.5-7B-Instruct'; check "硅基流动渠道透传响应" $? "$RESP"
grep -qi 'x-gateway-channel: %E7%A1%85%E5%9F%BA%E6%B5%81%E5%8A%A8-Qwen' /tmp/h5.txt; check "命中硅基流动渠道（头内 ASCII 编码）" $? "$(tr -d '\r' < /tmp/h5.txt | grep -i gateway)"

# 13.3 静态回退：删空动态 chat 渠道后回落 ZHIPU_CHANNELS 行为
for id in sf-cn dyn-b; do curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/$id > /dev/null; done
RESP=$(curl -sD /tmp/h6.txt http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"你好"}]}')
echo "$RESP" | grep -q 'glm-4-flash'; check "动态清空后回退静态默认通道" $? "$RESP"
grep -qi 'x-gateway-channel: default' /tmp/h6.txt; check "静态回退通道=default" $? "$(tr -d '\r' < /tmp/h6.txt | grep -i gateway)"
stop_workerd

echo "== 14. 模型选择引擎（四级能力 / 模型级冷却 / TTS / 跨渠道 Key 复用）=="
stop_workerd; sleep 0.3
start_workerd workerd.config.capnp; check "workerd(模型引擎) 已就绪" $? "model-engine worker not reachable"

echo "== 14.1 视觉路由：image_url 命中 vision 模型，纯文本命中 text 模型 =="
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-vision","provider":"zhipu","name":"视觉测试","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":200,"models_meta":[{"name":"glm-4v-flash","kind":"vision","priority":80},{"name":"glm-4-flash","kind":"text","priority":50}]}' > /dev/null
# 带图片的请求：want_kind=vision → 应选 glm-4v-flash
VIS=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":[{"type":"text","text":"图里是什么"},{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}]}]}')
echo "$VIS" | grep -q 'glm-4v-flash'; check "image_url 请求路由到 vision 模型 glm-4v-flash" $? "$VIS"
# 纯文本请求：want_kind=text → 应选 glm-4-flash
TXT=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"你好"}]}')
echo "$TXT" | grep -q 'glm-4-flash'; check "纯文本请求路由到 text 模型 glm-4-flash" $? "$TXT"

echo "== 14.2 跨渠道 Key 复用：音频渠道无 Key，借同 provider 聊天渠道 Key =="
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-chat","provider":"zhipu","name":"智谱主线","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":100,"models_meta":[{"name":"glm-4-flash","kind":"text","priority":50}]}' > /dev/null
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-audio","provider":"zhipu","name":"智谱音频","base_url":"http://127.0.0.1:8788","api_key":"","kind":"audio","tier":"T2","priority":200,"models_meta":[{"name":"glm-4-voice","kind":"stt","priority":50}]}' > /dev/null
head -c 512 /dev/urandom > /tmp/stt.wav
RESP=$(curl -s -w '\n%{http_code}' http://127.0.0.1:8787/v1/audio/transcriptions -F 'file=@/tmp/stt.wav;type=audio/wav' -F 'model=glm-4-voice')
CODE=$(echo "$RESP" | tail -1)
check "音频渠道(无Key)转录返回 200（借主线Key）" $([ "$CODE" = "200" ] && echo 0 || echo 1) "$RESP"
echo "$RESP" | grep -q 'glm-asr-2512'; check "转写命中 ASR 模型 glm-asr-2512" $? "$RESP"
# 渠道登记的是旧名 glm-4-voice——网关应自动改写为智谱当前 ASR 模型再发上游
LAST_MODEL=$(grep '"modelField"' "$MOCK_LOG" | tail -1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["modelField"])')
check "旧模型名 glm-4-voice 已改写为 glm-asr-2512 发给上游" $([ "$LAST_MODEL" = "glm-asr-2512" ] && echo 0 || echo 1) "upstream model=$LAST_MODEL"
LAST_AUTH=$(grep '"authorization"' "$MOCK_LOG" | tail -1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["authorization"])')
check "跨渠道借用生效：上游收到主线 Key" $(echo "$LAST_AUTH" | grep -q 'Bearer test-key-local' && echo 0 || echo 1) "$LAST_AUTH"

echo "== 14.3 TTS 播音：音频渠道只有 stt 模型时也能播音（能力回落 + 跨渠道借 Key）=="
# 回归场景（用户实测"播音无模型"）：eng-audio 只有 stt 模型且无 Key，
# eng-chat 有 Key 但只有 text 模型。修复前 tts 候选池只取 chat 渠道 → 503 无播音模型。
# 修复后：候选池 = chat+audio，tts 回落到音频渠道的 stt 模型，并借主线 Key。
CT=$(curl -s -o /tmp/tts.bin -w '%{http_code} %{content_type}' http://127.0.0.1:8787/v1/audio/speech -H 'Content-Type: application/json' -d '{"input":"你好世界，这是播音测试"}')
CODE=$(echo "$CT" | cut -d' ' -f1); MT=$(echo "$CT" | cut -d' ' -f2)
check "TTS 返回 200（stt-only 音频渠道回落）" $([ "$CODE" = "200" ] && echo 0 || echo 1) "$CT"
check "TTS 内容类型 audio/wav（mock 回合法 WAV，智谱默认格式）" $([ "$MT" = "audio/wav" ] && echo 0 || echo 1) "$CT"
grep -q '"op":"tts-received"' "$MOCK_LOG" 2>/dev/null; check "mock 收到 TTS 请求" $? "no tts-received in mock-received.log"
# v0.7.3：智谱系渠道一律收敛到合成端点唯一模型 glm-tts + 默认音色 tongtong。
# 老账本登记的 glm-4-voice（含 stt 回落）会被请求前改写——glm-4-voice 是
# 端到端语音模型，原样打到 /audio/speech 会被上游按 chat 校验报 1214。
LAST_TTS=$(grep '"op":"tts-received"' "$MOCK_LOG" | tail -1)
TTS_MODEL=$(echo "$LAST_TTS" | python3 -c 'import json,sys; print(json.load(sys.stdin)["model"])')
TTS_VOICE=$(echo "$LAST_TTS" | python3 -c 'import json,sys; print(json.load(sys.stdin)["voice"])')
check "TTS 模型收敛为 glm-tts（防 1214 messages 参数非法）" $([ "$TTS_MODEL" = "glm-tts" ] && echo 0 || echo 1) "upstream model=$TTS_MODEL"
check "TTS 默认音色 tongtong" $([ "$TTS_VOICE" = "tongtong" ] && echo 0 || echo 1) "upstream voice=$TTS_VOICE"

echo "== 14.4 模型级冷却 + 自动换模型（隔离环境，仅 eng-cool 避免被其他文本模型回退）=="
stop_workerd; sleep 0.3
start_workerd workerd.config.capnp; check "workerd(冷却隔离) 已就绪" $? "cooldown isolation worker not reachable"
# eng-cool 只提供 glm-test-429（无其他模型可回退）→ 触发 429 并写入冷却
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-cool","provider":"zhipu","name":"冷却测试A","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":200,"models_meta":[{"name":"glm-test-429","kind":"text","priority":90}]}' > /dev/null
# 第一次：命中 glm-test-429 → 上游 429（无回退，原样透传）
CODE=$(curl -s -o /tmp/c1.json -w '%{http_code}' http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}')
check "指定模型 glm-test-429 触发上游 429（原样透传）" $([ "$CODE" = "429" ] && echo 0 || echo 1) "got $CODE"
LISTX=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$LISTX" | grep -q 'cooldown_until_ms":[0-9]'; check "429 后模型级冷却窗口已写入" $? "$LISTX"
# 注册另一渠道的同类模型，再请求被冷却的 glm-test-429 → 引擎跳过冷却模型，自动换 glm-4.7-flash
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-other","provider":"zhipu","name":"冷却测试B","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":100,"models_meta":[{"name":"glm-4.7-flash","kind":"text","priority":50}]}' > /dev/null
RESP=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}')
echo "$RESP" | grep -q 'glm-4.7-flash'; check "冷却模型被跳过，自动换用另一模型 glm-4.7-flash" $? "$RESP"
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/eng-cool > /dev/null
curl -s -X DELETE http://127.0.0.1:8787/admin/api/channels/eng-other > /dev/null

echo "== 15. 语音全链路回归（v0.8.0：状态机 / 单消费者 / TTS-STT 双兜底）=="
# v0.7.5 修「第二次就挂」（麦克风竞争/播报叠加/AudioContext 累积）后，v0.8.0
# 把语音交互重构为三层能力：MicHub（麦克风单所有者）+ VoiceInput（一次输入
# 只用一个消费者）+ CallFsm（双向状态机，barge-in 打断是一等公民），TTS/STT
# 均为「网关 → 自定义端点 → 浏览器」递降链。以下断言锁定新架构不回归。
UI="$ROOT/src/admin_ui.html"
# ① 麦克风单所有者：MicHub 抢占前先彻底释放旧所有者的流（iOS 双流直接拒）
grep -q 'const MicHub' "$UI" && grep -q 'this.release(); // 抢占' "$UI"; check "MicHub 单所有者 + 抢占前彻底释放旧流" $? "no MicHub preempt release"
# ② VoiceInput 单消费者：识别可用走浏览器 SpeechRecognition 并先 return，
#    MediaRecorder 只在识别不可用分支出现——两个消费者绝不并发
python3 -c "
import re, sys
ui = open('$UI').read()
m = re.search(r'const VoiceInput = \{\s*cur:.*?\n\};', ui, re.S)
b = re.sub(r'//[^\n]*', '', m.group(0))  # 去行注释
br = b.find('return \"browser\"'); mr = b.find('new MediaRecorder')
sys.exit(0 if (br != -1 and mr != -1 and br < mr) else 1)
"; check "VoiceInput 单消费者（浏览器识别先 return，MediaRecorder 仅兜底分支）" $? "dual mic consumer"
# ③ CallFsm 六态 + barge-in 一等公民：speaking/thinking 态 press 合法
grep -q 'closed:.*{ open: \"idle\" }' "$UI"; check "CallFsm 状态机事件表存在（closed→idle）" $? "no fsm table"
grep -q 'speaking:.*{ press: \"listening\"' "$UI"; check "barge-in：播报中 press→listening（打断是一等公民）" $? "no barge-in transition"
grep -q 'if (!to) return false;' "$UI"; check "CallFsm 拒绝非法迁移（忙态按钮点不动）" $? "illegal transitions accepted"
# ④ 早锁：录音/识别启动即占锁，全程禁第二轮（同 v0.7.5 修复语义）
grep -A8 'mode = await VoiceInput.start(\"call\"' "$UI" | grep -q 'CallFsm.fire(\"press\")'; check "呼叫识别启动即迁移 listening 持锁" $? "no early lock (call)"
grep -A6 'mode = await VoiceInput.start(\"input\"' "$UI" | grep -q 'setVoiceBusy(true)'; check "输入框识别启动即持锁 voiceBusy" $? "no early lock (input)"
# ⑤ finishVoice/finishCallVoice 松锁在 finally（每条出口都不锁死按钮）
python3 -c "
import re, sys
ui = open('$UI').read()
m = re.search(r'async function finishVoice\(\) \{(.*?)\n\}', ui, re.S)
b = re.sub(r'//[^\n]*', '', m.group(1))
unlock = b.find('setVoiceBusy(false)'); aw = b.find('await ')
sys.exit(0 if (unlock != -1 and unlock > aw) else 1)
"; check "finishVoice 所有 await 出口后统一松锁（finally）" $? "unlock before await"
# ⑥ TTS 三层递降链：网关 → 自定义端点 → 浏览器兜底
grep -q 'mode === \"custom\" ? await customTtsBlob(text) : await ttsBlob(text)' "$UI"; check "TTS 递降链：网关/自定义端点优先" $? "no tts chain"
grep -q 'if (!BrowserTts.ready) throw e;' "$UI"; check "TTS 浏览器兜底可用性检查（不可用则上抛）" $? "no browser fallback guard"
grep -q 'async function customTtsBlob' "$UI" && grep -q 'response_format: \"wav\"' "$UI"; check "自定义 TTS 端点 OpenAI 兼容（客户自有资源替换点）" $? "no custom endpoint"
grep -q 'if (mode === \"gateway\") throw e;' "$UI"; check "仅网关模式不降级（错误明示不静默）" $? "gateway mode degrades"
# ⑦ 浏览器 TTS（Speakit-JS 融合体）：长文本切片 + 主动停止信号
grep -q 'parts = String(text || \"\").split' "$UI" && grep -q 'i += 120' "$UI"; check "浏览器 TTS 长文本按句切片 + 120 字硬切（防 Chrome 静默截断）" $? "no chunking"
grep -q 'reject(new Error(\"stopped\"))' "$UI" && grep -q '=== \"stopped\") return;' "$UI"; check "TTS 停止语义：cancel→stopped 信号被静默消费（不误报错）" $? "no stopped signal"
grep -q 'onvoiceschanged' "$UI" && grep -q 'Date.now() - t0 > 1500' "$UI"; check "浏览器音色列表加载有 onvoiceschanged + 1.5s 轮询兜底" $? "no voice poll"
# ⑧ 浏览器 STT：continuous+interimResults 实时文字流
grep -q 'rec.continuous = true; rec.interimResults = true;' "$UI"; check "浏览器 STT continuous+interimResults（真实时文字流）" $? "no realtime stt"
# ⑨ 防重入 + 60s 超时兜底（两个入口都有）
grep -q 'callFinishing' "$UI" && grep -q 'voiceFinishing' "$UI"; check "finish 函数防重入（60s 超时兜底与 pointerup 不双入）" $? "no reentry guard"
grep -q 'if (CallFsm.s === \"listening\") finishCallVoice(); }, 60000)' "$UI"; check "呼叫 60s 超时自动收尾（pointerup 丢失不锁死）" $? "no timeout guard (call)"
grep -q 'if (voiceBusy) finishVoice(); }, 60000)' "$UI"; check "输入框 60s 超时自动收尾（pointerup 丢失不锁死）" $? "no timeout guard (input)"
# ⑩ 入口清理：异常路径先 abort 旧输入会话 + 释放麦克风
grep -q 'VoiceInput.abort(\"input\")' "$UI" && grep -q 'VoiceInput.abort(\"call\")' "$UI"; check "入口先 abort 残留识别会话（防旧消费者占麦）" $? "no input cleanup"
grep -q 'MicHub.release(\"call\")' "$UI"; check "打断时显式释放麦克风（下轮重新 acquire）" $? "no mic release on barge-in"
# ⑪ 播报排空 + 挂断唤醒 + 流式中断（v0.7.5 修复不回退）
grep -q 'await ttsQueueDrained();' "$UI"; check "callAskStreaming 等播报队列排空（不早释锁）" $? "no drain await"
grep -q 'function ttsQueueDrained' "$UI" && grep -q 'flushTtsWaiters()' "$UI"; check "drain 等待者有 flush 唤醒机制" $? "no flush fn"
grep -A6 'function ttsStopAll' "$UI" | grep -q 'ttsCurResolve'; check "ttsStopAll 唤醒被 pause 卡住的播放（pumpTts 不会永挂）" $? "no manual resolve"
grep -q 'chatAbort = new AbortController' "$UI" && grep -q 'chatAbort.abort()' "$UI"; check "挂断中断进行中的流式请求（不必等上游把话说完）" $? "no chat abort"
# ⑫ AudioContext 单例（v0.7.5 修复不回退）
if grep -q '_wavCtx' "$UI" && ! grep -q 'ctx\.close()' "$UI"; then check "AudioContext 全局单例复用（iOS 并发上限不再触顶）" 0; else check "AudioContext 全局单例复用（iOS 并发上限不再触顶）" 1 "per-call context still present"; fi
# ⑬ 语音设置对话框：策略可配 + 客户端点 + 配置持久化
grep -q 'id=\"voice-dlg\"' "$UI" && grep -q 'function openVoiceCfg' "$UI"; check "语音设置对话框（TTS/STT 模式可配）" $? "no voice dialog"
grep -q 'gw_voice_cfg' "$UI" && grep -q 'VoiceCfg.save' "$UI"; check "语音配置 localStorage 持久化" $? "no cfg persistence"
grep -q 'speakMsg' "$UI" && grep -q 'await ttsPlaySentence' "$UI"; check "消息播音统一走 ttsPlaySentence（同一条递降链）" $? "speakMsg bypasses chain"

echo "== 16. 聊天记录 IndexedDB 回归（v0.9.0：可删可查 / 联动视图 / 流式持久化 / 新会话）=="
# 用户诉求「聊天记录进 IndexedDB、可删可查、可联动视图、会话流要适应、
# 新会话目前没做好」。localStorage 三硬伤：5MB 配额（附件 dataUrl 只能
# 剥离/60 条封顶）、同步序列化卡主线程、整坨读写做不了消息级增删与搜索。
UI="$ROOT/src/admin_ui.html"
# ① IDB 结构：convos/msgs 双 store + 消息复合主键（重发截断同位置覆盖不重复）
grep -q 'createObjectStore("convos", { keyPath: "id" })' "$UI"; check "IDB convos store（keyPath id）" $? "no convos store"
grep -q 'createObjectStore("msgs", { keyPath: \["convoId", "idx"\] })' "$UI"; check "IDB msgs store 复合主键（convoId, idx）" $? "no compound key"
grep -q 'createIndex("by_convo", "convoId")' "$UI"; check "msgs 按会话索引（单会话加载/删除走索引）" $? "no by_convo index"
# ② 旧数据迁移：localStorage gw_convos_v1 → IDB，迁完删旧 key 留标记（防双数据源）
grep -q 'localStorage.getItem("gw_convos_v1")' "$UI"; check "启动时读取 localStorage 旧会话数据" $? "no migration read"
grep -q 'localStorage.setItem("gw_convos_v1_migrated", "1")' "$UI" && grep -q 'localStorage.removeItem("gw_convos_v1")' "$UI"; check "迁移后删旧 key 留标记（无双写）" $? "old key not cleaned"
if grep -q 'localStorage.setItem("gw_convos_v1"' "$UI"; then check "localStorage 不再作为会话存储（仅迁移读取）" 1 "still writing gw_convos_v1"; else check "localStorage 不再作为会话存储（仅迁移读取）" 0; fi
# ③ 流式持久化：SSE 循环内节流落库 + 终态立即写 + 防抖窗口有 pagehide/visibility 兜底
grep -q 'markMsgDirty(convoId, asstIdx); // 流式期间节流落库' "$UI"; check "流式期间消息节流落库（刷新不丢已生成部分）" $? "no streaming persist"
grep -q 'IDB.putMsg(convoId, asstIdx, asst);' "$UI"; check "流式终态立即落库（不等防抖）" $? "no final write"
grep -q 'window.addEventListener("pagehide", flushPending)' "$UI" && grep -q 'visibilitychange' "$UI"; check "挂起写有 pagehide + visibilitychange 双兜底冲刷" $? "no flush on hide"
# ④ 新会话修复：开新对话先收尾旧流（streamGen 抢占），否则发送键还是旧流停止键
grep -q 'streamGen++' "$UI"; check "newConvo 抢占旧流代号（新对话能正常发送）" $? "no stream preempt"
grep -q 'if (myGen === streamGen) {' "$UI"; check "旧流 finally 按代号决定是否掌管 UI（不覆盖新会话状态）" $? "no gen guard"
# ⑤ 可删：会话级 / 消息级 / 全部清空
grep -q 'await IDB.deleteConvo(id)' "$UI"; check "删除会话同步删库（convos + 全部 msgs）" $? "convo not deleted"
grep -q 'async function menuDelete' "$UI" && grep -q 'IDB.deleteMsg(c.id, c.msgs.length)' "$UI"; check "消息级删除 + idx 前移后尾部游离记录精确清理" $? "msg delete broken"
grep -q 'async function clearAllConvos' "$UI" && grep -q 'IDB.clearAll()' "$UI"; check "一键清空全部会话（confirm 防误触）" $? "no clear all"
# ⑥ 可查：侧栏搜索框，标题即过滤 + 消息内容跨会话命中
grep -q 'id="convo-search"' "$UI" && grep -q 'async function runSearch' "$UI"; check "侧栏会话搜索（标题 / 内容）" $? "no search"
grep -q 'searchHits\[m.convoId\]' "$UI"; check "内容命中按会话聚合（侧栏显示片段）" $? "no hit grouping"
# ⑦ 可联动视图：搜索/删除/打开全部走同一 render 链路；附件随消息持久化
grep -q 'function renderConvos()' "$UI" && grep -q 'searchHits\[c.id\]' "$UI"; check "搜索态侧栏过滤（命中高亮 + 空态文案）" $? "no filtered render"
grep -q 'if (a.kind === "image" && a.dataUrl) base.dataUrl = a.dataUrl' "$UI"; check "图片 dataUrl 持久化（刷新后缩略图不丢）" $? "no dataUrl persist"
grep -q 'if ((a.kind === "audio" || a.kind === "video") && a.file) base.file = a.file' "$UI"; check "音频/视频 Blob 持久化（刷新后容器播放仍可用）" $? "no media persist"
# ⑧ 流式中断恢复：上次异常退出的流式消息重开标记中断，不再带光标
grep -q 'm.streaming = false; m.error = m.content ? "（生成中断）" : "（未完成）"' "$UI"; check "异常退出的流式消息重开标记中断" $? "no interrupt recovery"
# ⑨ 重发截断同步删库（否则重开后旧消息复活）
grep -q 'removed.forEach((rm, k) => IDB.deleteMsg(c.id, j + k))' "$UI"; check "重新生成截断的消息同步删库" $? "truncated msgs not deleted"
# ⑩ toast 不得拦截点击：底部输入区在 toast 悬浮的 4.6s 内必须可点
#    （实测缺陷：.toast 单独开 pointer-events:auto，清空会话后马上点发送没反应）
if grep -A1 '#toast { position: fixed' "$UI" | grep -q 'pointer-events: none' && ! grep '\.toast {' "$UI" | grep -q 'pointer-events: auto'; then check "toast 纯通知不拦截点击（底部区域随时可点）" 0; else check "toast 纯通知不拦截点击（底部区域随时可点）" 1 "toast still intercepts clicks"; fi

echo "== 17. 分享 /s 二级子页 + 微信渠道（v0.10.0：应用分发裂变）=="
# 用户诉求「分享微信功能线是应用分发裂变很重要途径，做域名二级子页实现」。
# 微信内分享卡片只能带 URL（不能带正文），故架构：正文压缩进 URL hash，
# 服务端零存储，二级子域 s.<域名> 独立落地页承接微信浏览器打开。
UI="$ROOT/src/admin_ui.html"
SH="$ROOT/src/share_ui.html"
# ① 路由：GET /s 返回独立分享页（服务端零存储，只发静态 HTML）
grep -q 'path == "/s" || path == "/s/"' "$ROOT/src/lib.rs" && grep -q 'include_str!("share_ui.html")' "$ROOT/src/lib.rs"; check "GET /s 路由 include 分享页（零存储）" $? "no /s route"
grep -q 'GET /s (share page)' "$ROOT/src/lib.rs"; check "404 文案含 /s（可发现性）" $? "404 text missing /s"
# ② 分享页本体：解码 + 渲染 + 微信引导 + 二维码
grep -q 'function decodePayload' "$SH"; check "分享页 hash 解码（z. 压缩 / p. 明文）" $? "no decodePayload"
grep -q 'DecompressionStream("deflate-raw")' "$SH"; check "压缩 payload 用 deflate-raw 解压" $? "no inflate"
grep -q 'id="wx-steps"' "$SH" && grep -q 'MicroMessenger' "$SH"; check "微信内 UA 检测 + 右上角转发引导" $? "no wx steps"
grep -q 'QRLIB(0, "M")' "$SH"; check "分享页注入 QR 库（M 级纠错，短链场景）" $? "no QRLIB in share page"
grep -q 'atob(raw.slice(2).replace(/-/g, "+").replace(/_/g, "/"))' "$SH"; check "p. 明文路径 base64url 字母表归一化（v0.10.0 实测 bug：中文必含 -/_）" $? "p. path not normalized"
# ③ 管理页：生成分享链接（压缩上限保护 + 二级子域推断 + 降级）
grep -q 'async function buildShareUrl' "$UI" && grep -q 'const SHARE_MAX = 6000' "$UI"; check "buildShareUrl 有压缩上限保护（URL 过长兜底）" $? "no SHARE_MAX"
grep -q 'function shareOrigin' "$UI" && grep -q 'location.hostname' "$UI" && grep -q '"//s." + root' "$UI"; check "shareOrigin 二级子域推断（www.x.com → s.x.com）" $? "no subdomain inference"
grep -q 'CompressionStream("deflate-raw")' "$UI"; check "链接生成走 deflate-raw 压缩（体积优先）" $? "no deflate in admin"
grep -q 'id="share-dlg"' "$UI" && grep -q 'QRLIB(0, "M")' "$UI"; check "分享对话框 + 管理页 QR 生成" $? "no share dialog"
grep -q 'function fallbackCopyText' "$UI" && grep -q 'navigator.share' "$UI"; check "复制失败降级 execCommand + 系统分享 API" $? "no copy fallback"
# ④ 真实 HTTP：/s 与 /s/ 均返回分享页（零存储静态 HTML），带 hash 不 404
SC=$(curl -s -o /tmp/share.html -w '%{http_code}' http://127.0.0.1:8787/s)
[ "$SC" = "200" ]; check "GET /s 返回 200" $? "code=$SC"
grep -q 'decodePayload' /tmp/share.html && grep -q 'wx-steps' /tmp/share.html; check "/s 响应体是分享页（含解码器 + 微信引导）" $? "not share page"
SC2=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8787/s/)
[ "$SC2" = "200" ]; check "GET /s/ 尾斜杠同效" $? "code=$SC2"
SC3=$(curl -s -o /dev/null -w '%{http_code}' 'http://127.0.0.1:8787/s#z.eJyrVspLLtJSSkWSBgYKtKRAK5YAIA')
[ "$SC3" = "200" ]; check "带 payload hash 打开不 404（hash 服务端不可见，前端解析）" $? "code=$SC3"
CT=$(curl -s -o /dev/null -D - http://127.0.0.1:8787/s | tr -d '\r' | grep -i '^content-type' || true)
echo "$CT" | grep -qi 'text/html'; check "/s Content-Type 为 text/html" $? "$CT"
# ⑤ 纯函数真实往返（node 跑 admin_ui.html 里提取的 b64url/deflate，非 grep 摆设）
cat > "$ROOT/.share-extract.mjs" <<'EOF'
// 从 admin_ui.html 提取分享纯函数落盘成模块，import 后真实跑往返（不用 eval：
// eval + 模板字符串会吃掉正则里的反斜杠，且函数声明提升语义也不同）
import { readFileSync, writeFileSync } from "node:fs";
function extract(src, name) {
  let i = src.indexOf("function " + name + "(");
  if (i < 0) throw new Error("missing " + name);
  if (src.slice(i - 6, i) === "async ") i -= 6; // 保住 async 关键字，否则 await 语法错误
  let d = 0;
  for (let k = src.indexOf("{", i); k < src.length; k++) {
    if (src[k] === "{") d++;
    else if (src[k] === "}" && --d === 0) return src.slice(i, k + 1);
  }
  throw new Error("unbalanced " + name);
}
// 管理页编码器 + 分享页解码器一起提取，验证两侧互逆（p. 字母表 bug 的回归防线）
const specs = [
  [process.argv[2], ["b64urlFromBytes", "b64urlUnicode", "deflateB64url"]],
  [process.argv[4], ["b64ToBytes", "inflate", "decodePayload"]],
];
let out = "";
for (const [file, names] of specs) {
  const src = readFileSync(file, "utf8");
  out += names.map((n) => extract(src, n)).join("\n\n") + "\n\n";
}
out += "export { b64urlFromBytes, b64urlUnicode, deflateB64url, b64ToBytes, inflate, decodePayload };\n";
writeFileSync(process.argv[3], out);
EOF
node "$ROOT/.share-extract.mjs" "$UI" "$ROOT/.share-fns.mjs" "$SH" || true
cat > "$ROOT/.share-roundtrip.mjs" <<'EOF'
import { b64urlUnicode, deflateB64url, decodePayload } from "./.share-fns.mjs";
const assert = (c, m) => { if (!c) { console.error("FAIL: " + m); process.exit(1); } };
// Unicode 编解码往返（中文/emoji/换行/特殊字符）
for (const s of ["你好世界", "hello world", "emoji 😀 换行\n制表\t", "混合 ABC123!@#"]) {
  const b64 = b64urlUnicode(s);
  assert(!/[+/=]/.test(b64), "b64url 字母表无 +/=");
  assert(decodeURIComponent(escape(atob(b64.replace(/-/g, "+").replace(/_/g, "/")))) === s, "b64urlUnicode 往返: " + s);
}
// 跨文件互逆：管理页编码器 → 分享页 decodePayload 解码（z. 压缩 + p. 明文两条路）
const long = "这是一段足够长的中文回答内容，含 **加粗** 与 `行内代码`。".repeat(50);
const json = JSON.stringify({ v: 1, t: "标题", c: long, m: "glm-4-flash", ts: 1700000000000 });
const z = await deflateB64url(json);
assert(/^[A-Za-z0-9_-]+$/.test(z), "压缩输出为合法 base64url");
assert(await decodePayload("z." + z) === json, "z. 压缩 payload 管理页→分享页互逆");
// p. 明文路径：中文内容几乎必含 -/_，atob 前必须做字母表归一化（v0.10.0 实测 bug 回归）
const p64 = b64urlUnicode(json);
assert(/[-_]/.test(p64), "测试样本需含 base64url 特有字符（否则回归无意义）");
assert(await decodePayload("p." + p64) === json, "p. 明文 payload 管理页→分享页互逆");
console.log("share-roundtrip OK");
EOF
node "$ROOT/.share-roundtrip.mjs" >/dev/null 2>&1; check "b64url/deflate 编解码真实往返（中英文/emoji/长文）" $? "roundtrip broken"
rm -f "$ROOT/.share-extract.mjs" "$ROOT/.share-fns.mjs" "$ROOT/.share-roundtrip.mjs"

echo "== 18. v0.11.0：呼叫交流模式 / 分享直通系统面板 / 多媒体容器播放 =="
UI="$ROOT/src/admin_ui.html"
# ① 交流模式：一问一答（half，AI 说完才能讲）/ 同时问答（duplex，可抢话打断）
grep -q 'callMode: "duplex"' "$UI"; check "VoiceCfg.def 定义 callMode（默认 duplex 同时问答）" $? "no callMode"
grep -q 'id="btn-call-mode"' "$UI" && grep -q 'function toggleCallMode' "$UI"; check "呼叫页模式切换按钮 + toggleCallMode（持久化 VoiceCfg.save）" $? "no toggle"
grep -q '一问一答模式：等 AI 说完再说话' "$UI"; check "一问一答禁抢话拦截（thinking/speaking 期间 press 被拦 + toast）" $? "no half guard"
grep -q 'function callStatusText' "$UI"; check "状态文案按模式分化（half 提示说完才能讲）" $? "no callStatusText"
# ② 播放修复：手势预热媒体栈（iOS Safari 需手势内 resume AudioContext/唤醒 speechSynthesis）
grep -q 'function unlockMedia' "$UI" && grep -q 'addEventListener("pointerdown", unlockMedia' "$UI"; check "首次 pointerdown 手势预热 AudioContext/静音 WAV/speechSynthesis" $? "no unlockMedia"
grep -q 'ttsDegradeNotified' "$UI"; check "TTS 降级浏览器语音有一次性可见提示（防刷屏）" $? "no degrade toast"
# ③ 分享直通：优先 navigator.share 系统面板不弹二级对话框；AbortError 安静退出
NS=$(grep -n 'await navigator.share' "$UI" | head -1 | cut -d: -f1)
DL=$(grep -n 'shareDlg.showModal' "$UI" | head -1 | cut -d: -f1)
[ -n "$NS" ] && [ -n "$DL" ] && [ "$NS" -lt "$DL" ]; check "navigator.share 先于 share-dlg.showModal（直通不弹窗）" $? "share order wrong ns=$NS dl=$DL"
grep -q 'AbortError' "$UI"; check "用户取消系统分享安静退出（不落级对话框）" $? "no AbortError"
grep -q 'PLATFORM_SUFFIXES' "$UI" && grep -q '"workers.dev"' "$UI"; check "平台域识别（workers.dev/vercel.app 等回落同源，修 s. 子域 404）" $? "no platform suffixes"
# ④ 多媒体：图片缩略图 + 音视频容器播放（Blob 持久化 IDB + 视频首帧 poster）
grep -q 'img class="att-thumb"' "$UI"; check "图片附件缩略图渲染（att-thumb）" $? "no thumb"
grep -q 'startsWith("video/") ? "video"' "$UI"; check "附件 kind 识别 video" $? "no video kind"
grep -q 'a.kind === "audio" || a.kind === "video") && a.file' "$UI"; check "slimAtt 音频/视频存 Blob（结构化克隆进 IDB）" $? "no media slim"
grep -q 'function attMediaUrl' "$UI" && grep -q 'new WeakMap()' "$UI"; check "blob: URL 按附件对象缓存（attMediaUrl + WeakMap）" $? "no attMediaUrl"
grep -q 'audio class="att-audio" controls preload="metadata"' "$UI"; check "音频消息 <audio controls> 容器播放" $? "no audio container"
grep -q 'video class="att-video" controls preload="metadata" playsinline' "$UI"; check "视频消息 <video controls playsinline> 容器播放" $? "no video container"
grep -q 'function videoPoster' "$UI" && grep -qF 'poster="${a.poster}"' "$UI"; check "视频首帧抽帧缩略图（videoPoster + poster 属性）" $? "no video poster"
grep -q '🎬' "$UI"; check "输入区视频 chip 🎬 图标（无首帧时）" $? "no video icon"
grep -q '.att-media {' "$UI" && grep -q '.att-video {' "$UI"; check "多媒体容器 CSS（att-media/att-audio/att-video）" $? "no media css"
# ⑤ shareOrigin 真实单测（node 跑提取函数 + location stub，非 grep 摆设）：
#    截图中 s.quakestone987.workers.dev 404 的回归防线
cat > "$ROOT/.origin-extract.mjs" <<'EOF'
// 提取 PLATFORM_SUFFIXES 常量 + shareOrigin 函数；location. 替换为
// globalThis.__loc. 便于测试桩切换（浏览器里 location 不可 stub）
import { readFileSync, writeFileSync } from "node:fs";
function extractFn(src, name) {
  let i = src.indexOf("function " + name + "(");
  if (i < 0) throw new Error("missing " + name);
  let d = 0;
  for (let k = src.indexOf("{", i); k < src.length; k++) {
    if (src[k] === "{") d++;
    else if (src[k] === "}" && --d === 0) return src.slice(i, k + 1);
  }
  throw new Error("unbalanced " + name);
}
const src = readFileSync(process.argv[2], "utf8");
const ci = src.indexOf("const PLATFORM_SUFFIXES = [");
if (ci < 0) throw new Error("missing PLATFORM_SUFFIXES");
const consts = src.slice(ci, src.indexOf("];", ci) + 2);
const fn = extractFn(src, "shareOrigin").replace(/location\./g, "globalThis.__loc.");
writeFileSync(process.argv[3], consts + "\n" + fn + "\nexport { shareOrigin };\n");
EOF
node "$ROOT/.origin-extract.mjs" "$UI" "$ROOT/.origin-fns.mjs" || true
cat > "$ROOT/.origin-test.mjs" <<'EOF'
import { shareOrigin } from "./.origin-fns.mjs";
const assert = (c, m) => { if (!c) { console.error("FAIL: " + m); process.exit(1); } };
const stub = (h, p) => { globalThis.__loc = { hostname: h, origin: p + "://" + h, protocol: p + ":" }; };
// 平台域：workers.dev/vercel.app 用户无权配 s. 子域 → 必须回落同源（截图 404 回归）
stub("s.quakestone987.workers.dev", "https");
assert(shareOrigin() === "https://s.quakestone987.workers.dev", "workers.dev s. 子域回落同源");
stub("quakestone987.workers.dev", "https");
assert(shareOrigin() === "https://quakestone987.workers.dev", "workers.dev 根域回落同源");
stub("myapp.vercel.app", "https");
assert(shareOrigin() === "https://myapp.vercel.app", "vercel.app 平台域回落同源");
// 自有域：三级标签推断 s.<root>
stub("admin.example.com", "https");
assert(shareOrigin() === "https://s.example.com", "admin.example.com → s.example.com");
// 本地/IP：回落同源
stub("localhost", "http");
assert(shareOrigin() === "http://localhost", "localhost 回落同源");
stub("127.0.0.1", "http");
assert(shareOrigin() === "http://127.0.0.1", "IP 回落同源");
console.log("origin-test OK (6 cases)");
EOF
node "$ROOT/.origin-test.mjs" >/dev/null 2>&1; check "shareOrigin 平台域回落/子域推断真实单测（node + location stub）" $? "origin logic broken"
rm -f "$ROOT/.origin-extract.mjs" "$ROOT/.origin-fns.mjs" "$ROOT/.origin-test.mjs"

echo "== 19. v0.12.0：海外模型融合（一键载入国内+海外 / 兜底路由 / 海外视觉承接）=="
stop_workerd
start_workerd workerd.config.capnp; check "workerd(海外融合) 已就绪" $? "worker not reachable"
# 19.1 seed 现在载入 6 条（国内 3 + 海外 3），全部 disabled 待登记
SEED=$(curl -s -X POST http://127.0.0.1:8787/admin/api/seed)
echo "$SEED" | grep -q '"added":6'; check "seed 载入 6 条（国内 3 + 海外 3）" $? "$SEED"
for id in seed-zhipu-chat seed-siliconflow-chat seed-zhipu-audio seed-openrouter-chat seed-groq-chat seed-nvidia-chat; do
  echo "$SEED" | grep -q "$id"; check "seed 含 $id" $? "$SEED"
done
# 海外种子全部 disabled（需登记 Key 才参与路由）
echo "$SEED" | python3 -c 'import json,sys; chs=json.load(sys.stdin)["channels"]; ids={"seed-openrouter-chat","seed-groq-chat","seed-nvidia-chat"}; bad=[c["id"] for c in chs if c["id"] in ids and c["enabled"]]; sys.exit(1 if bad else 0)'; check "海外种子全部 disabled（需登记 Key）" $? "$SEED"
# 优先级有序：openrouter(40)>groq(35)>nvidia(30)，且都低于国内(80~100)
echo "$SEED" | python3 -c 'import json,sys; pr={c["id"]:c["priority"] for c in json.load(sys.stdin)["channels"]}; ok=pr["seed-openrouter-chat"]>pr["seed-groq-chat"]>pr["seed-nvidia-chat"] and pr["seed-openrouter-chat"]<80; sys.exit(0 if ok else 1)'; check "海外优先级 40>35>30 且低于国内" $? "$SEED"
# 每条海外种子都声明 vision 能力（含视觉模型），image_url 才能兜底
echo "$SEED" | python3 -c 'import json,sys; chs=json.load(sys.stdin)["channels"]; ids={"seed-openrouter-chat","seed-groq-chat","seed-nvidia-chat"}; bad=[c["id"] for c in chs if c["id"] in ids and "vision" not in c["caps"]]; sys.exit(1 if bad else 0)'; check "海外种子全部声明 vision 能力" $? "$SEED"

# 19.2 海外视觉兜底路由：启用 openrouter 种子并指向 mock，image_url 应落到其 vision 模型
curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/seed-openrouter-chat -H 'Content-Type: application/json' \
  -d '{"enabled":true,"api_key":"test-key-local","base_url":"http://127.0.0.1:8788"}' > /dev/null
rm -f "$MOCK_LOG"; touch "$MOCK_LOG"
VIS=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":[{"type":"text","text":"图里是什么"},{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}]}]}')
echo "$VIS" | grep -q 'gemma-4-31b-it:free'; check "海外视觉模型 gemma-4-31b-it:free 承接 image_url（国内全挂时兜底）" $? "$VIS"
# 纯文本仍走海外 text 模型（同渠道多能力互不干扰）
TXT=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"你好"}]}')
echo "$TXT" | grep -q 'openrouter/free\|nemotron-3-super\|qwen3.8\|cohere'; check "纯文本走海外 text 模型（与视觉互通）" $? "$TXT"

# 19.3 预设清单：海外 3 个 provider 模型已更新为现行免费模型
#     PRESETS 由 /admin/api/channels 的 presets 字段下发（前端不内联），抓 API 而非 HTML
PJ=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$PJ" | grep -qF 'openrouter/free'; check "预设 OpenRouter 含现行 :free 自动路由模型" $? "no openrouter/free"
echo "$PJ" | grep -qF 'llama-3.2-11b-vision-preview'; check "预设 Groq 含视觉模型 llama-3.2-11b-vision-preview" $? "no groq vision"
echo "$PJ" | grep -qF 'nemotron-3.5-super-vl-120b-a12b'; check "预设 NVIDIA NIM 含视觉模型 nemotron-3.5-super-vl" $? "no nim vision"

echo
echo "结果: PASS=$PASS FAIL=$FAIL"
kill $MOCK_PID 2>/dev/null
stop_workerd
rm -f "$ROOT/workerd.retry.capnp" "$ROOT/workerd.retry2.capnp" "$ROOT/workerd.flaky.capnp"
[ "$FAIL" -eq 0 ]