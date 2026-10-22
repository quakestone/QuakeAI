/* ============================================================
   QuakeAI netmon.js —— 网络监视（B4：F29，横切能力）
   旧 UI（admin_ui.html NET 段）同构迁移；差异仅在于：旧架构的 WASM 工作流
   内核（/wf/kernel.wasm，CF Worker 托管）在纯静态部署下不存在——探测 DAG
   退化为直接跑插件链（trace → health 兜底），阈值/状态机/心跳口径不变。
   四态：ok（≤300ms）/ slow（≤800ms）/ bad（>800ms 或超时）/ offline（失败）。
   - 首屏 3s 首次探测（不抢首屏资源）+ 60s 心跳（requestIdleCallback 空闲调度）
   - online / offline 事件即时响应；缓存模式（bad/offline）停心跳省流量
   - gw_net_last 持久化：离线刷新后仍能显示「缓存 · HTTPS · 美国」
   - 探测失败静默（不弹 toast）；仅手动点击时给结论 toast
   ============================================================ */
(function (global) {
  "use strict";

  const $ = (s) => document.querySelector(s);
  const { toast } = global.QuakeComponents;

  const HEARTBEAT_MS = 60000;
  const IDLE_TIMEOUT_MS = 5000;   // rIC 兜底：浏览器一直忙也最多等 5s 强制跑
  const OK_MS = 300, BAD_MS = 800;
  const PROBE_TIMEOUT_MS = 1000;  // 探测硬超时：1s 内不出结论即判失败，绝不长期停在「检测中」

  const state = {
    st: "idle",        // idle | probing | ok | slow | bad | offline
    latency: 0,
    proto: "",
    country: "",
    colo: "",
    lastOk: (() => {
      try { return JSON.parse(localStorage.getItem("gw_net_last") || "null"); } catch (e) { return null; }
    })(),
    probe: null,
    timer: null,
    idle: null,
    gen: 0,
  };

  function probeSignal() {
    return (typeof AbortSignal !== "undefined" && AbortSignal.timeout)
      ? { signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) } : {};
  }
  function cacheMode() { return state.st === "bad" || state.st === "offline"; }

  /// 国家代码 → 中文名（Intl.DisplayNames 原生 API，零映射表）；不支持时原样返回
  function regionName(code) {
    if (!code) return "";
    try { return new Intl.DisplayNames(["zh"], { type: "region" }).of(String(code).toUpperCase()) || code; }
    catch (e) { return code; }
  }

  /* ---------- 状态条渲染 ---------- */
  function render() {
    const bar = $("#net-bar"), txt = $("#nb-txt");
    if (!bar || !txt) return;
    bar.dataset.st = state.st;
    if (state.st === "probing" || state.st === "idle") { txt.textContent = "检测中…"; return; }
    const cache = cacheMode();
    const snap = cache ? (state.lastOk || {}) : state;
    const tail = [snap.proto || "", snap.country || ""].filter(Boolean).join(" · ");
    txt.textContent = (cache ? "缓存" : state.latency + "ms") + (tail ? " · " + tail : "");
  }

  function applyResult(p) {
    state.probe = null;
    if (!p || !p.ok) { state.st = "offline"; render(); stopHeartbeat(); return; }
    state.latency = Math.max(0, Math.round(p.ms || 0));
    state.proto = p.proto || state.proto || "";
    if (p.loc) state.country = regionName(p.loc);
    if (p.colo) state.colo = p.colo;
    state.lastOk = { proto: state.proto, country: state.country };
    try { localStorage.setItem("gw_net_last", JSON.stringify(state.lastOk)); } catch (e) { /* 隐私模式忽略 */ }
    state.st = state.latency > BAD_MS ? "bad" : state.latency > OK_MS ? "slow" : "ok";
    render();
    if (state.st === "bad") stopHeartbeat();
  }

  /* ---------- 探测插件链 ----------
     插件① /cdn-cgi/trace：落点 colo、地区 loc、协议、延迟（CF 环境才有；
       静态托管/自建反代不存在该路径 → ok:false，交给插件②兜底）。
     插件② /health 计时：至少给出延迟与协议。两者都跑，取优先级高的那份。 */
  async function probeTrace() {
    const t0 = performance.now();
    let out;
    try {
      const r = await fetch("/cdn-cgi/trace", { cache: "no-store", ...probeSignal() });
      const ms = performance.now() - t0;
      if (!r.ok) out = { ok: false, reason: "trace-" + r.status, ms };
      else {
        const txt = await r.text();
        const kv = {};
        for (const line of String(txt).split("\n")) {
          const i = line.indexOf("=");
          if (i > 0) kv[line.slice(0, i)] = line.slice(i + 1).trim();
        }
        out = (kv.colo || kv.loc)
          ? { ok: true, ms, colo: kv.colo || "", loc: kv.loc || "", proto: location.protocol.replace(":", "").toUpperCase() }
          : { ok: false, reason: "trace-empty", ms };
      }
    } catch (e) {
      out = { ok: false, reason: (e && e.name === "TimeoutError") ? "trace-timeout" : "trace-net", ms: performance.now() - t0 };
    }
    state.probe = out;
    return out;
  }
  async function probeHealth() {
    const t0 = performance.now();
    let out;
    try {
      const r = await fetch("/health", { cache: "no-store", ...probeSignal() });
      const ms = performance.now() - t0;
      out = r.ok
        ? { ok: true, ms, proto: location.protocol.replace(":", "").toUpperCase(), loc: "", colo: "" }
        : { ok: false, reason: "health-" + r.status, ms };
    } catch (e) {
      out = { ok: false, reason: (e && e.name === "TimeoutError") ? "health-timeout" : "health-net", ms: performance.now() - t0 };
    }
    if (!state.probe || !state.probe.ok) state.probe = out;
    return out;
  }

  /// 触发一次探测。reason: boot | heartbeat | online | offline | manual
  async function probe(reason) {
    if (state.st === "probing" && reason !== "manual") return;
    const myGen = ++state.gen;
    state.st = "probing"; state.probe = null; render();
    try {
      const p = await probeTrace();
      if (!p.ok) await probeHealth();
    } catch (e) {
      state.probe = { ok: false, reason: "kernel" };
    }
    if (myGen !== state.gen) return;   // 有更新的探测在跑，丢弃旧结果
    applyResult(state.probe);
    // 链式续命：上一次探测结束才排下一次——不因探测慢而堆叠；缓存模式不排
    if (!cacheMode()) startHeartbeat();
    if (reason === "manual") {
      toast(cacheMode()
        ? "网络探测未通过（已切缓存展示），但发送仍可用——VPN 不稳时直接发即可"
        : "网络正常 · " + state.latency + "ms", cacheMode() ? "err" : "ok");
    }
  }

  /* ---------- 心跳调度（rIC 空闲窗口，两段式：先等 60s 再找空闲） ---------- */
  function startHeartbeat(delay) {
    stopHeartbeat();
    state.timer = setTimeout(() => {
      state.timer = null;
      if (document.hidden || cacheMode()) return;
      const ric = (typeof window.requestIdleCallback === "function")
        ? window.requestIdleCallback.bind(window)
        : (fn) => setTimeout(fn, 1);   // 降级：rIC 缺失（旧 Safari）
      state.idle = ric(() => {
        state.idle = null;
        if (document.hidden || cacheMode()) return;
        probe("heartbeat");
      }, { timeout: IDLE_TIMEOUT_MS });
    }, typeof delay === "number" ? delay : HEARTBEAT_MS);
  }
  function stopHeartbeat() {
    if (state.timer) { clearTimeout(state.timer); state.timer = null; }
    if (state.idle != null) {
      try { (window.cancelIdleCallback || clearTimeout)(state.idle); } catch (e) { /* 忽略 */ }
      state.idle = null;
    }
  }

  /* ---------- 启动 ---------- */
  function start() {
    render();
    setTimeout(() => probe("boot"), 3000);
    startHeartbeat();
    window.addEventListener("online", () => { probe("online"); startHeartbeat(); });
    window.addEventListener("offline", () => { state.st = "offline"; render(); stopHeartbeat(); });
    // 标题承诺「点击重新检测」——补上点击处理（手动探测才有结论 toast）
    const bar = $("#net-bar");
    if (bar) bar.addEventListener("click", () => probe("manual"));
  }

  global.QuakeNetMon = {
    start, probe, render,
    get state() { return state; },
  };
})(window);
