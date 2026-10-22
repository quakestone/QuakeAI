#!/usr/bin/env python3
#!/usr/bin/env python3
"""G5 门禁验证：UI 全量重建 B5——PWA / 性能打磨 / 深浅色 / 无障碍（F30 + 打磨项）
   覆盖：
   A Service Worker 与离线开壳（F30）：注册成功 + 页面受控 + 预缓存完整；
     断网 reload 后应用壳可开（导航/视图切换纯本地）+ WASM 网关从缓存装载就绪
   B manifest 与 iOS standalone：manifest 可 fetch 且字段齐全；icons 含 PNG
     且文件 200；apple metas（capable/status-bar/touch-icon）在位
   C 深色主题：toggle 切换 + data-theme + computed 背景色真实变化 + 持久化
     + 刷新保持 + theme-color meta 同步
   D 系统深浅色首次跟随：color_scheme=dark 首访 data-theme=dark 且不落盘；
     手动切换后以用户为准（落盘）
   E prefers-reduced-motion：reduce 下过渡时长趋零（对照非 reduce 有过渡）
   F 无障碍：Esc 关 Jarvis + 焦点还原到触发按钮；modal 打开焦点移入；
     aria-current 标记激活导航
   用法：python3 scripts/g5-verify.py [BASE_URL]（默认 http://127.0.0.1:8932/QuakeAI/）
   前置：bash scripts/build.sh；静态服务托管 build/（子路径）
   一键：bash scripts/test-ui-g5.sh
   注意：本门禁需要 SW 真实注册（context service_workers="allow"）——
         G0–G4 各自 block，互不影响；回归在 G5 全绿后单独跑。
"""
import json, os, re, sys
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
SW_SRC = os.path.join(HERE, "..", "src", "sw.js")
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8932/QuakeAI/"
PASS, FAIL = 0, 0

def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1; print(f"  [PASS] {name}")
    else:
        FAIL += 1; print(f"  [FAIL] {name}" + (f" -- {detail}" if detail else ""))

def precache_list():
    """从 src/sw.js 解析 PRECACHE 相对路径清单（随源漂移，不硬编码）"""
    src = open(SW_SRC, encoding="utf-8").read()
    m = re.search(r"const PRECACHE = \[(.*?)\];", src, re.S)
    return sorted(set(re.findall(r'"(\./[^"]+)"', m.group(1)))) if m else []

with sync_playwright() as p:
    browser = p.chromium.launch(args=["--no-sandbox"])
    # F30 需要 SW 真实注册与管控：允许 service worker（G0–G4 的 context 各自 block）
    ctx = browser.new_context(service_workers="allow", accept_downloads=True)
    page = ctx.new_page()
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text)
            if m.type == "error" and not ((m.location or {}).get("url") or "").endswith(("/cdn-cgi/trace", "/health"))
            else None)
    page.on("dialog", lambda d: d.accept())

    page.goto(BASE)
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    page.wait_for_timeout(500)

    # ================================================================
    print("== A Service Worker 与离线开壳（F30） ==")
    page.wait_for_function("() => navigator.serviceWorker.controller !== null", timeout=15000)
    check("SW 注册且页面受控（controller 非空）", True)
    reg = page.evaluate("""async () => {
      const r = await navigator.serviceWorker.getRegistration();
      return r ? { scope: r.scope, active: !!r.active } : null;
    }""")
    check("注册 scope 收敛到子路径目录", reg and reg.get("active") and reg.get("scope", "").endswith("/QuakeAI/"), str(reg))

    cached = page.evaluate("""async () => {
      const c = await caches.open("quakeai-gw-v2");
      const keys = await c.keys();
      return keys.map(r => new URL(r.url).pathname);
    }""")
    # PRECACHE 项是 "./x" 相对 SW scope（/QuakeAI/）——期望落表的 pathname 是 /QuakeAI/x
    missing = [it for it in precache_list() if ("/QuakeAI/" + it[2:]) not in cached]
    check(f"预缓存完整（{len(precache_list())} 项全部落 cache）", not missing, str(missing))

    # 离线开壳：断网 → reload → 壳可开 + WASM 从缓存装载
    ctx.set_offline(True)
    page.reload()
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=20000)
    check("断网 reload 后 WASM 网关从缓存装载就绪", True)
    check("断网下应用壳渲染（导航/品牌/视图容器在位）",
          page.locator("#appShell .nav-item[data-view='chat']").count() == 1
          and page.locator("#homeView").count() == 1)
    # 断网下纯本地交互：切视图 + 主题切换 + health（WASM 本地，不走网络）
    page.click(".nav-item[data-view='upstream']")
    page.wait_for_timeout(400)
    check("断网下视图切换正常（面包屑）", page.inner_text("#breadcrumb") == "上游与接入")
    health_ok = page.evaluate("() => window.QuakeAdapters.gateway.health().then(j => j.status === 'ok')")
    check("断网下 health 可调（WASM 本地响应）", health_ok is True)
    ctx.set_offline(False)

    # ================================================================
    print("== B manifest 与 iOS standalone ==")
    man = page.evaluate("""async () => {
      const r = await fetch("./manifest.webmanifest");
      return r.ok ? await r.json() : null;
    }""")
    check("manifest 可 fetch 且字段齐全",
          bool(man) and all(k in man for k in ("name", "short_name", "start_url", "scope", "display", "icons")),
          json.dumps(man)[:120])
    check("manifest 相对路径（子路径兼容）",
          bool(man) and man.get("start_url") == "./" and man.get("scope") == "./")
    png_icon = page.evaluate("() => fetch('./apple-touch-icon.png').then(r => r.status)")
    check("apple-touch-icon.png 可访问（200）", png_icon == 200, str(png_icon))
    check("manifest icons 含 PNG 条目",
          bool(man) and any(i.get("src", "").endswith("apple-touch-icon.png") for i in man.get("icons", [])))
    head = page.evaluate("() => document.head.innerHTML")
    check("iOS standalone metas 在位",
          'apple-mobile-web-app-capable' in head and 'apple-mobile-web-app-status-bar-style' in head
          and 'apple-touch-icon' in head)
    check("theme-color meta 初始为浅色值", 'id="metaTheme"' in head and '#f7f8fa' in head)

    # ================================================================
    print("== C 深色主题 ==")
    page.evaluate("() => localStorage.removeItem('gw_theme')")
    page.reload()
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    check("无 gw_theme 且系统 light → data-theme=light",
          page.evaluate("() => document.documentElement.dataset.theme") == "light")
    # 回归守卫：720p 矮视口下 sidebar flex 列曾溢出 11px，主题切换按钮被挤出视口
    # （.conversation-list min-height:60 不肯再缩）——修复=列表可缩至 0 + 两档 compaction
    tv = page.evaluate("() => { const r = document.getElementById('themeToggle').getBoundingClientRect(); return { bottom: r.bottom, innerH: window.innerHeight }; }")
    check("矮屏 compaction：主题切换按钮在视口内（不出折页）", tv["bottom"] <= tv["innerH"], str(tv))
    bg_light = page.evaluate("() => getComputedStyle(document.body).backgroundColor")
    page.click("#themeToggle")
    page.wait_for_timeout(300)
    check("toggle → data-theme=dark", page.evaluate("() => document.documentElement.dataset.theme") == "dark")
    check("gw_theme=dark 持久化", page.evaluate("() => localStorage.getItem('gw_theme')") == "dark")
    bg_dark = page.evaluate("() => getComputedStyle(document.body).backgroundColor")
    check("computed 背景色真实变化（浅→深）", bg_light != bg_dark, f"{bg_light} -> {bg_dark}")
    check("theme-color meta 同步为深色",
          page.evaluate("() => document.getElementById('metaTheme').content") == "#15171b")
    page.reload()
    page.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    check("刷新后保持 dark（持久化生效）",
          page.evaluate("() => document.documentElement.dataset.theme") == "dark")
    page.click("#themeToggle")
    page.wait_for_timeout(300)
    check("再 toggle 回 light + meta 回浅色",
          page.evaluate("() => document.documentElement.dataset.theme") == "light"
          and page.evaluate("() => document.getElementById('metaTheme').content") == "#f7f8fa")

    # ================================================================
    print("== D 系统深浅色首次跟随（独立 context：color_scheme=dark） ==")
    ctx_d = browser.new_context(service_workers="allow", color_scheme="dark")
    pd = ctx_d.new_page()
    pd.goto(BASE)
    pd.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    check("系统偏好 dark + 无 gw_theme → 首访 data-theme=dark",
          pd.evaluate("() => document.documentElement.dataset.theme") == "dark")
    check("系统跟随不落盘（gw_theme 仍为空）",
          pd.evaluate("() => localStorage.getItem('gw_theme')") is None)
    pd.click("#themeToggle")
    pd.wait_for_timeout(300)
    check("手动切换后以用户为准（落盘 light）",
          pd.evaluate("() => document.documentElement.dataset.theme") == "light"
          and pd.evaluate("() => localStorage.getItem('gw_theme')") == "light")
    ctx_d.close()

    # ================================================================
    print("== E prefers-reduced-motion（独立 context：reduced_motion=reduce） ==")
    ctx_r = browser.new_context(service_workers="allow", reduced_motion="reduce")
    pr = ctx_r.new_page()
    pr.goto(BASE)
    pr.wait_for_function("() => window.QuakeAdapters && window.QuakeAdapters.gateway.ready === true", timeout=15000)
    dur_reduce = pr.evaluate("() => parseFloat(getComputedStyle(document.querySelector('.primary-btn')).transitionDuration) || 0")
    check("reduce 下过渡时长趋零（<1ms）", dur_reduce < 0.001, str(dur_reduce))
    ctx_r.close()
    # 对照：非 reduce 环境下同一元素有过渡（证明断言靶子有效，不是恒零）
    dur_normal = page.evaluate("() => parseFloat(getComputedStyle(document.querySelector('.primary-btn')).transitionDuration) || 0")
    check("对照：非 reduce 环境过渡正常（>0.1s）", dur_normal > 0.1, str(dur_normal))

    # ================================================================
    print("== F 无障碍 ==")
    page.click("#openJarvis")
    page.wait_for_timeout(300)
    check("Jarvis 打开后焦点在浮层输入框",
          page.evaluate("() => document.activeElement && document.activeElement.id") == "jarvisText")
    page.keyboard.press("Escape")
    page.wait_for_timeout(300)
    check("Esc 关闭 Jarvis", page.evaluate("() => document.getElementById('jarvisOverlay').classList.contains('hidden')"))
    check("关闭后焦点还原到触发按钮",
          page.evaluate("() => document.activeElement && document.activeElement.id") == "openJarvis")
    # modal 焦点移入（模型选择 modal 走 openModal；按钮在 chat 视图内，先切视图）
    page.click(".nav-item[data-view='chat']")
    page.wait_for_timeout(400)
    page.click("#chatModelBtn")
    page.wait_for_timeout(300)
    check("modal 打开后焦点移入浮层内",
          page.evaluate("() => { const a = document.activeElement; return !!a && !!a.closest('#modalBackdrop'); }"))
    page.keyboard.press("Escape")
    page.wait_for_timeout(300)
    check("Esc 关闭 modal", page.evaluate("() => document.getElementById('modalBackdrop').classList.contains('hidden')"))
    check("激活导航项带 aria-current=page",
          page.evaluate("() => { const b = document.querySelector('.nav-item[data-view].active'); return b && b.getAttribute('aria-current') === 'page'; }"))
    check("浮层 aria 语义在位（jarvis dialog/modal、sidebar 标签）",
          page.evaluate("() => document.getElementById('jarvisOverlay').getAttribute('aria-modal') === 'true'"
                        + " && document.getElementById('sidebar').getAttribute('aria-label') === '主导航'"))

    # ================================================================
    print("== G 收尾 ==")
    page.screenshot(path="/tmp/g5-final.png", full_page=False)
    check("A–F 控制台/页面零错误（探测路径 404 除外）", len(errors) == 0, "; ".join(errors[:3]))
    ctx.close()
    browser.close()

print(f"\nG5 结果：PASS={PASS} FAIL={FAIL}")
sys.exit(1 if FAIL else 0)
