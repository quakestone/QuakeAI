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
echo "$UI" | grep -q '一键载入国内默认'; check "管理页含 seed 按钮" $? "no seed button"

# 12.2 一键载入国内默认（智谱/硅基流动/智谱音频，均为 disabled 待登记）
SEED=$(curl -s -X POST http://127.0.0.1:8787/admin/api/seed)
echo "$SEED" | grep -q '"added":3'; check "seed 载入 3 个国内默认渠道" $? "$SEED"
echo "$SEED" | grep -q 'seed-zhipu-chat'; check "seed 含智谱主线" $? "$SEED"
echo "$SEED" | grep -q 'seed-siliconflow-chat'; check "seed 含硅基流动" $? "$SEED"

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
check "TTS 内容类型 audio/mpeg" $([ "$MT" = "audio/mpeg" ] && echo 0 || echo 1) "$CT"
grep -q '"op":"tts-received"' "$MOCK_LOG" 2>/dev/null; check "mock 收到 TTS 请求" $? "no tts-received in mock-received.log"

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

echo
echo "结果: PASS=$PASS FAIL=$FAIL"
kill $MOCK_PID 2>/dev/null
stop_workerd
rm -f "$ROOT/workerd.retry.capnp" "$ROOT/workerd.retry2.capnp" "$ROOT/workerd.flaky.capnp"
[ "$FAIL" -eq 0 ]