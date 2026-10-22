#!/usr/bin/env python3
#!/usr/bin/env python3
"""G0 门禁验证：新 UI 壳 + GatewayAdapter 真实闭环（UI 全量重建 B0）
   - 子路径托管（/QuakeAI/）下 WASM 装载、health/channels 真实数据
   - 预置旧版 localStorage(gw_convos_v1) + IDB 旧会话 → 新 UI 无损读取/迁移
   - 经 gw.chat 走 WASM → mock 上游的真实流式一问一答（含落库）
   - 控制台/页面零错误
   用法：python3 scripts/g0-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）；node scripts/mock-zhipu.mjs
   一键：bash scripts/test-ui-g0.sh
"""
import json, sys
from playwright.sync_api import sync_playwright

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8932/QuakeAI/"
PASS, FAIL = 0, 0

def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1; print(f"  [PASS] {name}")
    else:
        FAIL += 1; print(f"  [FAIL] {name}" + (f" -- {detail}" if detail else ""))

with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])

    # ---------- Pass A：旧版 localStorage(gw_convos_v1) 迁移 + 真实对话闭环 ----------
    ctx = browser.new_context(service_workers="block")
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)

    # 预置旧版数据（模拟老用户浏览器）
    page.goto(BASE)  # 先同源再写存储
    page.evaluate("""() => {
      localStorage.setItem("gw_theme", "dark");
      localStorage.setItem("gw_convos_v1", JSON.stringify([{
        id: "legacy-convo-1", title: "旧版会话（迁移验证）", model: "glm-4-flash", updated: Date.now() - 86400000,
        msgs: [
          { role: "user", content: "旧版第一条提问", ts: Date.now() - 86400000 },
          { role: "assistant", content: "旧版第一条回答", ts: Date.now() - 86400000 }
        ]
      }]));
    }""")
    page.reload()
    page.wait_for_timeout(600)

    print("== A1 旧数据迁移与主题 ==")
    check("旧版 gw_convos_v1 已迁移（键被清除）",
          page.evaluate("() => !localStorage.getItem('gw_convos_v1') && !!localStorage.getItem('gw_convos_v1_migrated')"))
    check("迁移标记已写", page.evaluate("() => localStorage.getItem('gw_convos_v1_migrated') === '1'"))
    sidebar_txt = page.inner_text("#conversationList")
    check("侧栏出现迁移来的旧会话", "旧版会话（迁移验证）" in sidebar_txt, sidebar_txt[:80])
    check("旧主题 gw_theme=dark 生效", page.evaluate("() => document.documentElement.dataset.theme === 'dark'"))

    print("== A2 WASM 装载与真实网关数据 ==")
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    check("WASM 就绪（适配层单例）", True)
    health = page.evaluate("() => window.QuakeAdapters.gateway.health()")
    print("   health:", json.dumps(health, ensure_ascii=False)[:200])
    check("health 返回 JSON 对象", isinstance(health, dict) and len(health) > 0)

    # 登记一个指向 mock 的真实渠道
    created = page.evaluate("""async () => {
      const r = await window.QuakeAdapters.gateway.createChannel({
        provider: "mock", name: "G0 Mock", kind: "chat",
        base_url: "http://127.0.0.1:8788", api_key: "test-key-local",
        models: ["glm-4-flash"], tier: "T2", priority: 100, enabled: true, note: "g0"
      });
      return r;
    }""")
    print("   createChannel:", json.dumps(created, ensure_ascii=False)[:200])
    channels = page.evaluate("() => window.QuakeAdapters.gateway.channels()")
    check("渠道列表返回新建渠道", any(c.get("name") == "G0 Mock" for c in channels),
          json.dumps(channels, ensure_ascii=False)[:200])
    check("渠道 Key 只回掩码", all("*" in (c.get("api_key_masked") or "") or not c.get("api_key_masked") for c in channels))
    models = page.evaluate("() => window.QuakeAdapters.usableChatModels(window.QuakeAdapters.gateway && []) || []")
    models = page.evaluate("""async () => {
      const chs = await window.QuakeAdapters.gateway.channels();
      return window.QuakeAdapters.usableChatModels(chs);
    }""")
    check("可用模型口径含 glm-4-flash", any(m["name"] == "glm-4-flash" for m in models), json.dumps(models, ensure_ascii=False)[:200])

    # 首页/顶栏走真实用户路径：点「刷新状态」拉取新渠道后的网关数据
    page.click("#refreshHealth")
    page.wait_for_timeout(600)
    health_txt = page.inner_text("#healthGrid")
    check("首页网关状态卡渲染真实数据（1 个渠道可用）", "1" in health_txt and "渠道可用于对话" in health_txt, health_txt[:120])
    top_txt = page.inner_text("#gwTopLabel")
    check("顶栏状态显示可用模型数", "个可用模型" in top_txt, top_txt)

    print("== A3 真实对话闭环（gw.chat → WASM → mock 上游 SSE） ==")
    page.fill("#homePrompt", "G0 验证：你好")
    page.click("#homeSend")
    # setStreamingUI(true) 在 send() 内同步生效（首个 await 之前），点击后立即可见
    page.wait_for_selector("#chatStop", state="visible", timeout=3000)
    check("流式期间显示停止按钮", True)
    page.wait_for_function(
        """() => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes('WASM 流式透传验证');
        }""", timeout=15000)
    asst_txt = page.inner_text("#messages .message.assistant:last-child .message-text")
    check("助手消息含 mock 流式内容（你好）", "你好" in asst_txt, asst_txt[:80])
    check("助手消息含路由模型名", "来自智谱" in asst_txt, asst_txt[:80])
    page.wait_for_selector("#chatStop", state="hidden", timeout=10000)
    check("流结束后停止按钮隐藏", True)
    meta_txt = page.inner_text("#messages .message.assistant:last-child .msg-meta")
    check("消息元信息含渠道/首字耗时", "G0 Mock" in meta_txt or "首字" in meta_txt, meta_txt[:100])

    # 落库验证（IDB）
    persisted = page.evaluate("""async () => {
      const db = await new Promise((res, rej) => {
        const r = indexedDB.open("gw_convos", 1);
        r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
      });
      const msgs = await new Promise((res, rej) => {
        const r = db.transaction("msgs", "readonly").objectStore("msgs").getAll();
        r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
      });
      return msgs.map(m => ({ role: m.role, content: m.content||"", streaming: m.streaming, channel: m.channel||"" }));
    }""")
    contents = [m["content"] for m in persisted]
    check("IDB 落库：用户提问", any("G0 验证" in c for c in contents), str(contents)[:150])
    check("IDB 落库：助手完整回答", any("WASM 流式透传验证" in c for c in contents), str(contents)[:150])
    check("IDB 落库：流式标记已清除", all(not m["streaming"] for m in persisted))
    check("IDB 落库：渠道名随消息持久化", any(m["channel"] == "G0 Mock" for m in persisted))

    # 模型选择器（真实数据）
    page.click("#chatModelBtn")
    page.wait_for_timeout(200)
    modal_txt = page.inner_text("#modalBody")
    check("模型选择器列出真实可用模型", "glm-4-flash" in modal_txt, modal_txt[:100])
    page.click("#modalBody .gw-model-item[data-m='glm-4-flash']")
    page.wait_for_timeout(200)
    check("切换模型后按钮回显", page.inner_text("#chatModelName") == "glm-4-flash")
    check("模型选择持久化 gw_model", page.evaluate("() => localStorage.getItem('gw_model') === 'glm-4-flash'"))

    page.screenshot(path="/tmp/g0-home.png", full_page=False)
    check("控制台/页面零错误", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()

    # ---------- Pass B：旧 IDB 会话直读 + 中断消息标记 ----------
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
    print("== B 旧 IDB 会话直读 ==")
    check("侧栏出现 IDB 旧会话", "IDB 旧会话" in page2.inner_text("#conversationList"))
    page2.click("#conversationList .conversation-item")
    page2.wait_for_timeout(300)
    msg_txt = page2.inner_text("#messages")
    check("旧消息可读", "来自 IDB 的旧提问" in msg_txt, msg_txt[:100])
    check("中断流式消息被标记（生成中断）", "生成中断" in msg_txt, msg_txt[:150])
    check("B 控制台零错误", len(errors2) == 0, "; ".join(errors2[:3]))
    ctx2.close()
    browser.close()

print(f"\nG0 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
