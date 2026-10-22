#!/usr/bin/env python3
#!/usr/bin/env python3
"""G4 门禁验证：UI 全量重建 B4——上游转发 / 接入方式 / 管理面鉴权 / 本设备 Key / 部署自检 / 网络监视
   覆盖：
   A 上游转发代理（F25）：代理地址保存/持久化/状态展示；配代理后对话经 /proxy 转发、
     x-gw-target 带真实上游地址、回答含 ANSWER_MARK（海外渠道经代理可用）；清空恢复直连态
   B 接入方式（F26）：curl 示例含 /v1/chat/completions 与 /v1/audio/transcriptions；
     观测响应头 x-gateway-channel / x-gateway-attempts 与四类自定义头说明
   C 管理面鉴权（F27）：UI 设口令持久化 + 掩码展示；裸 WASM 实例无 x-admin-token → 401、
     带头 → 200（软锁端到端）；清空后恢复不鉴权
   D 本设备 Key（F19）：provider 行渲染；录入 → gw_device_keys JSON + 已登记状态；
     掩码拦截；端到端——无共享 Key 的音频渠道 + 本设备 Key → STT 转写成功且
     mock 收到 Bearer <设备 Key>（证明 x-device-keys 被 WASM 真实消费）；清除
   E 部署自检（F28）：GET /diag JSON 渲染（version/store/expected_binding/env）
   F 网络监视（F29）：net-bar 四态——route 拦截 /cdn-cgi/trace → ok（含 ms 与国家）；
     offline 事件 → offline + 缓存展示；online 事件恢复；gw_net_last 持久化
   用法：python3 scripts/g4-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）；node scripts/mock-zhipu.mjs
   一键：bash scripts/test-ui-g4.sh
"""
import json, os, sys
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
MOCK_LOG = os.path.join(HERE, "..", "mock-received.log")
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8932/QuakeAI/"
ANSWER_MARK = "WASM 流式透传验证"
PROXY_URL = "http://127.0.0.1:8788/proxy"
REAL_TARGET = "http://127.0.0.1:8788/chat/completions"
PASS, FAIL = 0, 0
# netmon 探测路径：静态托管下 /cdn-cgi/trace 与 /health 必然 404——这是插件①→②
# 链式兜底的设计内行为（探测失败静默降级），不算应用错误。其余任何 404/错误都算。
PROBE_SUFFIX = ("/cdn-cgi/trace", "/health")
probe_404s, other_404s = [], []

def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1; print(f"  [PASS] {name}")
    else:
        FAIL += 1; print(f"  [FAIL] {name}" + (f" -- {detail}" if detail else ""))

def log_entries(pred):
    """mock 日志里满足 pred(j) 的条目列表"""
    out = []
    try:
        with open(MOCK_LOG, encoding="utf-8") as f:
            for ln in f:
                try:
                    j = json.loads(ln)
                except Exception:
                    continue
                if pred(j):
                    out.append(j)
    except FileNotFoundError:
        pass
    return out

def proxy_entries():
    """经 /proxy 转发的请求（配了 gw_upstream_proxy 后网关才发到这里）"""
    return log_entries(lambda j: j.get("proxyReceived") is True)

def multipart_entries():
    return log_entries(lambda j: j.get("op") == "multipart-received")

def send_and_wait(page, text):
    page.fill("#chatPrompt", text)
    page.click("#chatSend")
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_selector("#chatStop", state="hidden", timeout=15000)

def setup_channels(page):
    """经管理面 API 造两个渠道：
       - g4chat：带 Key 的聊天渠道（配代理后的对话链）
       - g4audio：不带 Key 的音频渠道（本设备 Key 端到端：无共享 Key 时用设备 Key）"""
    return page.evaluate("""async () => {
      const gw = window.QuakeAdapters.gateway;
      const post = (b) => gw.admin('/channels', { method: 'POST', body: b });
      await post({ provider: 'g4chat', name: 'G4 文本源', base_url: 'http://127.0.0.1:8788',
        kind: 'chat', tier: 'T1', priority: 90, api_key: 'test-key-local',
        models_meta: [{ name: 'glm-4-flash', kind: 'text', enabled: true, priority: 50 }] });
      await post({ provider: 'g4audio', name: 'G4 音频源', base_url: 'http://127.0.0.1:8788',
        kind: 'audio', tier: 'T1', priority: 80, api_key: '',
        models_meta: [{ name: 'glm-4-voice', kind: 'stt', enabled: true, priority: 50 }] });
      return true;
    }""")

with sync_playwright() as p:
    browser = p.chromium.launch(args=[
        "--no-sandbox",
        "--use-fake-device-for-media-stream",   # 假麦克风：MediaRecorder 录音可用
        "--use-fake-ui-for-media-stream",       # 自动授予媒体权限
    ])
    ctx = browser.new_context(service_workers="block", accept_downloads=True)
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text)
            if m.type == "error" and not ((m.location or {}).get("url") or "").endswith(PROBE_SUFFIX)
            else None)
    # 404 双轨记账：探测路径的 404 是设计内兜底；任何其他资源 404 都是真缺陷
    def on_response(resp):
        if resp.status != 404:
            return
        (probe_404s if resp.url.endswith(PROBE_SUFFIX) else other_404s).append(resp.url)
    page.on("response", on_response)
    page.on("dialog", lambda d: d.accept())

    page.goto(BASE)
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    page.wait_for_timeout(500)
    setup_channels(page)
    page.evaluate("() => window.QuakeProviders.load()")
    # 渠道是经管理面 API 直接造的（绕过 UI onChange），手动刷新模型清单（同 G3）
    page.evaluate("() => window.QuakeApp.refreshGateway()")
    page.wait_for_timeout(1000)

    # ================================================================
    print("== A 上游转发代理（F25） ==")
    page.click(".nav-item[data-view='upstream']")
    page.wait_for_timeout(600)
    check("上游视图打开（面包屑）", page.inner_text("#breadcrumb") == "上游与接入")

    # A1 保存代理地址 → 持久化 + 状态展示
    page.fill("#upProxy", PROXY_URL)
    page.click("#upProxySave")
    page.wait_for_timeout(400)
    check("gw_upstream_proxy 持久化",
          page.evaluate("() => localStorage.getItem('gw_upstream_proxy')") == PROXY_URL)
    check("代理状态展示「已配置」", "已配置" in page.inner_text("#upProxyState"))
    check("保存提示含 x-gw-target 约定", "x-gw-target" in page.inner_text("#upProxyHint"))
    # 首页网关能力卡联动（B4-3 装配）
    page.evaluate("() => window.QuakeApp.refreshGateway()")
    page.wait_for_timeout(600)
    check("首页能力卡显示代理已配置", "已配置" in page.inner_text("#healthGrid"))

    # A2 配代理后对话：经 /proxy 转发，x-gw-target = 真实上游
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    send_and_wait(page, "G4 代理转发提问")
    page.wait_for_timeout(600)
    pes = proxy_entries()
    check("mock 的 /proxy 收到转发请求", len(pes) > 0, str(len(pes)))
    if pes:
        last = pes[-1]
        check("x-gw-target 为真实上游地址", last.get("gwTarget") == REAL_TARGET, str(last.get("gwTarget")))
        dg = last.get("chatDigest") or {}
        check("转发请求体带对模型与提问",
              dg.get("model") == "glm-4-flash"
              and any(h.startswith("user:G4 代理转发提问") for h in (dg.get("msgHeads") or [])),
              json.dumps(dg)[:160])
    # 海外渠道经代理可用：回答含透传标记（mock /proxy 与 /chat/completions 同一处理器）
    last_msg = page.locator("#messages .message.assistant .message-text").last.inner_text()
    check("经代理的回答完整透传（ANSWER_MARK）", ANSWER_MARK in last_msg, last_msg[:80])

    # A3 清空 → 恢复直连态
    page.click(".nav-item[data-view='upstream']")
    page.wait_for_timeout(500)
    page.fill("#upProxy", "")
    page.click("#upProxySave")
    page.wait_for_timeout(400)
    check("清空后键移除", page.evaluate("() => localStorage.getItem('gw_upstream_proxy')") is None)
    check("清空后状态展示「未配置」", "未配置" in page.inner_text("#upProxyState"))
    check("未配代理时发送失败引导返回 true",
          page.evaluate("() => window.QuakeUpstream.hintIfProxyMissing()") is True)

    # ================================================================
    print("== B 接入方式（F26） ==")
    doc = page.inner_text("#upstreamView")
    check("curl 示例含 /v1/chat/completions", "/v1/chat/completions" in doc)
    check("curl 示例含 /v1/audio/transcriptions", "/v1/audio/transcriptions" in doc)
    check("观测头说明 x-gateway-channel", "x-gateway-channel" in doc)
    check("观测头说明 x-gateway-attempts", "x-gateway-attempts" in doc)
    check("自定义头约定四类齐全",
          all(h in doc for h in ("x-gw-target", "x-gw-features", "x-device-keys", "x-admin-token")))

    # ================================================================
    print("== C 管理面鉴权软锁（F27） ==")
    page.fill("#upToken", "g4-secret")
    page.click("#upTokenSave")
    page.wait_for_timeout(400)
    check("gw_admin_token 持久化",
          page.evaluate("() => localStorage.getItem('gw_admin_token')") == "g4-secret")
    check("口令状态掩码展示", "已设置" in page.inner_text("#upTokenState") and "***" in page.inner_text("#upTokenState"))

    # 软锁端到端：同一 WASM 实例，裸构造请求（UI 适配层会自动带头，这里手动不发）
    lock = page.evaluate("""async () => {
      const gw = await window.QuakeAdapters.gateway.init();
      const mk = (tok) => new Request("/admin/api/channels", { method: "GET",
        headers: Object.assign({ "x-gw-sub": "/api/channels" }, tok ? { "x-admin-token": tok } : {}) });
      const noTok = await gw.admin(mk(null));
      const badTok = await gw.admin(mk("wrong-token"));
      const withTok = await gw.admin(mk("g4-secret"));
      return { noTok: noTok.status, badTok: badTok.status, withTok: withTok.status };
    }""")
    check("缺 x-admin-token → 401（软锁生效）", lock.get("noTok") == 401, str(lock))
    check("错误口令 → 401", lock.get("badTok") == 401, str(lock))
    check("正确口令 → 200（放行）", lock.get("withTok") == 200, str(lock))
    # 适配层带头时 UI 侧管理请求不受锁影响（G4 造渠道/读账本都走此路径）
    check("适配层自动带头可读渠道账本",
          page.evaluate("() => window.QuakeAdapters.gateway.channels().then(a => a.length)") >= 2)

    # 清空口令 → 软锁释放
    page.fill("#upToken", "")
    page.click("#upTokenSave")
    page.wait_for_timeout(400)
    check("清空后键移除", page.evaluate("() => localStorage.getItem('gw_admin_token')") is None)
    unlocked = page.evaluate("""async () => {
      const gw = await window.QuakeAdapters.gateway.init();
      const req = new Request("/admin/api/channels", { method: "GET", headers: { "x-gw-sub": "/api/channels" } });
      return (await gw.admin(req)).status;
    }""")
    check("清空口令后无头请求放行（200）", unlocked == 200, str(unlocked))

    # ================================================================
    print("== D 本设备 Key（F19） ==")
    check("provider 行来自渠道账本（g4audio）",
          page.locator("#upDevKeyRows .dk-row[data-prov='g4audio']").count() == 1)
    row = "#upDevKeyRows .dk-row[data-prov='g4audio']"
    page.fill(row + " .dk-in", "test-key-local")
    page.locator(row + " button[data-act='save']").click()
    page.wait_for_timeout(400)
    check("gw_device_keys JSON 持久化",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_device_keys')||'{}')") == {"g4audio": "test-key-local"})
    check("行状态「已登记」+ 掩码", "已登记" in page.locator(row + " .dk-st").inner_text())
    # 掩码拦截
    page.fill(row + " .dk-in", "abcd***wxyz")
    page.locator(row + " button[data-act='save']").click()
    page.wait_for_timeout(400)
    check("掩码被拦截（不写入）",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_device_keys')||'{}')") == {"g4audio": "test-key-local"})
    check("掩码拦截有 toast 提示", "掩码" in page.inner_text("#toastRegion"))
    # 端到端：无共享 Key 的音频渠道 + 本设备 Key → STT 转写，mock 收到设备 Key
    page.click(".nav-item[data-view='voice']")
    page.wait_for_timeout(800)
    page.select_option("#vc-stt-mode", "gateway")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    check("STT 下拉含迁移后的 glm-asr-2512",
          page.locator("#vc-stt-model option", has_text="glm-asr-2512").count() == 1)
    mm_before = len(multipart_entries())
    page.click("#vcTestStt")
    page.wait_for_function(
        "() => /转写结果/.test(document.getElementById('vcHint').textContent || '')",
        timeout=25000)
    hint = page.inner_text("#vcHint")
    check("无共享 Key 音频渠道转写成功（设备 Key 兜底）", "mock-transcription" in hint, hint[:100])
    mms = multipart_entries()
    check("mock 收到 multipart 上传", len(mms) > mm_before, f"{mm_before} -> {len(mms)}")
    if len(mms) > mm_before:
        check("上游鉴权用的是本设备 Key（x-device-keys 被 WASM 消费）",
              mms[-1].get("authorization") == "Bearer test-key-local", str(mms[-1].get("authorization")))
    # 清除设备 Key
    page.click(".nav-item[data-view='upstream']")
    page.wait_for_timeout(500)
    page.locator(row + " button[data-act='clear']").click()
    page.wait_for_timeout(400)
    check("清除后 JSON 移除该 provider",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_device_keys')||'{}')") == {})
    check("清除后行状态回「未登记」", "未登记" in page.locator(row + " .dk-st").inner_text())

    # ================================================================
    print("== E 部署自检（F28） ==")
    page.click("#upDiagRun")
    page.wait_for_function(
        "() => { const t = document.getElementById('upDiagBody').textContent || ''; return t.includes('version'); }",
        timeout=10000)
    diag = json.loads(page.inner_text("#upDiagBody"))
    check("diag 含 version/store/expected_binding/env",
          all(k in diag for k in ("version", "store", "expected_binding", "env")), json.dumps(diag)[:120])
    check("diag store 态 ok 且渠道数 ≥2",
          diag.get("store", {}).get("ok") is True and (diag.get("store", {}).get("channels") or 0) >= 2,
          json.dumps(diag.get("store"))[:120])
    check("diag env 反映未配管理口令", diag.get("env", {}).get("has_admin_token") is False)

    # ================================================================
    print("== F 网络监视四态（F29） ==")
    check("net-bar 常驻（固定底部）",
          page.evaluate("() => { const b = document.getElementById('net-bar'); return !!b && getComputedStyle(b).position === 'fixed'; }"))
    # route 拦截 /cdn-cgi/trace：模拟 CF 边缘 trace（插件①路径）
    page.route("**/cdn-cgi/trace", lambda route: route.fulfill(
        status=200, content_type="text/plain",
        body="fl=abc123\nh=127.0.0.1\nip=1.2.3.4\ncolo=SJC\nloc=US\ntls=TLSv1.3\n"))
    page.evaluate("() => window.QuakeNetMon.probe('manual')")
    page.wait_for_function(
        "() => document.getElementById('net-bar').dataset.st === 'ok'", timeout=8000)
    nb = page.inner_text("#nb-txt")
    check("ok 态：延迟 ms + 协议 + 国家（trace 解析）",
          "ms" in nb and "HTTP" in nb and "美国" in nb, nb)
    check("探测结论 toast（手动）", "网络正常" in page.inner_text("#toastRegion"))
    check("gw_net_last 持久化",
          page.evaluate("() => { const v = JSON.parse(localStorage.getItem('gw_net_last')||'null'); return !!v && v.country === '美国'; }"))

    # offline 事件 → offline 态 + 缓存展示
    page.evaluate("() => window.dispatchEvent(new Event('offline'))")
    page.wait_for_timeout(300)
    check("offline 事件 → data-st=offline",
          page.evaluate("() => document.getElementById('net-bar').dataset.st") == "offline")
    check("离线时展示缓存快照（含国家）", "缓存" in page.inner_text("#nb-txt"), page.inner_text("#nb-txt"))

    # online 事件 → 恢复 ok
    page.evaluate("() => window.dispatchEvent(new Event('online'))")
    page.wait_for_function(
        "() => document.getElementById('net-bar').dataset.st === 'ok'", timeout=8000)
    check("online 事件 → 恢复 ok 态", True)

    # ================================================================
    print("== G 收尾 ==")
    page.screenshot(path="/tmp/g4-upstream.png", full_page=False)
    check("boot 探测链式兜底真实发生（trace 404 → health 兜底）",
          any(u.endswith("/cdn-cgi/trace") for u in probe_404s)
          and any(u.endswith("/health") for u in probe_404s), str(probe_404s))
    check("无非探测路径的资源 404（无缺失文件）", len(other_404s) == 0, "; ".join(other_404s[:3]))
    check("A–F 控制台/页面零错误（探测路径 404 除外）", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()
    browser.close()

print(f"\nG4 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
