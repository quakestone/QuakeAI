/* ============================================================
   QuakeAI magic.js —— 魔法指令 + 应用内调试浮层 + 隐藏控制台面板
   口径与旧 UI 完全一致：
   - 魔法指令：/debug on|off（冒号可选，全角／兼容）、/console: zsj|off
     命中即消费——不发消息、不落库；纵深防御见 app.js sendWith 入口
   - 调试（gw_debug）：dbg() 同时写 DevTools 与页面内浮层（#debug-dlg）。
     iOS 主屏 PWA 是 standalone 模式，没有 F12，只能靠页面内浮层看日志
   - 控制台（gw_console）：顶栏 ⌗ 打开状态速览面板（#console-dlg），只读
   持久化键不变：gw_debug / gw_console。
   ============================================================ */
(function (global) {
  "use strict";

  const { config } = global.QuakeStore;
  const { toast } = global.QuakeComponents;

  const DEBUG_KEY = "gw_debug";
  const CONSOLE_KEY = "gw_console";
  const DBG_MAX = 500;            // 内存里最多保留最近 500 条，防无限增长
  let DBG_LOG = [];

  /* app.js 注入的状态源（控制台面板数据）——magic 不反向依赖 app */
  let stateProvider = null;
  function setStateProvider(fn) { stateProvider = fn; }

  function debugOn() { return config.get(DEBUG_KEY, "") === "1"; }
  function consoleOn() { return config.get(CONSOLE_KEY, "") === "1"; }

  function renderDebugBtn() { const b = document.getElementById("btn-debug"); if (b) b.hidden = !debugOn(); }
  function renderConsoleBtn() { const b = document.getElementById("btn-console"); if (b) b.hidden = !consoleOn(); }

  function setDebug(v) {
    if (v) config.set(DEBUG_KEY, "1"); else { config.remove(DEBUG_KEY); DBG_LOG = []; }
    renderDebugBtn();
    if (v) openDebug();           // 开启即自动弹出浮层——iOS 用户立刻能看到，不必找按钮
    toast(v ? "调试已开启：日志显示在页面内（点顶栏 🐞 查看）。iOS 主屏 PWA 无 F12，用此面板；桌面端也可按 F12 看 DevTools"
            : "调试已关闭", v ? "ok" : "");
  }
  function setConsoleOn(v) {
    if (v) config.set(CONSOLE_KEY, "1"); else config.remove(CONSOLE_KEY);
    renderConsoleBtn();
  }

  /// 调试日志：仅 debug 模式（/debug on）下输出。同时写 DevTools 与页面内浮层。
  function dbg(...a) {
    if (!debugOn()) return;
    const line = a.map(x => {
      if (typeof x === "string") return x;
      if (x && x.stack) return x.stack;          // Error 直接给堆栈，排查 VPN 断流有用
      try { return JSON.stringify(x); } catch { return String(x); }
    }).join(" ");
    const d = new Date();
    const ts = d.toLocaleTimeString("zh-CN", { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0");
    DBG_LOG.push(ts + " " + line);
    if (DBG_LOG.length > DBG_MAX) DBG_LOG.splice(0, DBG_LOG.length - DBG_MAX);
    try { console.log("[gw-debug]", ...a); } catch {}
    renderDbgLog();
  }
  function renderDbgLog() {
    const el = document.getElementById("dbg-body");
    if (!el) return;
    el.textContent = DBG_LOG.length ? DBG_LOG.join("\n") : "（暂无日志，操作或等待网络探测后会出现）";
    el.scrollTop = el.scrollHeight;   // 自动滚到底：浮层开着时最新日志始终可见
  }
  function clearDbgLog() { DBG_LOG = []; renderDbgLog(); toast("已清空调试日志", "ok"); }
  function copyDbgLog() {
    navigator.clipboard.writeText(DBG_LOG.join("\n"))
      .then(() => toast("已复制调试日志", "ok")).catch(() => toast("复制失败", "err"));
  }
  function openDebug() {
    const dlg = document.getElementById("debug-dlg");
    if (!dlg) return;
    renderDbgLog();
    try { dlg.showModal(); } catch { dlg.setAttribute("open", ""); }
  }
  function closeDebug() {
    const dlg = document.getElementById("debug-dlg");
    if (!dlg) return;
    try { dlg.close(); } catch { dlg.removeAttribute("open"); }
  }

  /* ---------- 隐藏控制台面板（状态速览，只读） ---------- */
  function openConsole() {
    const dlg = document.getElementById("console-dlg");
    if (!dlg) return;
    renderConsoleBody();
    try { dlg.showModal(); } catch { dlg.setAttribute("open", ""); }
  }
  function closeConsole() {
    const dlg = document.getElementById("console-dlg");
    if (!dlg) return;
    try { dlg.close(); } catch { dlg.removeAttribute("open"); }
  }
  function consoleState() {
    const s = stateProvider ? stateProvider() : {};
    const chs = s.channels || [];
    const usable = chs.filter(c => c.enabled && (c.api_key_masked || c.api_key)).length;
    return {
      version: window.__BUILD_VERSION__ || "?",
      net: s.net || null,
      channels: { total: chs.length, usable },
      models: s.models || [],
      model: s.model || "(自动)",
      skills: s.skills || null,
      debug: debugOn(),
      console: consoleOn(),
      ua: navigator.userAgent.slice(0, 80),
    };
  }
  function renderConsoleBody() {
    const el = document.getElementById("console-body");
    if (!el) return;
    el.textContent = JSON.stringify(consoleState(), null, 2);
  }
  function copyConsoleState() {
    navigator.clipboard.writeText(JSON.stringify(consoleState(), null, 2))
      .then(() => toast("已复制状态", "ok")).catch(() => toast("复制失败", "err"));
  }

  /* ---------- 魔法指令（纯解析，无副作用） ----------
     兼容全角斜杠 ／（中文输入法易误触）与半角 /。 */
  function parseMagic(t) {
    const s = String(t || "").trim();
    const dm = /^\s*[\/／]\s*debug\s*[:：]?\s*(on|off|1|0)\s*$/i.exec(s);
    if (dm) return { kind: "debug", on: /^(on|1)$/i.test(dm[1]) };
    const m = /^\s*[\/／]\s*(console|consle)\s*[:：]\s*([\w-]+)\s*$/i.exec(s);
    if (m) return { kind: "console", arg: String(m[2]).toLowerCase() };
    return null;
  }
  /// 魔法指令识别。返回 true 表示已消费（调用方直接 return，不发送本轮）。
  function handleMagicCommand(text) {
    const p = parseMagic(text);
    if (!p) return false;
    if (p.kind === "debug") {
      setDebug(p.on);
    } else if (p.arg === "zsj" || p.arg === "on") {
      setConsoleOn(true);
      toast("控制台已开启——顶栏出现 ⌗ 按钮", "ok");
    } else if (p.arg === "off") {
      setConsoleOn(false);
      toast("控制台已关闭", "");
    } else {
      toast("参数不认识：/console: zsj 开启，/console: off 关闭", "err");
    }
    return true;
  }

  /* ---------- 装配（按钮 / 对话框） ---------- */
  function init() {
    renderDebugBtn();
    renderConsoleBtn();
    const bd = document.getElementById("btn-debug");
    if (bd) bd.addEventListener("click", openDebug);
    const bc = document.getElementById("btn-console");
    if (bc) bc.addEventListener("click", openConsole);
    const dc = document.getElementById("dbg-clear");
    if (dc) dc.addEventListener("click", clearDbgLog);
    const dcopy = document.getElementById("dbg-copy");
    if (dcopy) dcopy.addEventListener("click", copyDbgLog);
    const dclose = document.getElementById("dbg-close");
    if (dclose) dclose.addEventListener("click", closeDebug);
    const cc = document.getElementById("console-copy");
    if (cc) cc.addEventListener("click", copyConsoleState);
    const cclose = document.getElementById("console-close");
    if (cclose) cclose.addEventListener("click", closeConsole);
  }

  global.QuakeMagic = {
    init, parseMagic, handleMagicCommand, dbg,
    debugOn, consoleOn, setDebug, setConsoleOn,
    openDebug, closeDebug, openConsole, closeConsole,
    setStateProvider, consoleState,
  };
})(window);
