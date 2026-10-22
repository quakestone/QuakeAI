#!/usr/bin/env python3
#!/usr/bin/env python3
"""G1 门禁验证：UI 全量重建 B1——对话全量（F1–F7）
   覆盖：真实流式闭环 / skills 三流注入（system 消息 + x-gw-features）/
   附件（图片入列 → image_url parts → 气泡缩略图）/ 积分账本（sel/use/share）/
   上下文边界（divider + ctxStart 历史切片）/ 修改流 / 重试·重新生成 /
   魔法指令（/console /debug，全角斜杠兼容）/ 调试浮层 / 控制台面板 /
   导出 / 分享（链接 + 二维码 + share 积分）。
   用法：python3 scripts/g1-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）；node scripts/mock-zhipu.mjs
   一键：bash scripts/test-ui-g1.sh
"""
import base64, json, os, sys
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
MOCK_LOG = os.path.join(HERE, "..", "mock-received.log")
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8932/QuakeAI/"
ANSWER_MARK = "WASM 流式透传验证"
PASS, FAIL = 0, 0

def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1; print(f"  [PASS] {name}")
    else:
        FAIL += 1; print(f"  [FAIL] {name}" + (f" -- {detail}" if detail else ""))

def log_len():
    try:
        with open(MOCK_LOG, encoding="utf-8") as f:
            return sum(1 for _ in f)
    except FileNotFoundError:
        return 0

def chat_digests(since=0):
    """mock 收到的 chat 请求摘要列表（增量：since 行号起）"""
    out = []
    try:
        with open(MOCK_LOG, encoding="utf-8") as f:
            lines = f.readlines()
    except FileNotFoundError:
        return out
    for ln in lines[since:]:
        try:
            j = json.loads(ln)
        except Exception:
            continue
        if j.get("chatDigest"):
            out.append(j["chatDigest"])
    return out

def main_digest(digests):
    """定位**主聊天请求**的 digest——B3 起工作流（autoTitle/memory）会在主聊天
    完成后补发 LLM 请求（user 消息以「用户：」开头、内容为对话转录），
    直接取末条会命中它们。主请求的 user 消息直接是用户提问（或以 image_url parts 开头）。"""
    out = [d for d in digests
           if any(h.startswith("user:") and not h.startswith("user:用户：")
                  or "image_url" in h for h in (d.get("msgHeads") or []))]
    return out[-1] if out else (digests[-1] if digests else {})

# 1x1 透明 PNG（附件验证用）
PNG_B64 = ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
PNG_PATH = "/tmp/g1-att.png"
with open(PNG_PATH, "wb") as f:
    f.write(base64.b64decode(PNG_B64))

IDB_MSGS = """async () => {
  const db = await new Promise((res, rej) => {
    const r = indexedDB.open("gw_convos", 1);
    r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
  });
  return await new Promise((res, rej) => {
    const r = db.transaction("msgs", "readonly").objectStore("msgs").getAll();
    r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
  });
}"""

def points(page):
    return page.evaluate("() => JSON.parse(localStorage.getItem('gw_model_points_v1') || '{}')")

def send_and_wait(page, text):
    """发送并等待**整条流结束**（停止按钮隐藏 = finally 已跑：
    积分记账 / 落库 / meta 均完成），避免在流式中途断言竞态。"""
    page.fill("#chatPrompt", text)
    page.click("#chatSend")
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_selector("#chatStop", state="hidden", timeout=15000)

with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    ctx = browser.new_context(service_workers="block", accept_downloads=True)
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    # 分享走确定性路径：屏蔽 navigator.share，强制落级到应用内分享对话框
    page.add_init_script("() => { try { Object.defineProperty(navigator, 'share', { value: undefined }); } catch (e) {} }")

    page.goto(BASE)
    page.wait_for_timeout(500)

    print("== A 基础：WASM + 渠道 + 首轮真实闭环 ==")
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    check("WASM 就绪", True)
    created = page.evaluate("""async () => {
      const r = await window.QuakeAdapters.gateway.createChannel({
        provider: "mock-zhipu", name: "G1 Mock", kind: "chat",
        base_url: "http://127.0.0.1:8788", api_key: "test-key-local",
        models: ["glm-4-flash"], tier: "T2", priority: 100, enabled: true, note: "g1"
      });
      return r;
    }""")
    check("createChannel 成功", bool(created), json.dumps(created, ensure_ascii=False)[:160])
    page.click("#refreshHealth")
    page.wait_for_timeout(600)
    check("顶栏显示可用模型", "个可用模型" in page.inner_text("#gwTopLabel"))
    page.wait_for_function("() => window.QuakeSkills && window.QuakeSkills.state.list.length > 0", timeout=10000)
    check("skills 数据层装载（种子化）", True)
    page.wait_for_timeout(200)
    # 切到 AI 对话视图（B1 composer 工具区在此视图）
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(300)

    mark = log_len()
    send_and_wait(page, "G1 第一问")
    digests = chat_digests(mark)
    check("首轮请求到达 mock 上游", len(digests) >= 1, str(digests)[:200])
    pts = points(page)
    auto = (pts.get("models") or {}).get("__auto__") or {}
    check("积分：自动模式使用 +1（__auto__.use）", auto.get("use", 0) >= 1, json.dumps(pts)[:200])

    print("== B skills 三流：系统级注入（system 消息 + x-gw-features） ==")
    page.click("#skSysThink")
    page.wait_for_timeout(200)
    check("系统级「深度思考」开关点亮", page.get_attribute("#skSysThink", "data-on") == "1")
    check("系统级开关持久化 gw_sys_think", page.evaluate("() => localStorage.getItem('gw_sys_think') === '1'"))
    mark = log_len()
    send_and_wait(page, "G1 第二问（带深度思考）")
    digests = chat_digests(mark)
    d = main_digest(digests)
    check("system 消息注入深度思考提示词（全渠道保底通道）",
          any("深度思考模式" in h for h in (d.get("systemHeads") or [])), json.dumps(d, ensure_ascii=False)[:240])
    # x-gw-features 是网关内部消费的头（不转发上游）；其效果按渠道能力落地：
    # 智谱系渠道 → 请求体注入 thinking:{type:enabled}。mock 渠道注册为
    # mock-zhipu（is_zhipu_like 命中），故断言注入结果即证明头已达网关且被正确解析。
    check("x-gw-features 生效：智谱系渠道注入 thinking",
          (d.get("thinking") or {}).get("type") == "enabled", json.dumps(d, ensure_ascii=False)[:240])

    print("== C 附件：图片入列 → image_url parts → 气泡缩略图 ==")
    page.set_input_files("#fileInput", PNG_PATH)
    page.wait_for_timeout(400)
    check("附件条显示图片 chip", page.locator("#attachBar .att-chip.att-pic").count() == 1)
    check("附件计数显示 图片 1/5", "图片 1/5" in page.inner_text("#attCount"))
    mark = log_len()
    send_and_wait(page, "G1 第三问（带图）")
    digests = chat_digests(mark)
    d = main_digest(digests)
    heads = d.get("msgHeads") or []
    check("请求体末条为 [text,image_url] parts",
          bool(heads) and "image_url" in heads[-1], str(heads[-1:]))
    check("用户气泡渲染缩略图", page.locator("#messages .message.user .att-thumb").count() >= 1)
    check("附件计数条发送后清空", page.evaluate("() => document.getElementById('attCount').hidden === true"))

    print("== D 上下文边界 + 积分 sel：切模型 ==")
    page.click("#chatModelBtn")
    page.wait_for_timeout(200)
    page.click("#modalBody .gw-model-item[data-m='glm-4-flash']")
    page.wait_for_timeout(300)
    check("模型按钮回显 glm-4-flash", page.inner_text("#chatModelName") == "glm-4-flash")
    pts = points(page)
    sel = ((pts.get("models") or {}).get("glm-4-flash") or {}).get("sel", 0)
    check("积分：选择 +1（sel）", sel == 1, json.dumps(pts)[:200])
    check("消息列出现上下文边界 divider", "上下文已重置" in page.inner_text("#messages"))
    check("divider 文案含新模型名", "已切换到 glm-4-flash" in page.inner_text("#messages"))
    idb = page.evaluate(IDB_MSGS)
    check("divider 已落库", any(m.get("role") == "divider" for m in idb))

    print("== E ctxStart 生效：边界后历史只带新消息 ==")
    mark = log_len()
    send_and_wait(page, "G1 第四问（边界后）")
    digests = chat_digests(mark)
    d = main_digest(digests)
    heads = d.get("msgHeads") or []
    user_heads = [h for h in heads if h.startswith("user:")]
    check("边界后请求只带 1 条用户消息（旧历史被截断）", len(user_heads) == 1, str(heads))
    check("边界后请求不含边界前的提问", not any("第一问" in h or "第二问" in h or "第三问" in h for h in heads), str(heads))
    pts = points(page)
    use = ((pts.get("models") or {}).get("glm-4-flash") or {}).get("use", 0)
    check("积分：glm-4-flash 使用 +1", use >= 1, json.dumps(pts)[:200])

    print("== F 修改流：回填 → 编辑 → 发送替换 ==")
    page.locator("#messages .message.user").last.locator("button[data-action='edit']").click()
    page.wait_for_timeout(200)
    check("修改态提示条显示", page.evaluate("() => !document.getElementById('editChip').hidden"))
    check("输入框回填原内容", page.input_value("#chatPrompt") == "G1 第四问（边界后）")
    page.fill("#chatPrompt", "G1 第四问改（已修改）")
    page.click("#chatSend")
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_selector("#chatStop", state="hidden", timeout=15000)
    check("旧提问被替换（界面无原内容）", "G1 第四问（边界后）" not in page.inner_text("#messages"))
    check("新提问已上屏", "G1 第四问改（已修改）" in page.inner_text("#messages"))
    idb = page.evaluate(IDB_MSGS)
    contents = [m.get("content") or "" for m in idb]
    check("IDB 无重复提问（旧提问已删）", not any(c == "G1 第四问（边界后）" for c in contents))
    check("IDB 新提问已落库", any(c == "G1 第四问改（已修改）" for c in contents))
    check("修改态已撤销", page.evaluate("() => document.getElementById('editChip').hidden === true"))

    print("== G 重试 / 重新生成 ==")
    before = len(page.evaluate(IDB_MSGS))
    page.locator("#messages .message.assistant").last.locator("button[data-action='retry']").click()
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_selector("#chatStop", state="hidden", timeout=15000)
    after = page.evaluate(IDB_MSGS)
    check("重新生成后消息总数不变（截断重发，无重复）", len(after) == before, f"{before} -> {len(after)}")
    check("重新生成内容落库", any(ANSWER_MARK in (m.get("content") or "") for m in after))

    print("== H 魔法指令：/console 与 /debug（全角斜杠兼容） ==")
    n_msgs = page.locator("#messages .message").count()
    page.fill("#chatPrompt", "/console: zsj")
    page.click("#chatSend")
    page.wait_for_timeout(300)
    check("魔法指令不产生聊天气泡", page.locator("#messages .message").count() == n_msgs)
    check("顶栏出现 ⌗ 控制台按钮", page.evaluate("() => document.getElementById('btn-console').hidden === false"))
    page.click("#btn-console")
    page.wait_for_timeout(200)
    check("控制台对话框打开", page.evaluate("() => document.getElementById('console-dlg').open === true"))
    console_json = page.evaluate("() => document.getElementById('console-body').textContent")
    try:
        cs = json.loads(console_json)
        ok = isinstance(cs.get("channels"), dict) and cs["channels"].get("total", 0) >= 1
    except Exception:
        ok = False
    check("控制台状态含渠道统计", ok, console_json[:160])
    page.click("#console-close")
    page.fill("#chatPrompt", "／debug on")   # 全角斜杠（中文输入法易误触）
    page.click("#chatSend")
    page.wait_for_timeout(300)
    check("全角／debug on 生效：顶栏出现 🐞 按钮", page.evaluate("() => document.getElementById('btn-debug').hidden === false"))
    check("调试浮层自动弹出", page.evaluate("() => document.getElementById('debug-dlg').open === true"))
    page.evaluate("() => window.QuakeMagic.dbg('[g1]', '探针写入')")
    page.wait_for_timeout(200)
    check("dbg 日志写入页面内浮层", "[g1] 探针写入" in page.inner_text("#dbg-body"))
    page.click("#dbg-close")
    page.fill("#chatPrompt", "/debug off")
    page.click("#chatSend")
    page.wait_for_timeout(300)
    check("/debug off 后按钮隐藏", page.evaluate("() => document.getElementById('btn-debug').hidden === true"))
    page.fill("#chatPrompt", "/console: off")
    page.click("#chatSend")
    page.wait_for_timeout(300)
    check("/console: off 后按钮隐藏", page.evaluate("() => document.getElementById('btn-console').hidden === true"))
    check("gw_debug 开关已清除", page.evaluate("() => !localStorage.getItem('gw_debug')"))

    print("== I 导出 ==")
    with page.expect_download() as dl_info:
        page.click("#exportChat")
    dl = dl_info.value
    check("导出下载 Markdown 文件", dl.suggested_filename.endswith(".md"), dl.suggested_filename)

    print("== J 分享：对话框 + 二维码 + share 积分 ==")
    page.locator("#messages .message.assistant").last.locator("button[data-action='share']").click()
    page.wait_for_timeout(400)
    check("分享对话框打开（无 navigator.share 时落级）", "分享这条回答" in page.inner_text("#modalTitle"))
    share_url = page.input_value("#shareUrl")
    check("分享链接以 http 开头（压缩进 hash）", share_url.startswith("http"), share_url[:80])
    check("分享链接带 /s# 片段", "/s#" in share_url, share_url[:80])
    pts = points(page)
    share = ((pts.get("models") or {}).get("glm-4-flash") or {}).get("share", 0)
    check("积分：分享 +10（share）", share >= 1, json.dumps(pts)[:200])
    page.click("#shareQr")
    page.wait_for_timeout(300)
    check("二维码渲染（data:image/gif）",
          page.locator("#shareQrImg").get_attribute("src", timeout=3000).startswith("data:image/gif"))
    page.click("#closeModal")

    print("== K 对话选项：换模型重置上下文开关 ==")
    page.click("#chatMore")
    page.wait_for_timeout(200)
    check("默认开启换模型重置上下文", page.evaluate("() => localStorage.getItem('gw_ctx_reset') !== '0'"))
    page.click("#modalBody button[data-act='ctxreset']")
    page.wait_for_timeout(200)
    check("可关闭（ gw_ctx_reset=0 ）", page.evaluate("() => localStorage.getItem('gw_ctx_reset') === '0'"))
    page.click("#chatMore")
    page.wait_for_timeout(200)
    page.click("#modalBody button[data-act='ctxreset']")
    page.wait_for_timeout(200)
    check("可再开启", page.evaluate("() => localStorage.getItem('gw_ctx_reset') === '1'"))

    page.screenshot(path="/tmp/g1-chat.png", full_page=False)
    check("A–K 控制台/页面零错误", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()

    # ---------- Pass B：旧 IDB 会话直读 + 中断消息标记（G0 回归） ----------
    ctx2 = browser.new_context(service_workers="block")
    page2 = ctx2.new_page()
    errors2 = []
    page2.on("pageerror", lambda e: errors2.append(str(e)))
    page2.goto(BASE)
    page2.evaluate("""async () => {
      const db = await new Promise((res, rej) => {
        const r = indexedDB.open("gw_convos", 1);
        r.onupgradeneeded = (e) => {
          const d = e.target.result;
          if (!d.objectStoreNames.contains("convos")) {
            const cs = d.createObjectStore("convos", { keyPath: "id" });
            cs.createIndex("updated", "updated");
          }
          if (!d.objectStoreNames.contains("msgs")) {
            const ms = d.createObjectStore("msgs", { keyPath: ["convoId", "idx"] });
            ms.createIndex("by_convo", "convoId");
          }
        };
        r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
      });
      const tx = db.transaction(["convos", "msgs"], "readwrite");
      tx.objectStore("convos").put({ id: "old-idb-1", title: "IDB 旧会话", model: "glm-4-flash", updated: Date.now() });
      tx.objectStore("msgs").put({ convoId: "old-idb-1", idx: 0, role: "user", content: "来自 IDB 的旧提问", ts: Date.now() });
      tx.objectStore("msgs").put({ convoId: "old-idb-1", idx: 1, role: "assistant", content: "半截回答", streaming: true, ts: Date.now() });
      return new Promise(r => tx.oncomplete = r);
    }""")
    page2.reload()
    page2.wait_for_timeout(800)
    print("== B 旧 IDB 会话直读（回归） ==")
    check("侧栏出现 IDB 旧会话", "IDB 旧会话" in page2.inner_text("#conversationList"))
    page2.click("#conversationList .conversation-item")
    page2.wait_for_timeout(300)
    msg_txt = page2.inner_text("#messages")
    check("旧消息可读", "来自 IDB 的旧提问" in msg_txt, msg_txt[:100])
    check("中断流式消息被标记（生成中断）", "生成中断" in msg_txt, msg_txt[:150])
    check("B 控制台零错误", len(errors2) == 0, "; ".join(errors2[:3]))
    ctx2.close()
    browser.close()

print(f"\nG1 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
