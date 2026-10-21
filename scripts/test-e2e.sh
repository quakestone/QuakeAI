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
SSE=$(curl -sN http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-4-flash","stream":true,"messages":[{"role":"user","content":"hi"}]}')
echo "$SSE" | grep -q 'data:'; check "SSE data 帧到达" $? "no data frames"
echo "$SSE" | grep -q 'data: \[DONE\]'; check "SSE 结束帧 [DONE]" $? "no [DONE]"
echo "$SSE" | grep -q 'WASM 流式透传验证'; check "SSE 内容完整" $? "content mismatch"
# v0.19.19 方案 A：显式请求池中不存在的模型 → 明确拒绝（不再静默回退自动选路）
NOSUCH=$(curl -s -o /tmp/nosuch.json -w '%{http_code}' http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"not-a-real-model","messages":[{"role":"user","content":"hi"}]}')
check "显式请求池外模型被明确拒绝（503）" $([ "$NOSUCH" = "503" ] && grep -q 'requested_model_unavailable' /tmp/nosuch.json && echo 0 || echo 1) "got $NOSUCH $(cat /tmp/nosuch.json)"

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
echo "$SEED" | grep -q '"added":7'; check "seed 载入 7 个默认渠道（国内 3 + DeepSeek + 海外 3）" $? "$SEED"
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
# 回归：即便 glm-4v-flash(vision) 的 priority(80) 高于 glm-4-flash(text) 的 50，
# 纯文本也必须选主能力命中的 glm-4-flash —— alt_kind/primary_rank 排序保证。
# （曾一度用「vision⊃text 一律放宽」实现，导致这里错选 glm-4v-flash 而回归失败。）

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
# 注册另一渠道的同类模型。v0.19.19 方案 A「选择必须诚实」：
# ① 显式请求被冷却的 glm-test-429 → 明确拒绝（503 requested_model_unavailable），
#    不再悄悄换成 glm-4.7-flash——这正是「选了 A 却用 B 回答」的根治；
# ② 不带 model（自动模式）→ 容错语义保留：引擎跳过冷却模型，自动换 glm-4.7-flash。
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-other","provider":"zhipu","name":"冷却测试B","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":100,"models_meta":[{"name":"glm-4.7-flash","kind":"text","priority":50}]}' > /dev/null
RESP=$(curl -s -o /tmp/c2.json -w '%{http_code}' http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}')
check "显式请求冷却模型被明确拒绝（503 requested_model_unavailable）" $([ "$RESP" = "503" ] && grep -q 'requested_model_unavailable' /tmp/c2.json && echo 0 || echo 1) "got $RESP $(cat /tmp/c2.json)"
grep -q '网关不会自动换成别的模型' /tmp/c2.json; check "拒绝信息明示「不自动换模型」" $? "$(cat /tmp/c2.json)"
grep -q 'glm-4.7-flash' /tmp/c2.json; [ $? -ne 0 ]; check "拒绝响应不含回退模型名（没有偷偷换）" $? "leaked fallback model"
RESP2=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"x"}]}')
echo "$RESP2" | grep -q 'glm-4.7-flash'; check "自动模式仍跳过冷却模型换用 glm-4.7-flash（容错语义保留）" $? "$RESP2"
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
grep -q 'IDB.deleteMsg(c.id, k)' "$UI"; check "重新生成截断的消息同步删库" $? "truncated msgs not deleted"
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

echo "== 19. v0.13.0：海外视觉全量对接（免费视觉模型补列 / video_url 甄别 / 兜底路由）=="
stop_workerd
start_workerd workerd.config.capnp; check "workerd(海外融合) 已就绪" $? "worker not reachable"
# 19.1 seed 现在载入 7 条（国内 3 + DeepSeek + 海外 3），全部 disabled 待登记
SEED=$(curl -s -X POST http://127.0.0.1:8787/admin/api/seed)
echo "$SEED" | grep -q '"added":7'; check "seed 载入 7 条（国内 3 + DeepSeek + 海外 3）" $? "$SEED"
for id in seed-zhipu-chat seed-siliconflow-chat seed-zhipu-audio seed-deepseek-chat seed-openrouter-chat seed-groq-chat seed-nvidia-chat; do
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
echo "$VIS" | grep -q 'gemma-4-31b-it:free\|openrouter/free'; check "海外视觉模型（gemma-4-31b-it:free / openrouter/free 自动路由）承接 image_url（国内全挂时兜底）" $? "$VIS"
# 纯文本仍走海外 text 模型（同渠道多能力互不干扰）
TXT=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"你好"}]}')
echo "$TXT" | grep -q 'openrouter/free\|nemotron-3-super\|qwen3.8\|cohere'; check "纯文本走海外 text 模型（与视觉互通）" $? "$TXT"

# 19.3 预设清单：海外 3 个 provider 模型已更新为现行免费模型
#     PRESETS 由 /admin/api/channels 的 presets 字段下发（前端不内联），抓 API 而非 HTML
PJ=$(curl -s http://127.0.0.1:8787/admin/api/channels)
echo "$PJ" | grep -qF 'openrouter/free'; check "预设 OpenRouter 含现行 :free 自动路由模型" $? "no openrouter/free"
echo "$PJ" | grep -qF 'llama-3.2-11b-vision-preview'; check "预设 Groq 含视觉模型 llama-3.2-11b-vision-preview" $? "no groq vision"
echo "$PJ" | grep -qF 'nemotron-3.5-super-vl-120b-a12b'; check "预设 NVIDIA NIM 含视觉模型 nemotron-3.5-super-vl" $? "no nim vision"

# 19.4 OpenRouter 种子已补列全部免费视觉/多模态模型 + 误标修正（qwen3.8、openrouter/free → vision）
CH=$(curl -s http://127.0.0.1:8787/admin/api/channels | python3 -c 'import json,sys; chs=json.load(sys.stdin)["channels"]; print(json.dumps([c for c in chs if c["id"]=="seed-openrouter-chat"][0]))')
for m in "google/gemma-4-26b-a4b-it:free" "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free" "thinkingmachines/inkling:free" "thinkingmachines/inkling-small:free"; do
  echo "$CH" | grep -qF "$m"; check "OpenRouter 种子含免费视觉模型 $m" $? "missing $m"
done
# 此前误标 text 的 qwen3.8 与 openrouter/free 现应为 vision
echo "$CH" | python3 -c 'import json,sys; c=json.load(sys.stdin); mm={m["name"]:m["kind"] for m in c["models_meta"]}; ok=mm.get("qwen/qwen3.8-27b:free")=="vision" and mm.get("openrouter/free")=="vision"; sys.exit(0 if ok else 1)'; check "qwen3.8 / openrouter/free 已修正为 vision" $? "$CH"

# 19.5 视觉甄别增强：video_url 同样走 vision 路由（多模态 omni 模型）
VVIS=$(curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":[{"type":"video_url","video_url":{"url":"data:video/mp4;base64,AAAA"}}]}]}')
echo "$VVIS" | grep -q 'gemma-4-31b-it:free\|openrouter/free'; check "video_url 也路由到海外视觉模型（omni 兜底）" $? "$VVIS"

echo "== 20. v0.14.0：DeepSeek 渠道对接 + 用量成本账本（记账 / 聚合 / 对账 CSV 导出）=="

# 20.1 DeepSeek 种子落库：模型、单价档案、视觉能力标在 deepseek-flash
DS=$(curl -s http://127.0.0.1:8787/admin/api/channels | python3 -c 'import json,sys; chs=json.load(sys.stdin)["channels"]; print(json.dumps([c for c in chs if c["id"]=="seed-deepseek-chat"][0]))')
echo "$DS" | grep -q 'api.deepseek.com/v1'; check "DeepSeek 种子 base_url 指向官方端点" $? "$DS"
echo "$DS" | grep -q 'deepseek-flash'; check "DeepSeek 种子含在售模型 deepseek-flash" $? "$DS"
echo "$DS" | grep -q 'deepseek-v4-pro'; check "DeepSeek 种子含旗舰 deepseek-v4-pro" $? "$DS"
# 已下线的 deepseek-v4-flash-vision-exp 不该再出现
echo "$DS" | grep -qF 'deepseek-v4-flash-vision-exp'; [ $? -ne 0 ]; check "已下线模型 deepseek-v4-flash-vision-exp 已移除" $? "$DS"
# 单价档案：Flash 命中 0.02 / 未命中 1 / 输出 2（CNY 每百万 token）
echo "$DS" | python3 -c 'import json,sys; c=json.load(sys.stdin); m={x["name"]:x for x in c["models_meta"]}.get("deepseek-flash") or {}; ok=m.get("price_in_hit")==0.02 and m.get("price_in_miss")==1.0 and m.get("price_out")==2.0 and m.get("currency")=="CNY"; sys.exit(0 if ok else 1)'; check "DeepSeek Flash 单价档案 0.02/1/2 CNY" $? "$DS"
# deepseek-flash 主能力 text（主力低价模型）+ 次要能力 vision（原生支持 image 输入）；
# v4-pro 纯 text 无 alt。caps 需含 vision。
echo "$DS" | python3 -c 'import json,sys
c=json.load(sys.stdin); m={x["name"]:x for x in c["models_meta"]}
f=m.get("deepseek-flash") or {}; p=m.get("deepseek-v4-pro") or {}
ok = f.get("kind")=="text" and f.get("alt_kind")=="vision" and p.get("kind")=="text" and not p.get("alt_kind") and "vision" in c["caps"]
sys.exit(0 if ok else 1)'; check "deepseek-flash 双能力(text+vision)、pro 纯 text、caps 含 vision" $? "$DS"
# 渠道默认 disabled + priority 70（低于国内 80~100、高于海外 40/35/30）
echo "$DS" | python3 -c 'import json,sys; c=json.load(sys.stdin); ok=(c["enabled"] is False) and c["priority"]==70; sys.exit(0 if ok else 1)'; check "DeepSeek 种子默认 disabled、priority 70" $? "$DS"
echo "$SEED" | python3 -c 'import json,sys; pr={c["id"]:c["priority"] for c in json.load(sys.stdin)["channels"]}; ok=pr["seed-deepseek-chat"]<pr["seed-siliconflow-chat"] and pr["seed-deepseek-chat"]>pr["seed-openrouter-chat"]; sys.exit(0 if ok else 1)'; check "DeepSeek 优先级夹在国内(80/90)与海外(40)之间" $? ""

# 20.2 用量记账：转发一次后 usage 落 DO（tee 旁路采集，不影响流式重放）
curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/seed-deepseek-chat -H 'Content-Type: application/json' \
  -d '{"enabled":true,"api_key":"test-key-local","base_url":"http://127.0.0.1:8788"}' > /dev/null
# 非流式：mock 返回 usage{7,9,16}
curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"hi"}]}' > /dev/null
# 流式：SSE 末帧 usage 同样要被解析（tee 旁路读到的是完整流）
curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"stream":true,"messages":[{"role":"user","content":"hi"}]}' > /dev/null
sleep 1
RAW=$(curl -s http://127.0.0.1:8787/admin/api/usage)
echo "$RAW" | grep -q '"total_tokens"'; check "/admin/api/usage 返回聚合结果" $? "$RAW"
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if d["records"]>=2 else 1)'; check "非流式+流式各记 1 条用量（共≥2）" $? "$RAW"
# token 解析正确：prompt 7 / completion 9 / total 16 / cache_hit 2
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); t=d["totals"]; ok=t["prompt_tokens"]>=14 and t["completion_tokens"]>=18 and t["total_tokens"]>=32 and t["cache_hit_tokens"]>=2; sys.exit(0 if ok else 1)'; check "token 解析正确（prompt/completion/total/cache_hit 累加）" $? "$RAW"
# 按源分组含 deepseek，按 源×模型 分组含 deepseek|deepseek-flash
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); ok=any(o["key"]=="deepseek" for o in d["by_provider"]) and any(o["key"].startswith("deepseek|") for o in d["by_model"]); sys.exit(0 if ok else 1)'; check "聚合含按源 deepseek 与 源×模型 分组" $? "$RAW"
# 关键回归：纯文本必须命中 deepseek-flash。此前它被标成 kind=vision，纯文本请求
# 把它整条过滤掉，DeepSeek 流量全落到更贵的 deepseek-v4-pro。修法是 alt_kind。
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); ks=[o["key"] for o in d["by_model"]]; sys.exit(0 if "deepseek|deepseek-flash" in ks else 1)'; check "纯文本命中 deepseek-flash（alt_kind 兜底生效）" $? "$RAW"
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); ks=[o["key"] for o in d["by_model"]]; sys.exit(0 if "deepseek|deepseek-v4-pro" not in ks else 1)'; check "纯文本未误用更贵的 deepseek-v4-pro" $? "$RAW"
# 视觉请求同样命中 deepseek-flash（复用同一引擎的能力筛选）
rm -f "$MOCK_LOG"; touch "$MOCK_LOG"
curl -s http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":[{"type":"text","text":"图里是什么"},{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}]}]}' > /dev/null
sleep 1
DSV=$(curl -s http://127.0.0.1:8787/admin/api/usage | python3 -c 'import json,sys; d=json.load(sys.stdin); ks=[o["key"] for o in d["by_model"]]; sys.exit(0 if "deepseek|deepseek-flash" in ks else 1)')
check "视觉请求也命中 deepseek-flash（原生支持 image 输入）" $? "$DSV"
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); ok=len(d["by_day"])>=1 and all(len(o["key"])==10 for o in d["by_day"]); sys.exit(0 if ok else 1)'; check "按自然日分组（YYYY-MM-DD）" $? "$RAW"

# 20.3 成本折算：mock 单次 7/9/16 token、Flash 价 0.02/1/2 CNY 每百万
#     → 7*0.02 + 7*1 + 9*2 = 25.14 / 1e6 = 0.00002514（cache_hit 2 从未命中里扣除则略低）
echo "$RAW" | python3 -c 'import json,sys; c=json.load(sys.stdin)["totals"]["cost"]; sys.exit(0 if c>0 else 1)'; check "成本已按单价档案折算（>0）" $? "$RAW"
echo "$RAW" | python3 -c 'import json,sys; d=json.load(sys.stdin); t=d["totals"]; ok=t["priced_requests"]>=2 and t["currency"]=="CNY"; sys.exit(0 if ok else 1)'; check "已计价请求数与币种 CNY 正确" $? "$RAW"

# 20.4 对账 CSV 导出：列名严格对齐 DeepSeek 官方 cost-*.csv，且带 UTF-8 BOM
CSV=$(curl -s -D /tmp/us_hdr http://127.0.0.1:8787/admin/api/usage/export)
grep -qi 'text/csv' /tmp/us_hdr; check "导出 Content-Type 为 text/csv" $? "$(cat /tmp/us_hdr)"
grep -qi 'attachment; filename="gateway-usage-export.csv"' /tmp/us_hdr; check "导出为附件下载" $? "$(cat /tmp/us_hdr)"
printf 'ï»¿' > /tmp/us_bom
curl -s http://127.0.0.1:8787/admin/api/usage/export > /tmp/us_body
head -c 3 /tmp/us_body | od -An -tx1 | grep -q 'ef bb bf'; check "导出带 UTF-8 BOM（Excel 不乱码）" $? "no BOM"
HDR=$(sed 's/^\xef\xbb\xbf//' /tmp/us_body | head -1)
[ "$HDR" = "user_id,start_time_iso,end_time_iso,model,name,wallet_type,cost,currency" ]; check "CSV 列名对齐官方 cost-*.csv schema" $? "$HDR"
grep -q ',deepseek-' /tmp/us_body; check "CSV 含 deepseek 数据行" $? "$(cat /tmp/us_body)"
grep -q '^gateway,' /tmp/us_body; check "CSV 数据行以 gateway 标识（本方网关账本）" $? "$(cat /tmp/us_body)"

# 20.5 管理页 alt_kind 编辑支持（双能力模型可被正确录入与展示）
UI0=$(curl -s http://127.0.0.1:8787/admin)
echo "$UI0" | grep -q 'class="mm-alt"'; check "模型行支持次要能力(alt_kind)下拉" $? "no mm-alt"
echo "$UI0" | grep -q 'r.querySelector(".mm-alt")'; check "保存时读取 alt_kind" $? "no read alt_kind"
echo "$UI0" | grep -q 'capBadge(m.kind, m.alt_kind)'; check "模型行同时渲染主+次能力徽章" $? "no alt badge"
echo "$UI0" | grep -q 'cap-badge.alt'; check "次要能力徽章有区分样式（虚线）" $? "no alt style"
# 原「能力筛选」chip 已被三标签页（文本/音频/图片·视频）取代，改为断言标签页按能力映射；
# alt_kind 的命中改由 TAB_CAPS + capBadge(主, 次) 覆盖（见 §22）。
echo "$UI0" | grep -q 'TAB_CAPS'; check "标签页按能力映射（取代原能力筛选）" $? "no TAB_CAPS"
# 前端改完跑一遍真实往返：编辑 deepseek-flash 的 alt_kind 并保存，账本应落库
curl -s -X PUT http://127.0.0.1:8787/admin/api/channels/seed-deepseek-chat -H 'Content-Type: application/json' \
  -d '{"models_meta":[{"name":"deepseek-flash","kind":"text","alt_kind":"vision","enabled":true,"priority":100,"price_in_hit":0.02,"price_in_miss":1.0,"price_out":2.0,"currency":"CNY"},{"name":"deepseek-v4-pro","kind":"text","alt_kind":"","enabled":true,"priority":90,"price_in_hit":0.025,"price_in_miss":3.0,"price_out":6.0,"currency":"CNY"}]}' > /dev/null
curl -s http://127.0.0.1:8787/admin/api/channels | python3 -c 'import json,sys
c=[x for x in json.load(sys.stdin)["channels"] if x["id"]=="seed-deepseek-chat"][0]
m={x["name"]:x for x in c["models_meta"]}
sys.exit(0 if m["deepseek-flash"].get("alt_kind")=="vision" and m["deepseek-v4-pro"].get("alt_kind")=="" else 1)'; check "alt_kind 经 API 往返持久化正确" $?

# 20.6 管理页用量面板已挂载
UI=$(curl -s http://127.0.0.1:8787/admin | grep -c 'usage-tables')
[ "$UI" -ge 1 ]; check "管理页含用量面板容器" $? "no usage panel"
curl -s http://127.0.0.1:8787/admin | grep -q '导出对账 CSV'; check "管理页含「导出对账 CSV」按钮" $? "no export button"
curl -s http://127.0.0.1:8787/admin | grep -q 'api("/usage")'; check "管理页拉取 /admin/api/usage" $? "no usage fetch"

# ==================== 21. v0.15.0：UI 体验刷版（积分体系 / 多媒体两块 / 精灵拟人 / 回退按钮）====================
echo "== 21.1 后端诊断头：降权模型列表（前端按模型给负分要用）=="
# 触发一次降权：eng-cool 的 glm-test-429 上游 429 → 该模型进 degraded 列表
curl -s -X POST http://127.0.0.1:8787/admin/api/channels -H 'Content-Type: application/json' \
  -d '{"id":"eng-deg","provider":"zhipu","name":"降权测试","base_url":"http://127.0.0.1:8788","api_key":"test-key-local","kind":"chat","tier":"T0","priority":210,"models_meta":[{"name":"glm-test-429","kind":"text","priority":90}]}' > /dev/null
DEG_HDR=$(curl -s -o /dev/null -D - http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"glm-test-429","messages":[{"role":"user","content":"x"}]}' | tr -d '\r')
echo "$DEG_HDR" | grep -qi 'x-gateway-degraded'; check "响应头含 x-gateway-degraded（降权模型列表）" $? "$(echo "$DEG_HDR" | grep -i gateway)"
echo "$DEG_HDR" | grep -qi 'x-gateway-degraded: glm-test-429'; check "降权列表精确到模型名（glm-test-429）" $? "$(echo "$DEG_HDR" | grep -i degraded)"
# 成功链路不应误报降权
OK_HDR=$(curl -s -o /dev/null -D - http://127.0.0.1:8787/v1/chat/completions -H 'Content-Type: application/json' -d '{"messages":[{"role":"user","content":"x"}]}' | tr -d '\r')
echo "$OK_HDR" | grep -qi 'x-gateway-model:'; check "响应头含 x-gateway-model（命中模型，积分用）" $? "$(echo "$OK_HDR" | grep -i model)"

echo "== 21.2 前端积分体系：选择+1 / 使用+1 / 分享+10 / 降权−20，按积分倒序 =="
# 行为测试（真跑一遍计分逻辑，不靠 grep）：曾出现 POINTS_RULE 写 select、
# 调用传 sel 的错位——静态断言全过但积分永远记不上，必须有执行级验证。
PT_OUT=$(node "$ROOT/scripts/test-points.mjs" 2>&1)
PT_FAIL=$(echo "$PT_OUT" | sed -n 's/^结果: PASS=[0-9]* FAIL=\([0-9]*\)$/\1/p')
PT_PASSN=$(echo "$PT_OUT" | sed -n 's/^结果: PASS=\([0-9]*\) FAIL=[0-9]*$/\1/p')
[ "${PT_FAIL:-1}" = "0" ]; check "积分账本行为测试全通过（${PT_PASSN:-0} 项）" $? "$(echo "$PT_OUT" | grep FAIL | head -3)"
UI=$(curl -s http://127.0.0.1:8787/admin)
echo "$UI" | grep -q 'const Points = {'; check "积分账本模块已注入" $? "no Points module"
echo "$UI" | grep -q 'POINTS_RULE = { sel: 1, use: 1, share: 10, degrade: -20 }'; check "积分规则（选择+1/使用+1/分享+10/降权−20）" $? "no rules"
# 计分键与规则键必须一一对应（曾经 POINTS_RULE 写 select 而调用传 sel，
# 导致 add() 直接 return、积分永远记不上——断言要能抓住这种"看起来接好了"）
echo "$UI" | grep -q 'if (!(kind in POINTS_RULE)) return;'; check "积分 add 有规则校验（防键名不匹配静默失败）" $? "no guard"
echo "$UI" | grep -q 'e.sel || 0) \* POINTS_RULE.sel'; check "积分重算用的规则键与计分键一致" $? "key mismatch"
echo "$UI" | grep -q 'localStorage.setItem(POINTS_KEY'; check "积分持久化到 localStorage" $? "no persist"
echo "$UI" | grep -q 'Points.add("sel", m || AUTO_KEY)'; check "选择模型 +1 已接入" $? "no sel hook"
echo "$UI" | grep -q 'Points.add("use", hit)'; check "模型使用 +1 已接入" $? "no use hook"
echo "$UI" | grep -q 'Points.add("share", msgModelOf(btn)'; check "分享 +10 已接入" $? "no share hook"
echo "$UI" | grep -q 'Points.add("degrade", m)'; check "降权 −20 已接入" $? "no degrade hook"
echo "$UI" | grep -q 'items.sort((a, b) => (b.score - a.score)'; check "模型选择器按积分倒序排序" $? "no sort"
echo "$UI" | grep -q 'AUTO_KEY'; check "「自动」作为独立条目参与计分与排序" $? "no auto key"
echo "$UI" | grep -q 'id="pane-points"'; check "模型管理页含积分榜面板" $? "no points pane"
echo "$UI" | grep -q 'id="points-table"'; check "积分榜面板含表格容器" $? "no points body"
echo "$UI" | grep -q 'function renderPoints()'; check "积分榜渲染函数已挂载" $? "no renderPoints"
echo "$UI" | grep -q 'function resetPoints()'; check "积分可清空重置" $? "no resetPoints"
echo "$UI" | grep -q 'x-gateway-degraded'; check "前端读降权诊断头" $? "no degraded header read"
echo "$UI" | grep -q 'x-gateway-model'; check "前端读命中模型诊断头" $? "no model header read"

echo "== 21.3 语音与多媒体设置拆两块（音频模型 / 图片视频模型）=="
echo "$UI" | grep -q 'id="vg-stt-model"'; check "音频块含转写模型选择" $? "no stt select"
echo "$UI" | grep -q 'id="vg-tts-model"'; check "音频块含播音模型选择" $? "no tts select"
echo "$UI" | grep -q 'id="vg-vision-model"'; check "图片视频块含视觉模型选择" $? "no vision select"
echo "$UI" | grep -q 'id="vg-video-model"'; check "图片视频块含视频模型选择" $? "no video select"
echo "$UI" | grep -q 'class="vg-block"'; check "两块用 vg-block 容器分隔" $? "no vg-block"
echo "$UI" | grep -q 'id="mg-stt-model"'; check "模型管理页含音频块内联选择" $? "no mg stt"
echo "$UI" | grep -q 'id="mg-vision-model"'; check "模型管理页含图片视频块内联选择" $? "no mg vision"
echo "$UI" | grep -q 'if (voiceCfg.ttsModel) payload.model = voiceCfg.ttsModel'; check "TTS 播音模型真正生效" $? "tts model not applied"
echo "$UI" | grep -q 'if (voiceCfg.sttModel) fd.append("model", voiceCfg.sttModel)'; check "STT 转写模型真正生效" $? "stt model not applied"
echo "$UI" | grep -q 'function pickMmModel(hasMedia)'; check "图片视频模型选择逻辑已接入请求" $? "no pickMmModel"

echo "== 21.4 UI 改造：侧栏 / 顶栏 / 精灵拟人 / 回退按钮 =="
echo "$UI" | grep -q 'id="btn-model"'; check "模型选择按钮在输入区（+ 号旁）" $? "no btn-model"
echo "$UI" | grep -q 'id="btn-newchat"'; check "顶栏右上角为新建会话" $? "no newchat"
echo "$UI" | grep -q 'class="dlg-back"'; check "对话框左上角有回退按钮" $? "no dlg-back"
[ "$(echo "$UI" | grep -c 'class="dlg-back"')" -ge 6 ]; check "全部 6 个对话框都有回退按钮" $? "$(echo "$UI" | grep -c 'class="dlg-back"') 个"
echo "$UI" | grep -q 'function bindEdgeSwipe()'; check "边缘滑动手势已绑定" $? "no swipe"
echo "$UI" | grep -q 'topClosable()'; check "滑动手势覆盖所有子页/弹层" $? "no topClosable"
echo "$UI" | grep -q 'orb-ripple\|orb-bars\|orb-breathe\|orb-think\|orb-speak\|@keyframes orb'; [ $? -ne 0 ]; check "精灵球动效已全部移除" $? "$(echo "$UI" | grep -o 'orb-ripple\|orb-bars\|@keyframes orb' | sort -u | tr '\n' ' ')"
echo "$UI" | grep -q 'class="orb-eye l"'; check "AI 精灵有拟人五官（眼睛）" $? "no eye"
echo "$UI" | grep -q 'class="orb-mouth"'; check "AI 精灵有拟人五官（嘴）" $? "no mouth"
echo "$UI" | grep -q 'class="orb-cheek l"'; check "AI 精灵有腮红" $? "no cheek"
echo "$UI" | grep -q 'class="primary" onclick="openDrawer()">＋ 添加服务商'; check "添加服务商入口在模型管理·模型服务商标签" $? "no register entry"
if echo "$UI" | grep -q 'onclick="openVoiceCfg()" aria-label="语音与多媒体设置"'; then check "多媒体设置小图标已从页头移除" 1 "mm icon still there"; else check "多媒体设置小图标已从页头移除" 0 ""; fi
echo "$UI" | grep -q 'id="btn-call-model"'; check "通话页模型选择按钮保留" $? "no call model btn"
echo "$UI" | grep -q 'function sniffAudioType(buf)'; check "TTS 播放修复：嗅探音频 MIME" $? "no sniff"
echo "$UI" | grep -q 'function playViaWebAudio(buf, mime)'; check "TTS 播放修复：Web Audio 兜底路径" $? "no webaudio fallback"
SHARE=$(curl -s http://127.0.0.1:8787/s)
echo "$SHARE" | grep -q 'id="back-btn"'; check "分享落地页有回退按钮" $? "no share back btn"


echo
echo "== 22. v0.16.0 模型选择器分页 / 搜索（⚡ DRILL 引导 prompt 内置） =="
echo "$UI" | grep -q 'const MP_PAGE_SIZE = 5'; check "模型列表每页上限 5 条" $? "no page size"
echo "$UI" | grep -q 'id="model-pager"'; check "模型选择器有分页控件" $? "no pager"
echo "$UI" | grep -q 'id="model-page-n"'; check "分页显示页码与总数" $? "no page num"
echo "$UI" | grep -q 'function turnModelPage(d)'; check "翻页函数已实现" $? "no turnPage"
echo "$UI" | grep -q 'pages > 1'; check "仅一页时隐藏分页条" $? "pager always shown"
echo "$UI" | grep -q 'id="model-search"'; check "模型选择器有搜索框" $? "no search box"
echo "$UI" | grep -q 'function setModelQuery(q)'; check "模糊查询函数已实现" $? "no setQuery"
echo "$UI" | grep -q 'it.name.toLowerCase().includes(mpQuery)'; check "搜索匹配模型名" $? "not match name"
echo "$UI" | grep -q 'it.ch && it.ch.provider'; check "搜索同时匹配渠道名与供应商" $? "not match ch"
echo "$UI" | grep -q 'mpPages\[mpTab\] = 1;'; check "换搜索词回到当前标签第 1 页" $? "no reset page"
echo "$UI" | grep -q 'class="icon-btn mp-mgmt"'; check "模型管理入口在选择页右上角" $? "no mgmt entry"
echo "$UI" | grep -q 'onclick="openModelScope()"'; check "右上角入口打开模型作用域/设置" $? "entry not wired"
echo "$UI" | grep -q 'const DRILL_PROMPT = `PRE-ANSWER DRILL PROTOCOL v1.0'; check "已内置 PRE-ANSWER DRILL PROTOCOL v1.0" $? "no drill prompt"
echo "$UI" | grep -q 'DRILL 3：验证任务'; check "DRILL 提示词含三轮清单" $? "incomplete protocol"
echo "$UI" | grep -q '#btn-send.streaming { animation: sendPulse'; check "发送按钮普通生成动效" $? "no sendPulse"
echo "$UI" | grep -q '@media (prefers-reduced-motion: reduce)'; check "动效尊重系统减弱偏好" $? "no reduced motion"
python3 - <<'PYX'
import re,sys
s=open('src/admin_ui.html',encoding='utf-8').read()
top=re.search(r'<header class="chat-top">(.*?)</header>', s, re.S)
body=top.group(1) if top else ''
# 顶栏右侧顺序：主题 → 通话 → 新建会话
i_theme=body.find('id="btn-theme"'); i_call=body.find('id="btn-call"'); i_new=body.find('id="btn-newchat"')
sys.exit(0 if -1<i_theme<i_call<i_new else 1)
PYX
check "顶栏右侧顺序：主题 → 通话 → 新建会话" $? "wrong topbar order"
echo "$UI" | grep -q 'id="mgmt-tabs"'; check "模型管理有独立标签页容器" $? "no mgmt tabs"
echo "$UI" | grep -q 'data-mtab="resources"'; check "作用域页含模型服务商标签" $? "no resources tab"
echo "$UI" | grep -q 'data-mtab="overview"'; [ $? -ne 0 ]; check "v0.19.19 撤销概览标签（信息并入模型标签头部）" $? "overview tab still exists"
echo "$UI" | grep -q 'id="sect-mm"'; check "多媒体配置已脱离模型管理（移至设置）" $? "no sect-mm"
echo "$UI" | grep -q 'data-mtab="skill"'; check "模型管理含 skill 标签" $? "no skill tab"
echo "$UI" | grep -q 'id="pane-skill"'; check "模型管理有 skill 独立面板" $? "no skill pane"
echo "$UI" | grep -q '语音与多媒体模型'; check "多媒体配置标题为语音与多媒体模型" $? "no mm title"
echo "$UI" | grep -q 'data-mtab="usage"'; check "模型管理含模型用量标签" $? "no usage tab"
echo "$UI" | grep -q 'data-mtab="points"'; check "模型管理含模型积分榜标签" $? "no points tab"
echo "$UI" | grep -qF 'const MGMT_TABS = ["models", "resources", "skill", "usage", "points"]'; check "作用域页共 5 个标签（模型/模型服务商/skill管理/用量/积分榜）" $? "bad MGMT_TABS"
echo "$UI" | grep -q 'const MG_PAGE_SIZE = 5;'; check "管理页每页 5 条（超5 全分页）" $? "bad MG_PAGE_SIZE"
echo "$UI" | grep -q 'id="pane-usage"'; check "模型用量独立面板" $? "no usage pane"
echo "$UI" | grep -q 'id="model-rows-pager"'; check "模型列表有翻页条" $? "no rows pager"
echo "$UI" | grep -q 'function turnModelRowsPage(d)'; check "模型列表翻页函数" $? "no rows page fn"
echo "$UI" | grep -q 'mgPages\[modelScope\] = cur'; check "各模型标签独立页码" $? "pages not per-tab"
echo "$UI" | grep -q 'id="points-pager"'; check "积分榜有翻页条" $? "no points pager"
echo "$UI" | grep -q 'function turnPointsPage(d)'; check "积分榜翻页函数" $? "no points page fn"
echo "$UI" | grep -q 'id="usage-pager"'; check "用量表有翻页条" $? "no usage pager"
echo "$UI" | grep -q 'function turnUsagePage(d)'; check "用量表翻页函数" $? "no usage page fn"
echo "$UI" | grep -q '\.tbl-scroll'; check "宽表包横向滚动壳（防出界）" $? "no tbl-scroll"
echo "$UI" | grep -q '\.mg-pane-body'; check "标签面板体可收缩（防出界）" $? "no pane body"
echo "$UI" | grep -q 'class="brand-logo"'; check "首页左上角有 logo" $? "no brand logo"
echo "$UI" | grep -q '<span class="brand-name">QuakeAI</span>'; check "首页左上角列出产品名 QuakeAI" $? "no brand name"
# v0.19.14：输入框无 placeholder、顶栏中间留白、发送即清空
python3 - <<'PYX'
import re, sys
s = open('src/admin_ui.html', encoding='utf-8').read()
ta = re.search(r'<textarea id="input"[^>]*>', s)
ct = re.search(r'<span id="chat-title"[^>]*>(.*?)</span>', s, re.S)
so = s[s.index("async function sendOrStop"):]
so = so[:so.index("\n}\n")]
ok = bool(ta) and 'placeholder' not in ta.group(0) \
     and bool(ct) and ct.group(1).strip() == "" \
     and so.index('input.value = ""') < so.index("await sendWith(text")
sys.exit(0 if ok else 1)
PYX
check "输入框无提示词 / 顶栏中间留白 / 发送即清空" $? "composer placeholder or clear-order wrong"

echo "== 22.x 模型管理页拆分（作用域页 + 设置页）=="
echo "$UI" | grep -q 'id="view-models"'; check "模型管理作用域页 DOM 存在" $? "no view-models"
echo "$UI" | grep -q 'id="view-settings"'; check "全局设置页 DOM 存在" $? "no view-settings"
echo "$UI" | grep -q 'function currentView'; check "#/models #/settings 路由已实现" $? "no router"
echo "$UI" | grep -q 'function applySect'; check "设置页分区切换函数已实现" $? "no applySect"
echo "$UI" | grep -q 'registry: "模型持续性"'; check "设置·模型登记已改名「模型持续性」" $? "registry not renamed"
echo "$UI" | grep -q 'id="sect-registry"'; check "设置·模型持续性分区存在" $? "no sect-registry"
echo "$UI" | grep -q 'id="sect-mm"'; check "设置·多媒体配置分区存在" $? "no sect-mm"
echo "$UI" | grep -q 'id="sect-wf"'; check "设置·工作流参数分区存在" $? "no sect-wf"
echo "$UI" | grep -q 'id="sect-api"'; check "设置·接入方式分区存在" $? "no sect-api"
echo "$UI" | grep -q 'id="sect-auth"'; check "设置·管理面鉴权分区存在" $? "no sect-auth"
if echo "$UI" | grep -q 'id="sect-keys"'; then check "「模型换 Key」子页已销毁" 1 "sect-keys still there"; else check "「模型换 Key」子页已销毁" 0 ""; fi
if echo "$UI" | grep -q 'renderKeyQueue'; then check "renderKeyQueue 已移除" 1 "renderKeyQueue still there"; else check "renderKeyQueue 已移除" 0 ""; fi
if echo "$UI" | grep -q '模型换 Key'; then check "「模型换 Key」文案已清除" 1 "still there"; else check "「模型换 Key」文案已清除" 0 ""; fi
echo "$UI" | grep -q 'data-mtab="resources"'; check "模型管理页新增「资源」标签" $? "no resources tab"
echo "$UI" | grep -q 'id="pane-resources"'; check "模型管理页含资源面板" $? "no pane-resources"
echo "$UI" | grep -q 'function renderResourceRows'; check "资源面板渲染已实现" $? "no renderResourceRows"
echo "$UI" | grep -q 'const RES_PER = 6;'; check "资源列表遵循规范列表分页（每页6条+页码）" $? "no RES_PER"
echo "$UI" | grep -q 'id="reg-pager"'; check "资源列表用统一 .pager 分页控件" $? "no reg-pager"
echo "$UI" | grep -q 'function openResources'; check "概览可直接跳到模型管理资源标签" $? "no openResources"
echo "$UI" | grep -q 'async function testTtsChain()'; check "多媒体配置·TTS链路测试已实现" $? "no testTtsChain"
echo "$UI" | grep -q 'function openModelScope()'; check "选择器⚙入口函数已实现" $? "no openModelScope"
echo "$UI" | grep -q '>模型管理</div>'; check "模型管理页头只显示「模型管理」字样" $? "no bare title"
echo "$UI" | grep -q 'id="mh-cur"'; check "模型标签头部含当前模型条" $? "no mh-cur"
echo "$UI" | grep -q 'id="mh-metrics"'; check "模型标签头部含统计 banner" $? "no mh-metrics"
echo "$UI" | grep -q 'id="mh-health"'; check "模型标签头部含渠道健康速览" $? "no mh-health"
echo "$UI" | grep -q 'id="mh-events"'; check "模型标签头部含最近事件" $? "no mh-events"
echo "$UI" | grep -q 'function renderModelsHead'; check "模型标签头部渲染函数存在" $? "no renderModelsHead"

# v0.19.15 / v0.19.16：实时流引擎
echo "$UI" | grep -qF 'const SSE_SEP = /\r?\n\r?\n/;'; check "SSE 分隔兼容 CRLF（\\r\\n\\r\\n）" $? "no CRLF separator"
echo "$UI" | grep -qF 'function paintStream(final)'; check "流式增量绘制（只动当前条 DOM）" $? "no paintStream"
echo "$UI" | grep -qF 'function stableCut(src)'; check "稳定前缀切分（段落级增量）" $? "no stableCut"
echo "$UI" | grep -qF 'function splitSegs(s)'; check "按段切分且保留代码围栏内空行" $? "no splitSegs"
echo "$UI" | grep -qF 'function paintContent(content, shown, final)'; check "稳定区只追加 / 尾段每帧重画" $? "no paintContent"
echo "$UI" | grep -qF 'minRate: 14'; check "呈现速率有下限（断顿也继续吐字）" $? "no minRate"
echo "$UI" | grep -qF 'const primed = STREAM.target.length >= TYPE.primeChars'; check "首字先攒缓冲再开口" $? "no prime"
echo "$UI" | grep -qF 'requestAnimationFrame(streamFrame)'; check "rAF 合并绘制（一帧只画一次）" $? "no rAF merge"
echo "$UI" | grep -qF 'if (followBottom) stickBottom()'; check "上翻阅读时不抢滚动条" $? "scroll steal"
echo "$UI" | grep -qF 'id="to-bottom"'; check "有「回到最新」按钮" $? "no to-bottom"
python3 - <<'PYX'
import sys
s = open('src/admin_ui.html', encoding='utf-8').read()
i = s.index('const reader = resp.body.getReader();')
j = s.index('if (buf.trim()) handleSseBlock', i)
sys.exit(0 if 'renderMessages()' not in s[i:j] else 1)
PYX
check "流循环里不再整列重绘" $? "renderMessages still in stream loop"
echo "$UI" | grep -q 'id="mp-tabs"'; check "模型选择页有 4 标签栏" $? "no mp tabs"

echo "$UI" | grep -q 'const MP_TABS = \["text", "skill", "audio", "mm"\]'; check "选择页共 4 个标签" $? "bad MP_TABS"
# v0.19.22：输入区结构改为「附件栏（框内左上部）+ 按钮行」两段——
# 顺序：attach-bar → + ⋯ 输入框 🎙 ↑。缩略图住进文本框内，"未发送"一眼可辨。
python3 - <<'PYX'
import re, sys
s = open('src/admin_ui.html', encoding='utf-8').read()
m = re.search(r'<div id="composer">(.*?)</div>\s*<input type="file"', s, re.S)
ids = re.findall(r'id="([^"]+)"', m.group(1)) if m else []
sys.exit(0 if ids == ["attach-bar", "btn-attach", "btn-model", "input", "btn-voice", "btn-send"] else 1)
PYX
check "输入区顺序：附件栏（框内）→ + ⋯ 输入框 🎙 ↑" $? "wrong composer order"
grep -q 'id="edit-chip"' "$ROOT/src/admin_ui.html"; check "有「修改中」提示条" $? "no edit chip"
grep -q 'function cancelEdit' "$ROOT/src/admin_ui.html"; check "修改可取消" $? "no cancelEdit"
grep -q 'if (editCtx) {' "$ROOT/src/admin_ui.html"; check "发送前处理修改态" $? "edit not applied on send"
if grep -q 'id="model-jump"' "$ROOT/src/admin_ui.html"; then check "「当前在第 N 页」已删除" 1 "model-jump still there"; else check "「当前在第 N 页」已删除" 0 ""; fi
grep -q 'mpTab = curModel ? (modelTabOf(curModel) || "text") : "text";' "$ROOT/src/admin_ui.html"; check "打开模型页定位到当前模型标签" $? "no tab locate"
grep -q 'Math.floor(idx / MP_PAGE_SIZE) + 1' "$ROOT/src/admin_ui.html"; check "打开模型页定位到当前页" $? "no page locate"
# 回归（v0.19.12）：内核就绪步只回 {id,input}，没有 plugin 字段。
# 曾写成 plugin(st.plugin, st.input)，插件收到 undefined，整条 run 空转——
# 表现就是模型页永远「检测中」。参数错位用 grep 就能挡住。
echo "$UI" | grep -q 'await fn(st.input)'; check "工作流插件收到的是步骤 input" $? "plugin input not passed"
if echo "$UI" | grep -q 'plugin(st\.plugin'; then check "驱动循环不再传 st.plugin 当入参" 1 "st.plugin passed as input"; else check "驱动循环不再传 st.plugin 当入参" 0 ""; fi
echo "$UI" | grep -q 'let mpPages = { text: 1, skill: 1, audio: 1, mm: 1 }'; check "选择页各标签独立页码" $? "mpPages not per-tab"
echo "$UI" | grep -q 'if (mpTab === "skill") return \[\];'; check "skill 标签不从模型池取数" $? "skill pulls models"
# 回归：后台 HTML 曾完全缺 Cache-Control，浏览器缓存旧 SPA，"改了没生效"
grep -q '"Cache-Control", "no-store, no-cache, must-revalidate, max-age=0"' src/admin.rs; check "后台 HTML 已禁缓存" $? "no no-store"
grep -qF '.set("Pragma", "no-cache")' src/admin.rs; check "后台 HTML 带 Pragma 兼容老浏览器" $? "no pragma"
grep -q '.set("X-Gateway-Version", VERSION)' src/admin.rs; check "后台 HTML 带版本头" $? "no version header"
grep -q 'split(MARK).collect::<Vec<_>>().join(VERSION)' src/admin.rs; check "版本号注入全部占位符" $? "single replace only"
# 注意：这一段 $UI 来自运行中的服务（占位符已被替换成真实版本号），
# 所以涉及 __BUILD_VERSION__ 字面量的断言必须查源文件。
grep -qF 'const BUILD_MARK = "__BUILD_VERSION__";' "$ROOT/src/admin_ui.html"; check "前端有 BUILD_MARK 常量" $? "no BUILD_MARK"
grep -q 'split(MARK)' "$ROOT/src/admin.rs" && grep -q 'join(VERSION)' "$ROOT/src/admin.rs"; check "服务端注入版本到页面" $? "no inject"
echo "$UI" | grep -qF 'const BUILD_MARK = "'; check "服务页面已注入真实版本号" $? "mark not replaced"
echo "$UI" | grep -q 'id="cache-warn"'; check "侧栏有缓存自检提示位" $? "no cache-warn"
echo "$UI" | grep -q 'function setMpTab(k)'; check "选择页标签切换函数" $? "no setMpTab"
echo "$UI" | grep -q 'const TAB_CAPS = { text: \["text"\], mm: \["stt", "tts", "vision", "video"\] }'; check "多媒体标签合并音频+图视能力" $? "bad tab caps"
echo "$UI" | grep -q 'let mgmtTab = "models"'; check "标签页默认模型（v0.19.19 撤销概览）" $? "no default tab"
echo "$UI" | grep -q 'id="convo-pager"'; check "历史会话列表有分页条" $? "no convo pager"
echo "$UI" | grep -q 'const CONVO_PAGE_SIZE = 8;'; check "会话每页 8 条" $? "bad CONVO_PAGE_SIZE"
echo "$UI" | grep -q 'function turnConvoPage(d)'; check "会话翻页函数" $? "no turnConvoPage"
echo "$UI" | grep -q 'convoPage = Math.floor(i / CONVO_PAGE_SIZE) + 1'; check "打开会话自动跳到所在页" $? "no jump"
if echo "$UI" | grep -q '"store: error"'; then check "store 徽章不再显示 error" 1 "error still shown"; else check "store 徽章不再显示 error" 0 ""; fi
if echo "$UI" | grep -q 'id="store-badge"'; then check "store 徽章已从页头移除" 1 "store-badge still there"; else check "store 徽章已从页头移除" 0 ""; fi
# v0.19.22：模型管理页头从 bare（标题伪装返回）改为 slim 范式——左上角 ‹ 回退键
# （与设置页同构；原来把标题做成返回入口没有任何可见性提示）
echo "$UI" | grep -q '<button class="back" onclick="navigate('"'"'chat'"'"')" aria-label="返回聊天" title="返回聊天">‹</button>'; check "模型管理页头有 ‹ 回退键（v0.19.22 弃用 bare）" $? "no back button in models header"
if echo "$UI" | grep -q 'mgmt-top bare'; then check "bare 页头已弃用" 1 "bare header still present"; else check "bare 页头已弃用" 0 ""; fi
echo "$UI" | grep -q 'class="mgmt-top slim"'; check "设置页页头 slim（只留返回+标题）" $? "no slim header"
echo "$UI" | grep -q 'tabbar'; check "标签栏样式存在（横向滚动不换行）" $? "no tabbar css"
echo "$UI" | grep -q 'flex-wrap: wrap; min-width: 0; }'; check "模型行允许换行防溢出" $? "m-row overflow"

# ==================== 23. v0.19.0：skill 子系统（独立于模型的表达线）====================
echo "== 23. skill 子系统：增删改查 / 计数 / 选择器前6 + 扩展入口 =="
SKILLS_API=http://127.0.0.1:8787/admin/api/skills
S1=$(curl -s -X POST $SKILLS_API -H 'Content-Type: application/json' -d '{"title":"三个诸葛亮","prompt":"PRE-ANSWER DRILL PROTOCOL","summary":"回答前三角度自辩，再统一作答"}')
echo "$S1" | grep -q '"id"'; check "POST /skills 创建 skill" $? "$S1"
S1ID=$(echo "$S1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["skill"]["id"])')
echo "$S1" | grep -q '"summary":"回答前三角度自辩，再统一作答"'; check "POST /skills 落库 summary 简介字段" $? "$S1"
S2=$(curl -s -X POST $SKILLS_API -H 'Content-Type: application/json' -d '{"title":"中英润色","prompt":"polish"}')
S2ID=$(echo "$S2" | python3 -c 'import json,sys; print(json.load(sys.stdin)["skill"]["id"])')
curl -s -X PUT $SKILLS_API/$S1ID -H 'Content-Type: application/json' -d '{"enabled":false}' | grep -q '"enabled":false'; check "PUT /skills/{id} 更新字段" $? "update failed"
curl -s -X PUT $SKILLS_API/$S1ID -H 'Content-Type: application/json' -d '{"summary":"改过的简介"}' | grep -q '"summary":"改过的简介"'; check "PUT /skills/{id} 覆盖 summary" $? "summary update failed"
curl -s -X POST $SKILLS_API/$S1ID/use > /dev/null
USE=$(curl -s -X POST $SKILLS_API/$S1ID/use)
echo "$USE" | grep -q '"used_count":2'; check "POST /skills/{id}/use 累加 used_count" $? "$USE"
LIST=$(curl -s $SKILLS_API)
echo "$LIST" | python3 -c 'import json,sys; s=json.load(sys.stdin)["skills"]; print("OK" if s[0]["used_count"]>=s[-1]["used_count"] else "BAD")' | grep -q OK; check "GET /skills 按 used_count 倒序" $? "$LIST"
curl -s -X DELETE $SKILLS_API/$S2ID | grep -q '"ok":true'; check "DELETE /skills/{id} 删除" $? "delete failed"
curl -s $SKILLS_API | python3 -c "import json,sys; s=[x['id'] for x in json.load(sys.stdin)['skills']]; print('OK' if '$S2ID' not in s else 'BAD')" | grep -q OK; check "删除后列表不含被删 skill" $? "still present"
curl -s -o /tmp/sk.json -w '%{http_code}' -X POST $SKILLS_API -H 'Content-Type: application/json' -d '{"title":""}' | grep -q 400; check "POST /skills 缺标题返回 400" $? "no 400"
curl -s -o /tmp/sk2.json -w '%{http_code}' -X POST $SKILLS_API -H 'Content-Type: application/json' -d '{"title":"x","prompt":""}' | grep -q 400; check "POST /skills 缺提示词返回 400" $? "no 400 prompt"
echo "$UI" | grep -q 'function renderSkillPicker'; check "前端选择器 skill 渲染函数存在" $? "no renderSkillPicker"
echo "$UI" | grep -q 'function renderSkillPane'; check "前端管理页 skill 面板函数存在" $? "no renderSkillPane"
echo "$UI" | grep -q 'function openSkillDialog'; check "前端 skill 对话框函数存在" $? "no openSkillDialog"
echo "$UI" | grep -q 'function selectSkill'; check "前端选择 skill 函数存在" $? "no selectSkill"
echo "$UI" | grep -q 'id="skill-dlg"'; check "前端 skill 对话框 DOM 存在" $? "no skill-dlg"
echo "$UI" | grep -q 'id="pane-skill"'; check "前端管理页 skill 面板 DOM 存在" $? "no pane-skill"
echo "$UI" | grep -q 'sk-add'; check "前端扩展 skill 入口存在" $? "no sk-add"
echo "$UI" | grep -q 'let activeSkill = null'; check "前端 activeSkill 状态存在" $? "no activeSkill"
echo "$UI" | grep -q 'history.unshift(...sysMsgs.map(s => ({ role: "system", content: s.prompt })));'; check "前端发送时注入 skill system 消息（v0.19.24 双流）" $? "no inject"
echo "$UI" | grep -q 'location.hash === "#/skills"'; check "前端 #/skills 路由存在" $? "no skills route"

echo "== 23.5 v0.19.19：skill 简介（summary）+ 扩展按钮直通新增 + skill 管理页改造 =="
echo "$UI" | grep -q 'id="sk-summary"'; check "前端 skill 对话框含简介输入框" $? "no sk-summary"
echo "$UI" | grep -q 'maxlength="30"'; check "简介输入框限长 30 字" $? "no maxlength 30"
echo "$UI" | grep -q 'function summarizePrompt'; check "前端简介兜底截取函数存在" $? "no summarizePrompt"
echo "$UI" | grep -q 'sk-desc'; check "前端选择器列表含简介行 DOM" $? "no sk-desc"
echo "$UI" | grep -q 'onclick="openSkillDialog()">＋ 扩展 SKILL（新增）'; check "扩展 SKILL 按钮直接弹新增对话框（大写 SKILL）" $? "extend button not bound to openSkillDialog"
echo "$UI" | grep -q '＋ 新增 skill'; [ $? -ne 0 ]; check "管理页 skill 面板不再有新增按钮" $? "add button still in pane"
echo "$UI" | grep -q 'title="SKILL 管理：独立于模型的表达线，二级分类 + 分页">SKILL 管理'; check "管理页提示词标签改名 SKILL 管理（v0.19.23）" $? "tab not renamed"
echo "$UI" | grep -q 'if (!s.summary) s.summary = summarizePrompt(s.prompt);'; check "旧 skill 无 summary 时前端兜底生成" $? "no legacy fallback"
echo "$UI" | grep -q 'summary: "回答前三角度自辩，再统一作答"'; check "内置种子 skill 带简介" $? "seed has no summary"

echo "== 24. v0.19.21 发送流水线：文本/图片/音频/视频状态机 + 工作流小引擎串行流 =="
# 行为测试（真跑 WASM 内核 + 抽测纯函数）：串行依赖由 depends_on 链保证，
# 一旦种子漏依赖（三步同时就绪变并行），静态断言全过但流水线语义已坏。
SP_OUT=$(node "$ROOT/scripts/test-send-pipeline.mjs" 2>&1)
SP_FAIL=$(echo "$SP_OUT" | sed -n 's/^结果: PASS=[0-9]* FAIL=\([0-9]*\)$/\1/p')
SP_PASSN=$(echo "$SP_OUT" | sed -n 's/^结果: PASS=\([0-9]*\) FAIL=[0-9]*$/\1/p')
[ "${SP_FAIL:-1}" = "0" ]; check "发送流水线行为测试全通过（${SP_PASSN:-0} 项，含内核串行语义）" $? "$(echo "$SP_OUT" | grep FAIL | head -3)"
echo "$UI" | grep -q 'id="pipe-bar"'; check "输入区含流水线进度条容器" $? "no pipe-bar"
echo "$UI" | grep -q 'const FSM = { READY: "ready"'; check "媒体状态机六态常量存在" $? "no FSM"
echo "$UI" | grep -q 'const PIPE_STAGES = \['; check "流水线阶段表存在" $? "no PIPE_STAGES"
echo "$UI" | grep -q '{ key: "chat",  icon: "💬", label: "生成回答" }'; check "末阶段为生成回答" $? "no chat stage"
echo "$UI" | grep -q 'function fsmSet'; check "附件状态机迁移函数存在" $? "no fsmSet"
echo "$UI" | grep -q 'function pipeRender'; check "流水线进度条渲染函数存在" $? "no pipeRender"
echo "$UI" | grep -q 'const WF_SEED_SEND'; check "发送流水线工作流种子存在" $? "no WF_SEED_SEND"
echo "$UI" | grep -q '{ id: "video", plugin: "wf.send_video", depends_on: \["stt"\]'; check "视频步依赖语音步（串行链）" $? "no dep stt->video"
echo "$UI" | grep -q '{ id: "image", plugin: "wf.send_image", depends_on: \["video"\]'; check "图片步依赖视频步（串行链）" $? "no dep video->image"
echo "$UI" | grep -q 'send: load(WF_SEED_SEND)'; check "发送种子随内核一起加载" $? "seed not loaded"
echo "$UI" | grep -q 'async function runSendPipeline'; check "发送流水线编排函数存在" $? "no runSendPipeline"
echo "$UI" | grep -q 'await runSendPipeline(text, atts)'; check "sendWith 已接入流水线" $? "sendWith not wired"
echo "$UI" | grep -q 'function modelHasVision'; check "图片智能混合路由决策函数存在" $? "no modelHasVision"
echo "$UI" | grep -q 'async function videoFrames'; check "视频多帧抽取函数存在" $? "no videoFrames"
echo "$UI" | grep -q 'async function wfVisionDescribe'; check "视觉理解调用函数存在" $? "no wfVisionDescribe"
echo "$UI" | grep -q 'a.desc'; check "附件理解结果（desc）字段已使用" $? "no desc"
echo "$UI" | grep -q 'if (a.desc) base.desc = a.desc;'; check "desc 随附件持久化进 IDB" $? "desc not persisted"
echo "$UI" | grep -q '理解：${a.desc}'; check "buildApiContent 渲染理解注记" $? "no desc note"
echo "$UI" | grep -q 'pipeFinishChat(true)'; check "流式结束收尾流水线⑤阶段" $? "no chat stage finish"
echo "$UI" | grep -q 'const inPipe = PIPE.visible'; check "流水线期间输入区 chip 持续可见" $? "no inPipe fallback"

echo "== 25. v0.19.22 UI 优化：SKILL 页大写化/刀形 logo/限5分页 + 管理页改名回退 + 缩略图入文本框 =="
# —— 需求 1：选择器 SKILL 页 ——
echo "$UI" | grep -q 'data-mtab="skill" onclick="setMpTab('"'"'skill'"'"')" title="SKILL：独立于模型的表达线">SKILL'; check "1a 选择器 skill 小标签改大写 SKILL" $? "tab not uppercased"
echo "$UI" | grep -q '<span id="mp-ttl">选择模型</span>'; check "1a 对话框标题容器存在（大分页名）" $? "no mp-ttl"
echo "$UI" | grep -q 'function mpTitle(k)'; check "1a 标题跟随小标签函数存在" $? "no mpTitle"
echo "$UI" | grep -q 'skill: "SKILL" };'; check "1a MP_TAB_LABEL skill 项为大写 SKILL" $? "label not SKILL"
echo "$UI" | grep -q 'const SKILL_LOGO'; check "1b SKILL 刀形几何 logo 常量存在" $? "no SKILL_LOGO"
echo "$UI" | grep -q 'M12 1.6 15.1 9.2v6H8.9v-6Z'; check "1b logo 为匕首刀形路径" $? "not knife path"
echo "$UI" | grep -q '<span class="sk-rank">${SKILL_LOGO}</span>'; check "1b 选择器条目用刀形 logo" $? "picker logo not swapped"
echo "$UI" | grep -q 'class="sk-ic"><svg class="sk-logo"'; check "1b composer skill chip 也用刀形 logo" $? "chip logo not swapped"
echo "$UI" | grep -q 'id="skill-chip-title"'; check "1b chip 标题容器仍在（logo 替换不影响结构）" $? "no chip title"
echo "$UI" | grep -q 'const top = list.slice((cur - 1) \* MP_PAGE_SIZE, cur \* MP_PAGE_SIZE);'; check "1c skill 列表按 MP_PAGE_SIZE(5) 分页切片" $? "no page size slice"
echo "$UI" | grep -q 'mpSkillPages\[cat\] = Math.min(Math.max(1, mpSkillPages\[cat\] || 1), pages);'; check "1c skill 页码钳制存在（v0.19.23 改按二级分类）" $? "no skill page clamp"
echo "$UI" | grep -q 'function mpModelOnly'; check "1 SKILL 标签隐藏模型专属控件" $? "no mpModelOnly"
echo "$UI" | grep -q '🔍 搜索 SKILL 标题 / 简介…'; check "1a 搜索占位符跟随标签" $? "no skill placeholder"
# —— 需求 2：管理页改名 + 回退 + 按钮常驻 ——
echo "$UI" | grep -q 'data-mtab="models" onclick="setMgmtTab('"'"'models'"'"')">所有模型'; check "2a 管理页模型标签改名「所有模型」" $? "tab not renamed"
echo "$UI" | grep -q '<header class="mgmt-top slim">'; check "2b 管理页头部改 slim 范式" $? "header not slim"
echo "$UI" | grep -q '<button class="back" onclick="navigate('"'"'chat'"'"')" aria-label="返回聊天"'; check "2b 管理页有 ‹ 回退键" $? "no back button"
# —— v0.19.23 需求：「所有模型」标签去掉「回到对话 / 模型服务商」两按钮（反向断言） ——
absent() { if echo "$UI" | grep -q "$1"; then return 1; else return 0; fi; }
absent '<button onclick="navigate('"'"'chat'"'"')">回到对话</button>'; check "2a-v0.19.23 「所有模型」已去掉「回到对话」" $? "chat btn still present"
absent '<button onclick="openResources()">模型服务商 →</button>'; check "2a-v0.19.23 「所有模型」已去掉「模型服务商」" $? "resources btn still present"
# —— 需求 3：附件缩略图入文本框 ——
# 注意：此处 $UI 是 812 行抓取的在线 HTML 内容（非路径），先落临时文件再解析
printf '%s' "$UI" > "$ROOT/.ui-snapshot.html"
ATT_IN=$(python3 - "$ROOT/.ui-snapshot.html" <<'PYEOF'
import re, sys
html = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'<div id="composer">(.*?)</div>\s*<input type="file"', html, re.S)
print("1" if (m and 'id="attach-bar"' in m.group(1) and 'class="composer-row"' in m.group(1)) else "0")
PYEOF
)
[ "$ATT_IN" = "1" ]; check "3 附件栏移入 #composer 内（按钮行包 composer-row）" $? "attach-bar not inside composer"
echo "$UI" | grep -q '\.att-chip\.att-pic { width: 42px; height: 42px;'; check "3 图片 chip 为 42×42 小方片" $? "no 42px pic chip"
echo "$UI" | grep -q '\.att-chip\.att-file {'; check "3 音频/文件 chip 为小 pill" $? "no file pill"
echo "$UI" | grep -q '#attach-bar { display: flex; flex-wrap: wrap;'; check "3 附件栏在框内左上部换行堆叠" $? "attach-bar not wrapping"
echo "$UI" | grep -q 'att-chip att-pic" data-st'; check "3 renderAttachments 输出小方片 markup" $? "no att-pic markup"
echo "$UI" | grep -q 'att-chip att-file" data-st'; check "3 renderAttachments 输出小 pill markup" $? "no att-file markup"

echo "== 26. v0.19.23 SKILL 管理：二级分类 + 台属性 + 分页 + 选择器趋同 =="
# —— 前端：分类/台常量与工具 ——
echo "$UI" | grep -q 'const SKILL_CATS = \["日常应用", "专业场景", "笔记整理", "模型整理"\];'; check "1 SKILL 二级分类四标签常量" $? "no SKILL_CATS"
echo "$UI" | grep -q 'const SKILL_TIER_SYSTEM = "system";'; check "1 台属性系统级常量" $? "no SKILL_TIER_SYSTEM"
echo "$UI" | grep -q 'function skillListable(s) { return skillTier(s) !== SKILL_TIER_SYSTEM; }'; check "1 系统级 skill 不罗列列表流的判定函数" $? "no skillListable"
echo "$UI" | grep -q 'function skCatBar(scope, cur)'; check "1 二级分类标签条渲染函数" $? "no skCatBar"
echo "$UI" | grep -q 'function setSkCat(scope, cat)'; check "1 二级分类切换函数（mp/mgmt 分流）" $? "no setSkCat"
# —— 前端：选择器 SKILL 子页趋同（分类过滤 + 独立页码）——
echo "$UI" | grep -q 'SKILLS.filter(s => skillListable(s) && skillCat(s) === cat)'; check "2 选择器 SKILL 列表按分类过滤且排除系统级" $? "picker no cat filter"
echo "$UI" | grep -q 'mpSkillPages\[cat\] = Math.min(Math.max(1, mpSkillPages\[cat\] || 1), pages);'; check "2 选择器 SKILL 各分类独立页码钳制" $? "no per-cat page clamp"
echo "$UI" | grep -q 'let mpSkillCat = SKILL_CATS\[0\];'; check "2 选择器 SKILL 当前分类状态" $? "no mpSkillCat"
echo "$UI" | grep -q 'if (mpTab === "skill") mpSkillPages\[mpSkillCurCat()\] = 1;'; check "2 搜索词变更重置当前分类页码" $? "search no cat reset"
# —— 前端：管理页 SKILL 管理（分页条 + 翻页函数）——
echo "$UI" | grep -q 'id="skill-pager" hidden'; check "3 管理页 SKILL 独立分页条容器" $? "no skill-pager"
echo "$UI" | grep -q 'function turnMgmtSkillPage(d)'; check "3 管理页 SKILL 翻页函数" $? "no turnMgmtSkillPage"
echo "$UI" | grep -q 'mgmtSkillPages\[cat\] = Math.min(Math.max(1, mgmtSkillPages\[cat\] || 1), pages);'; check "3 管理页各分类独立页码钳制" $? "no mgmt per-cat clamp"
echo "$UI" | grep -q 'let mgmtSkillCat = SKILL_CATS\[0\];'; check "3 管理页 SKILL 当前分类状态" $? "no mgmtSkillCat"
# —— 前端：对话框分类/台字段 + 提交 ——
echo "$UI" | grep -q '<select id="sk-category">'; check "4 skill 对话框分类下拉" $? "no sk-category"
echo "$UI" | grep -q '<select id="sk-tier">'; check "4 skill 对话框台下拉" $? "no sk-tier"
echo "$UI" | grep -q 'const body = { title, prompt, summary, enabled, category, tier };'; check "4 提交带 category/tier" $? "body missing fields"
# —— 前端：零迁移兜底 ——
echo "$UI" | grep -q 'if (!SKILL_CATS.includes(s.category)) s.category = SKILL_CATS\[0\];'; check "5 旧账本 category 内存兜底（不回写）" $? "no cat fallback"
echo "$UI" | grep -q 'if (s.tier !== SKILL_TIER_SYSTEM) s.tier = "normal";'; check "5 旧账本 tier 内存兜底" $? "no tier fallback"
# —— 后端：数据模型 + API 校验 ——
grep -q 'pub const SKILL_CATEGORIES' src/channels.rs; check "6 后端 SKILL_CATEGORIES 合法值表" $? "no SKILL_CATEGORIES"
grep -q 'pub fn default_category() -> String' src/channels.rs; check "6 后端缺省分类函数" $? "no default_category"
grep -q 'pub fn skill_tier_valid(tier: &str) -> bool' src/channels.rs; check "6 后端 tier 合法性函数" $? "no skill_tier_valid"
grep -q '#\[serde(default = "default_category")\]' src/channels.rs; check "6 Skill.category serde 缺省（零迁移）" $? "no category serde default"
grep -q '#\[serde(default = "default_tier")\]' src/channels.rs; check "6 Skill.tier serde 缺省（零迁移）" $? "no tier serde default"
grep -q '"invalid_category"' src/admin.rs; check "6 创建/更新 category 非法值 400" $? "no invalid_category"
grep -q '"invalid_tier"' src/admin.rs; check "6 创建/更新 tier 非法值 400" $? "no invalid_tier"
grep -q 'const category = SKILL_CATS.includes($("sk-category").value)' src/admin_ui.html; check "4 提交前分类白名单校验" $? "no client cat whitelist"

echo "== 27. v0.19.24 默认对话 skill 多选（≤3，每次发送必带）=="
# —— 多选存储与迁移 ——
echo "$UI" | grep -q 'const GW_DEFAULT_SKILLS_KEY = "gw_default_skills";'; check "1 多选默认 skill 存储 key" $? "no multi key"
echo "$UI" | grep -q 'const MAX_DEFAULT_SKILLS = 3;'; check "1 默认对话 skill 上限 3" $? "no cap 3"
echo "$UI" | grep -q 'function defaultSkillIds()'; check "1 多选读取函数" $? "no defaultSkillIds"
echo "$UI" | grep -q 'function saveDefaultSkillIds(ids)'; check "1 多选保存函数（去重 + 截断上限）" $? "no saveDefaultSkillIds"
echo "$UI" | grep -q 'function isDefaultSkill(id)'; check "1 单选判定改多选口径" $? "no isDefaultSkill"
echo "$UI" | grep -q 'const old = localStorage.getItem(GW_DEFAULT_SKILL_KEY) || "";'; check "1 旧单选 key 只读迁移" $? "no legacy migration"
# —— 管理页：计数行 + 切换按钮 + 上限禁用 ——
echo "$UI" | grep -q 'class="sk-def-count">默认对话 skill：'; check "2 管理页默认对话计数行" $? "no def count"
echo "$UI" | grep -q 'function toggleDefaultSkill(id)'; check "2 默认对话切换函数（加/减）" $? "no toggleDefaultSkill"
echo "$UI" | grep -q 'disabled title="默认对话最多 ' ; check "2 达上限时按钮禁用并提示" $? "no cap disable"
echo "$UI" | grep -q 'toast(`默认对话最多 ${MAX_DEFAULT_SKILLS} 个 —— 先取消一个再加`, "err")'; check "2 超上限 toast 提示" $? "no cap toast"
echo "$UI" | grep -q ' onclick="toggleDefaultSkill(' ; check "2 管理页按钮接切换函数" $? "btn not wired"
# —— 选择器徽标 ——
echo "$UI" | grep -q 'const isDef = isDefaultSkill(s.id);'; check "3 选择器徽标用多选判定" $? "picker badge not multi"
# —— 发送注入双流 ——
echo "$UI" | grep -q 'for (const id of defIds) {'; check "4 发送时遍历默认流注入" $? "no def loop"
echo "$UI" | grep -q 'if (activeSkill && activeSkill.prompt && !defIds.includes(activeSkill.id)) sysMsgs.push(activeSkill);'; check "4 手选流追加且与默认流去重" $? "no dedup"
echo "$UI" | grep -q 'history.unshift(...sysMsgs.map(s => ({ role: "system", content: s.prompt })));'; check "4 双流整体前置（保序）" $? "no ordered prefix"
echo "$UI" | grep -q 'if (sysMsgs.length) {'; check "4 无 skill 时不注入空 system" $? "no empty guard"

echo "== 28. v0.19.25 首页 skill 三流展示：系统级深度思考/智能搜索 + 默认流 + 一键不带上 =="
# —— 前端：系统级 skill 常量与提示词 ——
echo "$UI" | grep -q 'const SYS_THINK_ID = "sk-deep-think";'; check "1 系统级 skill id：深度思考" $? "no SYS_THINK_ID"
echo "$UI" | grep -q 'const SYS_SEARCH_ID = "sk-web-search";'; check "1 系统级 skill id：智能搜索" $? "no SYS_SEARCH_ID"
echo "$UI" | grep -q 'const SYS_SKILLS = \['; check "1 系统级 skill 常量表" $? "no SYS_SKILLS"
echo "$UI" | grep -q 'const DEEP_THINK_PROMPT = `'; check "1 深度思考提示词（推理保底）" $? "no DEEP_THINK_PROMPT"
echo "$UI" | grep -q 'const WEB_SEARCH_PROMPT = `'; check "1 智能搜索提示词（检索保底）" $? "no WEB_SEARCH_PROMPT"
echo "$UI" | grep -q 'const GW_SYS_THINK_KEY = "gw_sys_think";'; check "1 深度思考开关持久化 key" $? "no GW_SYS_THINK_KEY"
echo "$UI" | grep -q 'const GW_SKIP_DEFAULT_KEY = "gw_skip_default";'; check "1 一键不带上 key" $? "no GW_SKIP_DEFAULT_KEY"
# —— 前端：开关函数与三流渲染 ——
echo "$UI" | grep -q 'function sysSkillOn(which)'; check "1 系统级开关状态读取" $? "no sysSkillOn"
echo "$UI" | grep -q 'function toggleSysSkill(which)'; check "1 系统级开关切换函数" $? "no toggleSysSkill"
echo "$UI" | grep -q 'function skipDefault() { return localStorage.getItem(GW_SKIP_DEFAULT_KEY) === "1"; }'; check "1 暂停默认流判定" $? "no skipDefault"
echo "$UI" | grep -q 'function toggleSkipDefault()'; check "1 一键不带上切换函数" $? "no toggleSkipDefault"
echo "$UI" | grep -q 'function renderSkillFlows()'; check "1 三流展示渲染函数" $? "no renderSkillFlows"
# —— 前端：首页三流展示区 DOM ——
echo "$UI" | grep -q '<div id="skill-flows" class="skill-flows">'; check "2 三流展示区容器" $? "no skill-flows"
echo "$UI" | grep -q 'id="sf-think" onclick="toggleSysSkill('"'"'think'"'"')"'; check "2 深度思考图标开关按钮" $? "no sf-think"
echo "$UI" | grep -q 'id="sf-search" onclick="toggleSysSkill('"'"'search'"'"')"'; check "2 智能搜索图标开关按钮" $? "no sf-search"
echo "$UI" | grep -q '<span class="sf-group" id="sf-defaults"></span>'; check "2 默认流 chips 容器" $? "no sf-defaults"
echo "$UI" | grep -q 'id="sf-skip" onclick="toggleSkipDefault()"'; check "2 一键不带上开关按钮" $? "no sf-skip"
echo "$UI" | grep -q 'style="margin-bottom:0;margin-left:auto"'; check "2 手选 chip 归拢进三流区（靠右）" $? "chip not in flows"
echo "$UI" | grep -q '\.sf-sys\.on { border-color: var(--acc)'; check "2 系统级图标点亮态样式" $? "no sf-sys.on"
echo "$UI" | grep -q '\.sf-group\.paused { opacity: \.45; }'; check "2 默认流暂停变暗样式" $? "no paused style"
# —— 前端：补种 / 删除联动 / 刷新点 ——
echo "$UI" | grep -q 'category: "日常应用", tier: "system", enabled: true,'; check "3 系统级 skill 幂等补种（tier=system）" $? "no sys seed"
echo "$UI" | grep -q 'if (dIds.includes(id)) saveDefaultSkillIds(dIds.filter(x => x !== id));'; check "3 删除默认 skill 同步移出默认列表" $? "no delete cleanup"
echo "$UI" | grep -q 'renderSkillFlows();   // v0.19.25：首页三流展示随 skill 列表刷新（默认 chips 标题查找）'; check "3 loadSkills 末尾刷新三流展示" $? "no flows refresh in loadSkills"
echo "$UI" | grep -q 'renderSkillFlows();   // v0.19.25：首页默认流 chips 同步加减'; check "3 toggleDefaultSkill 后刷新三流展示" $? "no flows refresh in toggle"
# —— 前端：发送三流合并 + 请求头 ——
echo "$UI" | grep -q 'if (sysSkillOn("think")) { sysMsgs.push(SYS_SKILLS\[0\]); sysFeats.push("think"); }'; check "4 系统级深度思考流注入" $? "no think flow"
echo "$UI" | grep -q 'if (sysSkillOn("search")) { sysMsgs.push(SYS_SKILLS\[1\]); sysFeats.push("search"); }'; check "4 系统级智能搜索流注入" $? "no search flow"
echo "$UI" | grep -q 'const defIds = skipDefault() ? \[\] : defaultSkillIds();'; check "4 暂停时不注入默认流" $? "skip not honored"
echo "$UI" | grep -q '"x-gw-features": sysFeats.join(",")'; check "4 系统级特性请求头下发" $? "no x-gw-features"
echo "$UI" | grep -q 'history.unshift(...sysMsgs.map(s => ({ role: "system", content: s.prompt })));'; check "4 三流整体前置（保序）" $? "no ordered prefix"
# —— 后端：特性头解析 + 按渠道注入参数 ——
grep -q 'fn want_feature(features: &str, name: &str) -> bool' src/lib.rs; check "5 特性头解析纯函数" $? "no want_feature"
grep -q 'let gw_features = req' src/lib.rs; check "5 chat handler 读 x-gw-features 头" $? "no gw_features read"
grep -q 'let want_think = want_feature(&gw_features, "think");' src/lib.rs; check "5 解析 think 特性" $? "no want_think"
grep -q 'if is_zhipu_like(&ch.provider, &ch.base_url) {' src/lib.rs; check "5 按渠道（智谱系）注入" $? "no channel gate"
grep -q 'p\["thinking"\] = serde_json::json!({ "type": "enabled" });' src/lib.rs; check "5 注入 thinking 参数（推理模式）" $? "no thinking inject"
grep -q '{ "type": "web_search", "web_search": { "enable": true } }' src/lib.rs; check "5 注入 web_search 工具（联网搜索）" $? "no web_search inject"
grep -q 'fn gw_features_header_parsing' src/lib.rs; check "5 特性头解析单测" $? "no features unit test"

echo "== 29. v0.19.26 首页问答视图：用户右 AI 左分立两侧 =="
# —— 用户行：row-reverse（头像贴右、气泡靠右）——
echo "$UI" | grep -q '\.msg\.user { flex-direction: row-reverse; align-items: flex-start; }'; check "1 用户消息行反转为靠右" $? "user not row-reverse"
echo "$UI" | grep -q 'flex: 0 1 auto; max-width: 82%;'; check "1 用户气泡限宽 82%（不顶屏边）" $? "no 82% cap"
# —— 气泡尾角与强调条随右对齐联动 ——
echo "$UI" | grep -q 'border-radius: 14px 14px 4px 14px'; check "2 用户气泡尾角在右下" $? "no right-bottom tail"
echo "$UI" | grep -q 'box-shadow: inset -3px 0 0 var(--warn, #d19a2a);'; check "2 强调条移到气泡右侧" $? "emph still left"
# —— meta 行右对齐 ——
echo "$UI" | grep -q '\.msg\.user \.msg-meta { justify-content: flex-end; }'; check "3 用户时间戳靠右" $? "meta not flex-end"
# —— 反向断言：旧的左对齐形态已不存在 ——
absent '.msg.user { flex-direction: row; align-items: flex-start; }'; check "4 旧 row 布局已移除" $? "old row layout remains"
absent 'border-radius: 14px 14px 14px 4px; padding: 9px 30px 9px 13px'; check "4 旧左下尾角已移除" $? "old tail remains"
absent '.msg.user .msg-meta { justify-content: flex-start; }'; check "4 旧左对齐 meta 已移除" $? "old meta remains"
# —— 助手侧不受影响（仍左、内容可选中）——
echo "$UI" | grep -q '\.msg\.assistant \.avatar { background: linear-gradient(135deg, var(--acc), var(--acc2));'; check "5 助手头像样式保持" $? "assistant avatar changed"
absent '\.msg\.assistant { flex-direction: row-reverse'; check "5 助手未右对齐（仍左）" $? "assistant flipped"

echo
echo "结果: PASS=$PASS FAIL=$FAIL"
kill $MOCK_PID 2>/dev/null
stop_workerd
rm -f "$ROOT/workerd.retry.capnp" "$ROOT/workerd.retry2.capnp" "$ROOT/workerd.flaky.capnp" "$ROOT/.ui-snapshot.html"
[ "$FAIL" -eq 0 ]