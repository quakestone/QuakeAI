#!/usr/bin/env python3
#!/usr/bin/env python3
"""G2 门禁验证：UI 全量重建 B2——服务商与模型管理（F8–F18）
   覆盖：渠道 CRUD / Key 轮换（掩码拦截）/ 启停 / 优先级 / 连通测试三态 /
   模型元数据 / 预设预填 / 一键载入默认 / 一键导入内置通道（跳过已存在）/
   导出配置 / 批量体检（串行不崩）/ 用量对账（三表 + CSV）。
   用法：python3 scripts/g2-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）；node scripts/mock-zhipu.mjs
   一键：bash scripts/test-ui-g2.sh
"""
import json, os, sys
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8932/QuakeAI/"
ANSWER_MARK = "WASM 流式透传验证"
PASS, FAIL = 0, 0

def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1; print(f"  [PASS] {name}")
    else:
        FAIL += 1; print(f"  [FAIL] {name}" + (f" -- {detail}" if detail else ""))

def channels(page):
    return page.evaluate("() => (window.QuakeProviders.state.channels || []).map(c => c.id)")

def card(page, cid):
    return page.locator(f"#chRows .ch-card[data-id='{cid}']")

def send_and_wait(page, text):
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
    page.on("dialog", lambda d: d.accept())   # confirm（删除渠道）自动确认

    page.goto(BASE)
    page.wait_for_timeout(500)

    print("== A 渠道 CRUD：表单建渠道 → 列表 → 编辑 →（后段删） ==")
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    page.click(".nav-item[data-view='providers']")
    page.wait_for_selector("#chRows .ch-card, #chRows .empty", timeout=10000)
    page.wait_for_timeout(300)
    check("服务商视图打开（面包屑）", page.inner_text("#breadcrumb") == "服务商")
    check("统计行渲染", page.locator("#pvSummary .prov-stat").count() >= 5)

    page.click("#pvAdd")
    page.wait_for_timeout(200)
    check("渠道对话框打开", page.evaluate("() => document.getElementById('channel-dlg').open === true"))
    page.fill("#ch-provider", "g2test")
    page.fill("#ch-name", "G2 测试源")
    page.fill("#ch-base", "http://127.0.0.1:8788")
    page.select_option("#ch-kind", "chat")
    page.select_option("#ch-tier", "T2")
    page.fill("#ch-priority", "60")
    page.fill("#ch-key", "g2-secret-key")
    page.fill("#ch-note", "g2 门禁造数据")
    # 默认已有一行空模型行 → 填第一个模型
    page.locator("#ch-meta-rows .mm-row").first.locator(".mm-name").fill("glm-4-flash")
    page.click("#ch-save")
    page.wait_for_selector("#chRows .ch-card[data-id='g2test-chat-1']", timeout=8000)
    check("createChannel：列表出现 g2test-chat-1", True)
    check("渠道卡显示 provider 标签", "g2test" in card(page, "g2test-chat-1").inner_text())
    check("渠道卡 Key 非「缺 Key」", "缺 Key" not in card(page, "g2test-chat-1").inner_text())
    models0 = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return (c && c.models) || []; }")
    check("模型清单落库（models = [glm-4-flash]）", models0 == ["glm-4-flash"], str(models0))

    print("== B Key 轮换：掩码回显 + 前端掩码拦截 + 真实轮换 ==")
    card(page, "g2test-chat-1").locator("button[data-act='key']").click()
    page.wait_for_timeout(200)
    check("换 Key 对话框打开", page.evaluate("() => document.getElementById('key-dlg').open === true"))
    masked = page.inner_text("#key-current")
    check("当前 Key 以掩码展示（无明文）", masked != "未登记" and "g2-secret-key" != masked and masked != "—", masked)
    page.fill("#key-new", "5bc8...4bRY")
    page.click("#key-save")
    page.wait_for_timeout(200)
    check("掩码被前端拦截（对话框不关 + 错误提示）",
          page.evaluate("() => document.getElementById('key-dlg').open === true")
          and page.inner_text("#key-dlg-err") != "", page.inner_text("#key-dlg-err"))
    page.fill("#key-new", "g2-secret-key-2")
    page.click("#key-save")
    page.wait_for_timeout(600)
    check("轮换成功（对话框关闭）", page.evaluate("() => document.getElementById('key-dlg').open === false"))
    masked2 = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c ? c.api_key_masked : ''; }")
    check("掩码已更新", masked2 != masked and "g2-secret-key-2" != masked2, f"{masked} -> {masked2}")
    # 轮换回 mock 认可的有效 Key：连通测试（下段）才会拿到 200 → healthy
    card(page, "g2test-chat-1").locator("button[data-act='key']").click()
    page.wait_for_timeout(200)
    page.fill("#key-new", "test-key-local")
    page.click("#key-save")
    page.wait_for_timeout(600)
    masked3 = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c ? c.api_key_masked : ''; }")
    check("轮换回有效 Key（掩码再变）", masked3 != masked2 and "test-key-local" != masked3, f"{masked2} -> {masked3}")

    print("== C 启停 / 优先级（F10） ==")
    card(page, "g2test-chat-1").locator("button[data-act='toggle']").click()
    page.wait_for_timeout(600)
    check("停用生效（卡片禁用样式）", card(page, "g2test-chat-1").evaluate("el => el.classList.contains('disabled')"))
    check("停用后徽标文案", "停用" in card(page, "g2test-chat-1").inner_text())
    card(page, "g2test-chat-1").locator("button[data-act='toggle']").click()
    page.wait_for_timeout(600)
    check("重新启用", not card(page, "g2test-chat-1").evaluate("el => el.classList.contains('disabled')"))
    prio_before = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c ? c.priority : -1; }")
    card(page, "g2test-chat-1").locator("button[data-act='prio-up']").click()
    page.wait_for_timeout(600)
    prio_after = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c ? c.priority : -1; }")
    check("优先级 +10 即时生效", prio_after == prio_before + 10, f"{prio_before} -> {prio_after}")

    print("== D 连通测试（F11，mock /models 200 → healthy 回写） ==")
    card(page, "g2test-chat-1").locator("button[data-act='test']").click()
    page.wait_for_function(
        "() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c && c.health && c.health.status === 'healthy'; }",
        timeout=10000)
    page.wait_for_timeout(300)
    check("健康状态回写 healthy", "健康" in card(page, "g2test-chat-1").inner_text())

    print("== E 模型元数据编辑（F12） ==")
    card(page, "g2test-chat-1").locator("button[data-act='edit']").click()
    page.wait_for_timeout(200)
    check("编辑对话框打开且回填原值", page.input_value("#ch-provider") == "g2test")
    page.click("#ch-meta-add")
    page.locator("#ch-meta-rows .mm-row").nth(1).locator(".mm-name").fill("glm-4.7-flash")
    page.locator("#ch-meta-rows .mm-row").nth(1).locator(".mm-kind").select_option("text")
    page.click("#ch-save")
    page.wait_for_timeout(800)
    meta_n = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return c && c.models_meta ? c.models_meta.length : -1; }")
    check("models_meta 落库（2 行）", meta_n == 2, str(meta_n))
    names = page.evaluate("() => { const c = (window.QuakeProviders.state.channels||[]).find(x => x.id==='g2test-chat-1'); return (c.models_meta||[]).map(m => m.name); }")
    check("元数据含新模型", names == ["glm-4-flash", "glm-4.7-flash"], str(names))

    print("== F 预设快速添加（F13） ==")
    page.fill("#pvPresetSearch", "zhipu")
    page.wait_for_timeout(300)
    preset_btn = page.locator("#pvPresets button[data-preset]").first
    check("筛选后出现预设卡", preset_btn.count() >= 1)
    preset_btn.click()
    page.wait_for_timeout(300)
    check("预填进表单（provider）", "zhipu" in page.input_value("#ch-provider").lower())
    check("预填模型行非空", page.locator("#ch-meta-rows .mm-row").count() >= 1)
    page.click("#ch-cancel")
    page.wait_for_timeout(200)
    check("取消关闭对话框", page.evaluate("() => document.getElementById('channel-dlg').open === false"))

    print("== G 一键载入默认（F14，seed 幂等） ==")
    before_seed = len(channels(page))
    page.click("#pvSeed")
    page.wait_for_function(
        "(n) => (window.QuakeProviders.state.channels || []).length > n",
        arg=before_seed, timeout=10000)
    after_seed = len(channels(page))
    check("seed 后渠道数增加", after_seed > before_seed, f"{before_seed} -> {after_seed}")
    page.click("#pvSeed")
    page.wait_for_timeout(800)
    check("seed 幂等（重试不重复）", len(channels(page)) == after_seed, str(len(channels(page))))

    print("== H 一键导入内置通道（F15：已存在跳过 + 删后再导补回） ==")
    # seed 的默认渠道与内置文件同 id 命名空间（seed-{provider}-{kind}），首导应全部跳过
    page.click("#pvImport")
    page.wait_for_timeout(1500)
    imported = channels(page)
    bundled_ids = ["seed-zhipu-chat", "seed-zhipu-audio", "seed-siliconflow-chat",
                   "seed-openrouter-chat", "seed-groq-chat", "seed-nvidia-chat", "seed-deepseek-chat"]
    check("内置通道 id 全部在位（seed 已建 → 首导全跳过）",
          all(i in imported for i in bundled_ids), str(imported))
    check("首导数量不变（7 个全部命中跳过）", len(imported) == after_seed, f"{after_seed} -> {len(imported)}")
    # 删掉一个 → 再导应恰好补回这一个（验「新增」路径，且不影响其它）
    card(page, "seed-groq-chat").locator("button[data-act='del']").click()
    page.wait_for_selector("#chRows .ch-card[data-id='seed-groq-chat']", state="detached", timeout=8000)
    check("UI 删除 seed-groq-chat", "seed-groq-chat" not in channels(page))
    page.click("#pvImport")
    page.wait_for_selector("#chRows .ch-card[data-id='seed-groq-chat']", timeout=15000)
    check("再导补回被删的一个（数量 +1）", len(channels(page)) == len(imported), f"{len(imported)} -> {len(channels(page))}")
    page.click("#pvImport")
    page.wait_for_timeout(1200)
    check("重导全部跳过（数量不变）", len(channels(page)) == len(imported), f"{len(imported)} -> {len(channels(page))}")

    print("== I 导出配置（F16，Key 掩码） ==")
    with page.expect_download() as dl_info:
        page.click("#pvExport")
    dl = dl_info.value
    check("导出 JSON 文件", dl.suggested_filename.endswith(".json"), dl.suggested_filename)
    dl_path = "/tmp/g2-export.json"
    dl.save_as(dl_path)
    exported = json.load(open(dl_path, encoding="utf-8"))
    chans = exported.get("channels") or []
    check("导出结构 {exported_at, note, channels}",
          bool(exported.get("exported_at")) and bool(exported.get("note")) and len(chans) > 0)
    check("导出不含明文 Key（仅掩码）",
          all(c.get("api_key_masked") and "g2-secret-key-2" != c.get("api_key_masked") for c in chans))

    print("== J 批量体检（F17，串行 + 单点失败不崩） ==")
    page.click("#pvSelftest")
    page.wait_for_selector("#selftest-dlg[open]", timeout=5000)
    page.wait_for_function(
        "() => /可用 \\d+\\/\\d+/.test(document.getElementById('selftest-body').textContent || '')",
        timeout=30000)
    body_txt = page.inner_text("#selftest-body")
    check("体检产出汇总行", "可用" in body_txt and "/" in body_txt, body_txt[:120])
    check("mock 渠道模型探测为可用", "✅" in body_txt, body_txt[:200])
    page.click("#selftest-close")

    print("== K 用量与对账（F18：三表 + CSV） ==")
    # 先跑一轮真实对话产生用量记录（g2test 渠道有 Key）
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(300)
    send_and_wait(page, "G2 用量造数据提问")
    page.click(".nav-item[data-view='providers']")
    page.wait_for_timeout(600)
    page.wait_for_selector("#usageCards .usage-card", timeout=8000)
    check("用量统计卡 6 张", page.locator("#usageCards .usage-card").count() == 6)
    totals = page.evaluate("async () => { const d = await window.QuakeAdapters.gateway.usage(); return d.totals || {}; }")
    check("usage totals 请求数 ≥ 1", (totals.get("requests") or 0) >= 1, json.dumps(totals)[:160])
    check("三张表渲染（按源/按源×模型/按日）", page.locator("#usageTables .usage-cap").count() == 3)
    check("按源表有行（g2test/mock）", "g2test" in page.inner_text("#usageTables"))
    with page.expect_download() as dl2:
        page.click("#pvUsageExport")
    dl2v = dl2.value
    check("CSV 导出文件名", dl2v.suggested_filename.endswith(".csv"), dl2v.suggested_filename)
    csv_path = "/tmp/g2-usage.csv"
    dl2v.save_as(csv_path)
    csv_head = open(csv_path, encoding="utf-8-sig").readline()
    check("CSV 列名与旧 UI 一致",
          csv_head.strip() == "user_id,start_time_iso,end_time_iso,model,name,wallet_type,cost,currency", csv_head.strip())

    print("== L 删除渠道（CRUD 收尾） ==")
    card(page, "g2test-chat-1").locator("button[data-act='del']").click()
    page.wait_for_selector("#chRows .ch-card[data-id='g2test-chat-1']", state="detached", timeout=8000)
    check("删除后卡片消失", page.locator("#chRows .ch-card[data-id='g2test-chat-1']").count() == 0)

    page.screenshot(path="/tmp/g2-providers.png", full_page=False)
    check("A–L 控制台/页面零错误", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()
    browser.close()

print(f"\nG2 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
