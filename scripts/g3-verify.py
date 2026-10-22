#!/usr/bin/env python3
#!/usr/bin/env python3
"""G3 门禁验证：UI 全量重建 B3——提示词 + 语音多媒体 + 工作流（F20–F24）
   覆盖：
   A 提示词 CRUD（F20）：对话框新建/编辑/启停/设默认（≤3 联动对话页三流条）/
     二级分类切换/每类分页/使用计数/删除联动清理
   B TTS 三级递降链（F21）：链路状态/测试播报走网关（mock /audio/speech）/
     消息播音按钮/仅浏览器模式/自定义端点（第二级真实命中）/持久化
   C STT 两级递降链（F22）：链路状态/网关模式测试转写（fake mic → mock
     transcriptions）/Jarvis 浮层文字入口 + 回答 TTS 播报
   D 视觉/视频模型范围（F23）：能力下拉来自渠道账本/带附件选路覆盖/
     always 范围（仅自动模式）/显式选模型优先/mmScope 持久化
   E 工作流参数（F24）：保存 → CSS 变量实时生效/gw_wf_cfg 持久化/
     auto_title 真实跑（LLM 总结标题）/memory 压缩真实跑（LLM）/usage 开关
   用法：python3 scripts/g3-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）；node scripts/mock-zhipu.mjs
   一键：bash scripts/test-ui-g3.sh
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

def log_ops(op):
    """mock 日志里指定 op 的条目列表"""
    out = []
    try:
        with open(MOCK_LOG, encoding="utf-8") as f:
            for ln in f:
                try:
                    j = json.loads(ln)
                except Exception:
                    continue
                if j.get("op") == op:
                    out.append(j)
    except FileNotFoundError:
        pass
    return out

def chat_digests():
    out = []
    try:
        with open(MOCK_LOG, encoding="utf-8") as f:
            for ln in f:
                try:
                    j = json.loads(ln)
                except Exception:
                    continue
                if j.get("chatDigest"):
                    out.append(j["chatDigest"])
    except FileNotFoundError:
        pass
    return out

def find_chat_digest(marker=None, want_image=False):
    """定位**主聊天请求**的 digest——排除工作流 autoTitle/memory 在主聊天完成后
    补发的后续 LLM 请求（其 user 消息以「用户：」开头、且绝不带 image_url）。
    marker：主请求 user 文本消息须以该串开头；want_image：主请求须带图片附件。"""
    out = []
    for d in chat_digests():
        heads = d.get("msgHeads") or []
        if want_image and not any("image_url" in h for h in heads):
            continue
        if marker and not any(h.startswith("user:" + marker) for h in heads):
            continue
        out.append(d)
    return out[-1] if out else None

# 1x1 透明 PNG（F23 视觉选路验证用）
PNG_B64 = ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5Erkg==")
PNG_PATH = "/tmp/g3-att.png"
with open(PNG_PATH, "wb") as f:
    f.write(base64.b64decode(PNG_B64))

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
    """经管理面 API 造三个渠道（UI CRUD 由 G2 覆盖，此处求稳求快）"""
    return page.evaluate("""async () => {
      const gw = window.QuakeAdapters.gateway;
      const post = (b) => gw.admin('/channels', { method: 'POST', body: b });
      await post({ provider: 'g3chat', name: 'G3 文本源', base_url: 'http://127.0.0.1:8788',
        kind: 'chat', tier: 'T1', priority: 90, api_key: 'test-key-local',
        models_meta: [{ name: 'glm-4-flash', kind: 'text', enabled: true, priority: 50 }] });
      await post({ provider: 'g3audio', name: 'G3 音频源', base_url: 'http://127.0.0.1:8788',
        kind: 'audio', tier: 'T1', priority: 80, api_key: 'test-key-local',
        models_meta: [{ name: 'glm-4-voice', kind: 'stt', enabled: true, priority: 50 }] });
      await post({ provider: 'g3vision', name: 'G3 视觉源', base_url: 'http://127.0.0.1:8788',
        kind: 'chat', tier: 'T1', priority: 70, api_key: 'test-key-local',
        models_meta: [{ name: 'glm-4v-flash', kind: 'vision', enabled: true, priority: 50 }] });
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
    # netmon 探测路径（/cdn-cgi/trace、/health）在静态托管下必然 404——插件①→②
    # 链式兜底的设计内行为（探测失败静默降级），不算应用错误
    page.on("console", lambda m: errors.append(m.text)
            if m.type == "error" and not ((m.location or {}).get("url") or "").endswith(("/cdn-cgi/trace", "/health"))
            else None)
    page.on("dialog", lambda d: d.accept())   # confirm（删除提示词）自动确认

    page.goto(BASE)
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    page.wait_for_timeout(500)
    setup_channels(page)
    page.evaluate("() => window.QuakeProviders.load()")
    # 渠道是经管理面 API 直接造的（绕过了服务商页 UI 的 onChange 联动），
    # 手动触发一次网关刷新，把模型选择器清单（state.models）同步到新账本
    page.evaluate("() => window.QuakeApp.refreshGateway()")
    page.wait_for_timeout(1000)

    # ================================================================
    print("== A 提示词 CRUD（F20） ==")
    page.click(".nav-item[data-view='prompts']")
    page.wait_for_selector("#prRows .sk-row, #prRows .empty-state", timeout=10000)
    page.wait_for_timeout(400)
    check("提示词视图打开（面包屑）", page.inner_text("#breadcrumb") == "提示词")
    check("统计行 5 项", page.locator("#prSummary .prov-stat").count() == 5)
    check("二级分类栏 4 类", page.locator("#prCatBar .sk-cat").count() == 4)
    check("默认对话徽标 0/3", "0/3" in page.inner_text(".sk-def-badge"))

    # A1 对话框新建
    page.click("#prAdd")
    page.wait_for_timeout(200)
    check("提示词对话框打开", page.evaluate("() => document.getElementById('skill-dlg').open === true"))
    page.fill("#sk-title", "G3 测试提示词")
    page.fill("#sk-prompt", "你是 G3 门禁测试助手。回答以「G3OK」开头。")
    page.fill("#sk-summary", "门禁造数据")
    page.click("#sk-save")
    # 等「含新建标题」的那一行落桌再取其 data-id——种子条目可能先于新建行渲染，
    # 直接取首行会拿到别人的 id（首跑竞态）
    page.wait_for_selector("#prRows .sk-row:has-text('G3 测试提示词')", timeout=8000)
    new_id = page.evaluate("() => { const r = [...document.querySelectorAll('#prRows .sk-row')].find(x => x.textContent.includes('G3 测试提示词')); return r ? r.dataset.id : ''; }")
    check("新建后列表出现（存储端生成 id）", new_id.startswith("sk-"), new_id)
    check("简介按手填展示", "门禁造数据" in page.inner_text("#prRows"))

    # A2 编辑改名
    page.locator(f"#prRows .sk-row[data-id='{new_id}'] button[data-act='edit']").click()
    page.wait_for_timeout(200)
    check("编辑对话框回填标题", page.input_value("#sk-title") == "G3 测试提示词")
    page.fill("#sk-title", "G3 测试提示词·改")
    page.click("#sk-save")
    page.wait_for_timeout(800)
    check("编辑改名生效", "G3 测试提示词·改" in page.inner_text("#prRows"))

    # A3 启停
    page.locator(f"#prRows .sk-row[data-id='{new_id}'] button[data-act='toggle']").click()
    page.wait_for_timeout(800)
    check("停用徽标出现", "已停用" in page.locator(f"#prRows .sk-row[data-id='{new_id}']").inner_text())
    page.locator(f"#prRows .sk-row[data-id='{new_id}'] button[data-act='toggle']").click()
    page.wait_for_timeout(800)
    check("重新启用", "已停用" not in page.locator(f"#prRows .sk-row[data-id='{new_id}']").inner_text())

    # A4 设默认 + 对话页三流条联动
    page.locator(f"#prRows .sk-row[data-id='{new_id}'] button[data-act='def']").click()
    page.wait_for_timeout(500)
    check("默认徽标 1/3", "1/3" in page.inner_text(".sk-def-badge"))
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    check("对话页默认流 chip 联动", "G3 测试提示词·改" in page.inner_text("#skillDefaultList"))
    page.click(".nav-item[data-view='prompts']")
    page.wait_for_timeout(400)

    # A5 分类切换 + 分页（再造 5 条同分类 → 2 页）
    page.evaluate("""async () => {
      const api = window.QuakeSkills.api;
      for (let i = 0; i < 5; i++) {
        await api.create({ title: 'G3 分页' + i, prompt: '分页测试 ' + i, summary: 'pg', category: '日常应用', tier: 'normal', enabled: true });
      }
      await window.QuakePrompts.load();
    }""")
    page.wait_for_timeout(800)
    total = page.locator("#prRows .sk-row").count()
    check("同分类造数后本页 5 条（分页生效）", total == 5, str(total))
    check("分页条出现且共 7 个", "7" in page.inner_text("#prPageN"), page.inner_text("#prPageN"))
    page.click("#prNext")
    page.wait_for_timeout(400)
    check("翻到第 2 页（2 条）", page.locator("#prRows .sk-row").count() == 2)
    page.click("#prPrev")
    page.wait_for_timeout(400)
    # 切到空分类
    page.click("#prCatBar .sk-cat[data-cat='专业场景']")
    page.wait_for_timeout(400)
    check("空分类显示空态", page.locator("#prRows .empty-state").count() == 1)
    page.click("#prCatBar .sk-cat[data-cat='日常应用']")
    page.wait_for_timeout(400)

    # A6 使用计数：对话页手选该 skill 发送 → used_count +1
    used_before = page.evaluate("(id) => { const s = (window.QuakeSkills.state.list||[]).find(x => x.id === id); return s ? (s.used_count||0) : -1; }", new_id)
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    page.click("#skPick")
    page.wait_for_timeout(300)
    page.locator(f"#modalBody .gw-model-item[data-sk='{new_id}']").click()
    page.wait_for_timeout(300)
    send_and_wait(page, "G3 提示词使用计数提问")
    page.wait_for_timeout(600)
    # used_count 是服务端计数（POST /skills/{id}/use），客户端 state.list 不会自增——
    # 重新拉一次账本再读（与用户切回提示词页触发的 load 同路径）
    page.evaluate("() => window.QuakeSkills.load()")
    page.wait_for_timeout(600)
    used_after = page.evaluate("(id) => { const s = (window.QuakeSkills.state.list||[]).find(x => x.id === id); return s ? (s.used_count||0) : -1; }", new_id)
    check("发送后 used_count +1", used_after == used_before + 1, f"{used_before} -> {used_after}")

    # A7 删除（confirm 自动接受）+ 默认联动清理
    page.click(".nav-item[data-view='prompts']")
    page.wait_for_timeout(500)
    page.locator(f"#prRows .sk-row[data-id='{new_id}'] button[data-act='del']").click()
    page.wait_for_selector(f"#prRows .sk-row[data-id='{new_id}']", state="detached", timeout=8000)
    check("删除后卡片消失", page.locator(f"#prRows .sk-row[data-id='{new_id}']").count() == 0)
    check("删除后默认徽标回 0/3", "0/3" in page.inner_text(".sk-def-badge"))
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    check("对话页默认流 chip 同步移除", "G3 测试提示词" not in page.inner_text("#skillDefaultList"))
    page.click(".nav-item[data-view='prompts']")
    page.wait_for_timeout(400)

    # ================================================================
    print("== B TTS 三级递降链（F21） ==")
    page.click(".nav-item[data-view='voice']")
    page.wait_for_timeout(800)
    chain = page.inner_text("#vcChainStatus")
    check("链路状态含网关 TTS", "网关 /v1/audio/speech" in chain, chain[:80])
    check("链路状态含 STT 描述", "transcriptions" in chain)
    check("音频模型下拉来自渠道账本（glm-4-voice 入库即迁移为 glm-asr-2512）",
          page.locator("#vc-stt-model option", has_text="glm-asr-2512").count() == 1)

    tts_before = len(log_ops("tts-received"))
    page.click("#vcTestTts")
    page.wait_for_function(
        "() => /网关：成功/.test(document.getElementById('vcHint').textContent || '')",
        timeout=15000)
    check("测试播报走网关成功（auto 第一级）", True)
    tts_after = log_ops("tts-received")
    check("mock 收到 /audio/speech 请求", len(tts_after) > tts_before, f"{tts_before} -> {len(tts_after)}")
    check("合成输入非空", (tts_after[-1].get("inputChars") or 0) > 0, str(tts_after[-1]))

    # 消息播音按钮（真实回答 → 播报）
    tts_before = len(log_ops("tts-received"))
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    send_and_wait(page, "G3 播报造数据提问")
    page.wait_for_timeout(400)
    page.locator("#messages .message.assistant").last.locator("button[data-action='speak']").click()
    page.wait_for_timeout(2500)
    check("消息播音按钮触发网关合成", len(log_ops("tts-received")) > tts_before,
          f"{tts_before} -> {len(log_ops('tts-received'))}")

    # 仅浏览器模式（headless 无语音引擎：验证模式切换与链路文案，不断言播放成功）
    page.click(".nav-item[data-view='voice']")
    page.wait_for_timeout(500)
    page.select_option("#vc-tts-mode", "browser")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    check("ttsMode=browser 持久化",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_voice_cfg')||'{}').ttsMode") == "browser")
    check("链路文案切浏览器语音", "浏览器语音（离线可用）" in page.inner_text("#vcChainStatus"))

    # 自定义端点（第二级真实命中：指向 mock /audio/speech，Bearer 有效 Key）
    page.select_option("#vc-tts-mode", "custom")
    page.fill("#vc-custom-url", "http://127.0.0.1:8788/audio/speech")
    page.fill("#vc-custom-key", "test-key-local")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    tts_before = len(log_ops("tts-received"))
    page.click("#vcTestTts")
    page.wait_for_function(
        "() => /自定义端点：成功/.test(document.getElementById('vcHint').textContent || '')",
        timeout=15000)
    check("自定义端点播报成功（第二级）", True)
    check("mock 再收合成请求", len(log_ops("tts-received")) > tts_before)
    # 恢复 auto（后续 Jarvis 播报用）
    page.select_option("#vc-tts-mode", "auto")
    page.click("#vcSave")
    page.wait_for_timeout(300)

    # ================================================================
    print("== C STT 两级递降链（F22） ==")
    check("headless 下浏览器识别不可用（链路如实标注）",
          "浏览器识别不可用" in page.inner_text("#vcChainStatus") or "浏览器实时识别" in page.inner_text("#vcChainStatus"))
    page.select_option("#vc-stt-mode", "gateway")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    check("sttMode=gateway 持久化",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_voice_cfg')||'{}').sttMode") == "gateway")
    stt_before = len(log_ops("multipart-received"))
    page.click("#vcTestStt")
    page.wait_for_function(
        "() => /转写结果/.test(document.getElementById('vcHint').textContent || '')",
        timeout=20000)
    hint = page.inner_text("#vcHint")
    check("网关转写返回 mock 文本", "mock-transcription" in hint, hint[:100])
    check("mock 收到 multipart 上传", len(log_ops("multipart-received")) > stt_before)

    # Jarvis 浮层：文字入口 → 回答 → TTS 播报
    tts_before = len(log_ops("tts-received"))
    page.click("#openJarvis")
    page.wait_for_timeout(300)
    check("语音助手浮层打开", page.evaluate("() => !document.getElementById('jarvisOverlay').classList.contains('hidden')"))
    page.fill("#jarvisText", "G3 语音助手入口提问")
    page.click("#jarvisSend")
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_timeout(3000)
    check("Jarvis 回答按 TTS 链播报", len(log_ops("tts-received")) > tts_before,
          f"{tts_before} -> {len(log_ops('tts-received'))}")
    check("浮层显示回答文本", ANSWER_MARK in page.inner_text("#jarvisTranscript") or "G3OK" in page.inner_text("#jarvisTranscript"))
    page.click("#closeJarvis")
    page.wait_for_timeout(300)

    # ================================================================
    print("== D 视觉/视频模型范围（F23） ==")
    page.click(".nav-item[data-view='voice']")
    page.wait_for_timeout(800)
    check("视觉下拉含 g3vision 渠道模型",
          page.locator("#vc-vision-model option", has_text="glm-4v-flash").count() == 1)
    page.select_option("#vc-vision-model", "glm-4v-flash")
    page.select_option("#vc-mm-scope", "attach")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    check("visionModel 持久化",
          page.evaluate("() => JSON.parse(localStorage.getItem('gw_voice_cfg')||'{}').visionModel") == "glm-4v-flash")

    # D1 带图片附件 → 视觉模型覆盖
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    page.set_input_files("#fileInput", PNG_PATH)
    page.wait_for_timeout(600)
    check("附件栏出现图片 chip", page.locator("#attachBar .att-chip").count() == 1)
    page.fill("#chatPrompt", "G3 视觉选路提问")
    page.click("#chatSend")
    page.wait_for_function(
        """(mark) => {
          const msgs = document.querySelectorAll('#messages .message.assistant .message-text');
          return msgs.length && msgs[msgs.length-1].textContent.includes(mark);
        }""", arg=ANSWER_MARK, timeout=20000)
    page.wait_for_selector("#chatStop", state="hidden", timeout=15000)
    page.wait_for_timeout(400)
    # 主聊天完成后工作流可能补发 autoTitle/memory 请求——按「带 image_url 的主请求」定位
    d1 = find_chat_digest(want_image=True)
    check("带附件消息走视觉模型（选路覆盖）",
          bool(d1) and d1.get("model") == "glm-4v-flash", str(d1.get("model") if d1 else None))

    # D2 always 范围 + 自动选路 → 纯文本也走视觉模型
    page.click(".nav-item[data-view='voice']")
    page.wait_for_timeout(600)
    page.select_option("#vc-mm-scope", "always")
    page.click("#vcSave")
    page.wait_for_timeout(400)
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    send_and_wait(page, "G3 always 范围纯文本提问")
    page.wait_for_timeout(400)
    d2 = find_chat_digest(marker="G3 always 范围纯文本提问")
    check("always 范围 + 自动选路：纯文本也走视觉模型",
          bool(d2) and d2.get("model") == "glm-4v-flash", str(d2.get("model") if d2 else None))

    # D3 显式选模型优先（always 不抢走用户选择）
    page.click("#chatModelBtn")
    page.wait_for_timeout(300)
    page.locator("#modalBody .gw-model-item[data-m='glm-4-flash']").click()
    page.wait_for_timeout(300)
    send_and_wait(page, "G3 显式选模型提问")
    page.wait_for_timeout(400)
    d3 = find_chat_digest(marker="G3 显式选模型提问")
    check("显式选模型优先于 always 范围",
          bool(d3) and d3.get("model") == "glm-4-flash", str(d3.get("model") if d3 else None))
    # 恢复 attach + 自动选路
    page.click("#chatModelBtn"); page.wait_for_timeout(200)
    page.locator("#modalBody .gw-model-item[data-m='']").click(); page.wait_for_timeout(200)
    page.click(".nav-item[data-view='voice']"); page.wait_for_timeout(600)
    page.select_option("#vc-mm-scope", "attach")
    page.click("#vcSave"); page.wait_for_timeout(300)

    # ================================================================
    print("== E 工作流参数（F24） ==")
    page.click(".nav-item[data-view='workflow']")
    page.wait_for_timeout(500)
    check("工作流视图打开（面包屑）", page.inner_text("#breadcrumb") == "工作流")
    check("标题/记忆模型下拉有选项", page.locator("#wf-model-sel option").count() >= 2)
    page.fill("#wf-thumb", "120")
    page.fill("#wf-vw", "300")
    page.fill("#wf-vh", "220")
    page.fill("#wf-mem-maxtok", "256")
    page.fill("#wf-mem-keep", "4")
    page.click("#wfSave")
    page.wait_for_timeout(400)
    cssvars = page.evaluate("""() => {
      const s = document.documentElement.style;
      return { thumb: s.getPropertyValue('--att-thumb'), vw: s.getPropertyValue('--att-video-w'), vh: s.getPropertyValue('--att-video-h') };
    }""")
    check("格式参数实时生效（CSS 变量）",
          cssvars["thumb"] == "120px" and cssvars["vw"] == "300px" and cssvars["vh"] == "220px", str(cssvars))
    wf = page.evaluate("() => JSON.parse(localStorage.getItem('gw_wf_cfg')||'{}')")
    check("gw_wf_cfg 持久化（数值字段）",
          wf.get("thumb") == 120 and wf.get("vw") == 300 and wf.get("vh") == 220
          and wf.get("memMaxTokens") == 256 and wf.get("memKeepRecent") == 4, json.dumps(wf)[:160])

    # E1 auto_title：两轮真实对话后标题被 LLM 改写
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    send_and_wait(page, "G3 工作流第一轮")
    page.wait_for_timeout(600)
    send_and_wait(page, "G3 工作流第二轮")
    page.wait_for_timeout(1500)
    title_info = page.evaluate("""async () => {
      const convos = await window.QuakeStore.loadConversations();
      const c = convos.find(x => (x.msgs||[]).some(m => (m.content||'').includes('G3 工作流第一轮')));
      return c ? { title: c.title, auto: c.title_auto, answers: c.answers } : null;
    }""")
    check("auto_title 真实跑（title_auto=true 且标题非空）",
          bool(title_info) and title_info.get("auto") is True and bool(title_info.get("title")),
          json.dumps(title_info)[:120])

    # E2 memory 压缩：直接驱动 memoryUpdate（token 计数来自 mock 固定 16/轮，
    # 阈值 256 需 16 轮太慢——用同等代码路径 + 超阈 token 验证 LLM 压缩真实发生。
    # 注意：消息数必须 > memKeepRecent（E 段已设为 4），否则没有可折叠区域，
    # 走的是「无新增内容」分支（计数减半、不压缩）。
    mem = page.evaluate("""async () => {
      const W = window.QuakeWorkflow;
      const msgs = [];
      for (let i = 0; i < 10; i++) {
        msgs.push({ role: 'user', content: '第 ' + i + ' 轮提问：项目背景是网关重构。', ts: i * 2 + 1 });
        msgs.push({ role: 'assistant', content: '第 ' + i + ' 轮回答：好的，记住了。', ts: i * 2 + 2 });
      }
      const c = { id: 'g3-mem-test', msgs, memory: '' };
      const r = await W.memoryUpdate(c, 9999);
      return { r, memory: c.memory };
    }""")
    check("memory 压缩触发且产出长期记忆",
          mem["r"].get("compressed") is True and bool(mem["memory"]), json.dumps(mem)[:140])

    # E3 usage 开关：关闭后消息元信息不含 token 徽标
    page.click(".nav-item[data-view='workflow']")
    page.wait_for_timeout(400)
    page.uncheck("#wf-on-usage")
    page.click("#wfSave")
    page.wait_for_timeout(300)
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    send_and_wait(page, "G3 usage 关闭提问")
    page.wait_for_timeout(400)
    last_meta = page.locator("#messages .message.assistant .msg-meta").last.inner_text()
    check("usage 关闭后无 token 徽标", "⛁" not in last_meta, last_meta[:80])
    # 恢复
    page.click(".nav-item[data-view='workflow']")
    page.wait_for_timeout(400)
    page.check("#wf-on-usage")
    page.click("#wfSave")
    page.wait_for_timeout(300)

    # ================================================================
    print("== F 收尾 ==")
    page.screenshot(path="/tmp/g3-workflow.png", full_page=False)
    check("A–F 控制台/页面零错误", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()
    browser.close()

print(f"\nG3 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
