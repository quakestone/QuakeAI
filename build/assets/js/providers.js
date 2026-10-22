/* ============================================================================
   QuakeProviders —— 服务商与模型管理（B2 / F8–F18）
   ----------------------------------------------------------------------------
   本模块是「服务商」视图的唯一业务层：渠道 CRUD、Key 轮换、启停/优先级、
   连通测试、模型元数据、预设、一键导入/默认/导出、批量体检、用量对账。
   与旧 UI（admin_ui.html 模型服务商标签）行为对齐，表现层换 Clarity Kit。
   依赖：adapters（WASM 边界）/ components（toast/escapeHTML/md）。
   app.js 只做视图装配与 onChange 回调（渠道变更 → 刷新模型/顶栏）。
   脚本加载序：adapters → components → … → providers → app（见 index.html）。
   ==========================================================================*/
(function (global) {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const { toast, escapeHTML } = global.QuakeComponents;
  const gw = global.QuakeAdapters.gateway;
  const effModels = global.QuakeAdapters.effModels;

  const CAP_LABEL = { text: "文本", vision: "视觉", stt: "STT", tts: "播音" };
  const CAP_KINDS = ["text", "vision", "stt", "tts"];
  const US_PAGE_SIZE = 8;        // 用量三表共用页码
  const SELF_LIMIT = 12;         // 批量体检每轮上限（串行，防打爆上游限流）
  const MASK_RE = /\*\*\*|\.\.\.|…/;

  const state = {
    channels: [], summary: null, presets: [], store: "", storeError: "",
    loaded: false, loading: false,
    usagePage: 1, usageRaw: { provider: [], model: [], day: [] },
    editId: null,                // 渠道对话框：null = 新建，否则编辑该 id
    keyId: null,                 // 换 Key 对话框目标
    selfBusy: false,
  };
  let onChange = () => {};       // app.js 注入：渠道账本变更后的联动刷新

  /* ================= 工具 ================= */
  function looksMaskedKey(k) { return MASK_RE.test(k || ""); }
  function chMissKey(c) { return !c.api_key_masked || c.api_key_masked === "未登记" || c.api_key_masked === "—"; }
  function healthOf(c) {
    const now = Date.now();
    if (!c.enabled) return { dot: "off", label: "停用" };
    if (c.health && c.health.cooldown_until_ms && c.health.cooldown_until_ms > now) {
      const left = Math.ceil((c.health.cooldown_until_ms - now) / 1000);
      return { dot: "cool", label: "冷却中 " + left + "s" };
    }
    switch (c.health && c.health.status) {
      case "healthy": return { dot: "ok", label: "健康" };
      case "degraded": return { dot: "warn", label: "降权" };
      case "down": return { dot: "bad", label: "故障" };
      default: return { dot: "off", label: "未知" };
    }
  }
  function tierClass(t) { return "tier t" + String(t || "T1").replace(/[^0-9]/g, ""); }
  function capBadge(k) { return '<span class="cap-badge" title="' + escapeHTML(CAP_LABEL[k] || k) + '">' + escapeHTML(CAP_LABEL[k] || k) + "</span>"; }
  function fmtInt(n) {
    n = Number(n || 0);
    if (n >= 1e8) return (n / 1e8).toFixed(2) + " 亿";
    if (n >= 1e4) return (n / 1e4).toFixed(2) + " 万";
    return n.toLocaleString("zh-CN");
  }
  function fmtCost(v) {
    const n = Number(v || 0);
    if (!n) return "—";
    if (n < 0.001) return n.toFixed(6);
    if (n < 1) return n.toFixed(4);
    return n.toFixed(2);
  }

  /* ================= 装载与总览 ================= */
  async function load() {
    if (state.loading) return;
    state.loading = true;
    try {
      const j = await gw.listChannels();
      state.channels = Array.isArray(j.channels) ? j.channels : [];
      state.summary = j.summary || null;
      state.presets = Array.isArray(j.presets) ? j.presets : [];
      state.store = j.store || "";
      state.storeError = j.store_error || "";
      state.loaded = true;
    } catch (e) {
      state.channels = []; state.summary = null; state.presets = [];
      state.store = "unavailable"; state.storeError = e.message || String(e);
    } finally {
      state.loading = false;
    }
    renderAll();
  }

  function renderAll() {
    renderSummary();
    renderChannels();
    renderPresets($("#pvPresetSearch").value || "");
    loadUsage();
  }

  function renderSummary() {
    const el = $("#pvSummary");
    if (!el) return;
    if (state.store === "unavailable") {
      el.innerHTML = '<div class="prov-store-bad">资源账本不可用：' + escapeHTML(state.storeError || "未知错误") +
        "。渠道数据保存在浏览器本地存储，请检查存储权限后重试。</div>";
      $("#pvBanner").hidden = true;
      return;
    }
    const s = state.summary || {};
    el.innerHTML =
      '<span class="prov-stat"><b>' + (s.total || 0) + "</b> 已登记</span>" +
      '<span class="prov-stat"><b>' + (s.enabled || 0) + "</b> 启用</span>" +
      '<span class="prov-stat"><b class="' + (s.healthy ? "ok" : "") + '">' + (s.healthy || 0) + "</b> 健康</span>" +
      '<span class="prov-stat"><b class="' + (s.cooling ? "warn" : "") + '">' + (s.cooling || 0) + "</b> 冷却中</span>" +
      '<span class="prov-stat"><b class="' + (s.pending_key ? "warn" : "") + '">' + (s.pending_key || 0) + "</b> 缺 Key</span>" +
      '<span class="prov-stat"><b>' + (s.events_24h || 0) + "</b> 24h 路由事件</span>";
  }

  /* ================= 渠道卡（F8 列表） ================= */
  function renderChannels() {
    const el = $("#chRows");
    if (!el) return;
    const all = state.channels;
    const miss = all.filter(chMissKey).length;
    const cnt = $("#chCount");
    if (cnt) cnt.textContent = all.length ? ("共 " + all.length + " 个 · 有 Key " + (all.length - miss) + " · 缺 Key " + miss) : "";
    if (!all.length) {
      el.innerHTML = '<div class="empty">还没有添加任何服务商 —— 点上方「＋ 添加服务商」，或从预设快速添加 / 一键导入内置通道。</div>';
      return;
    }
    el.innerHTML = all.map(channelCard).join("");
  }

  function channelCard(c) {
    const h = healthOf(c);
    const ms = effModels(c);
    const caps = [...new Set(ms.flatMap(m => m.alt_kind ? [m.kind, m.alt_kind] : [m.kind]))].map(capBadge).join("");
    const lat = c.health && c.health.latency_ms ? '<span class="ch-lat">' + c.health.latency_ms + "ms</span>" : "";
    const err = c.health && c.health.last_error ? '<span class="ch-err" title="' + escapeHTML(c.health.last_error) + '">' + escapeHTML(c.health.last_error) + "</span>" : "";
    const keyTxt = chMissKey(c) ? "缺 Key" : (c.api_key_masked || "");
    const id = escapeHTML(c.id);
    return '<div class="ch-card' + (c.enabled ? "" : " disabled") + '" data-id="' + id + '">' +
      '<div class="ch-head"><span class="dot ' + h.dot + '"></span><span class="ch-name">' + escapeHTML(c.name) + "</span>" +
      '<span class="tag">' + escapeHTML(c.provider) + '</span><span class="tag">' + escapeHTML(c.kind) + "</span>" +
      '<span class="' + tierClass(c.tier) + '">' + escapeHTML(c.tier || "T1") + "</span>" +
      '<span class="push-right dot-lbl">' + h.label + "</span></div>" +
      '<div class="ch-sub" title="' + escapeHTML(c.base_url) + '">' + escapeHTML(c.base_url) + "</div>" +
      (caps ? '<div class="ch-caps">' + caps + "</div>" : "") +
      '<div class="ch-meta"><span class="mask">' + escapeHTML(keyTxt) + "</span>" +
      '<span class="prio"><button data-act="prio-down" title="降优先级">▼</button>' + (c.priority ?? 0) + '<button data-act="prio-up" title="升优先级">▲</button></span>' +
      lat + err + "</div>" +
      (c.note ? '<div class="ch-sub ch-note">' + escapeHTML(c.note) + "</div>" : "") +
      '<div class="ch-ops">' +
      '<button class="soft-btn" data-act="edit">编辑</button>' +
      '<button class="soft-btn" data-act="key">换 Key</button>' +
      '<button class="soft-btn" data-act="test">测试</button>' +
      '<button class="soft-btn" data-act="toggle">' + (c.enabled ? "停用" : "启用") + "</button>" +
      '<button class="soft-btn danger" data-act="del">删除</button>' +
      "</div></div>";
  }

  /* 渠道卡操作委派（编辑 / 换 Key / 测试 / 启停 / 优先级 / 删除） */
  function bindCardOps() {
    const el = $("#chRows");
    if (!el) return;
    el.addEventListener("click", async (e) => {
      const btn = e.target.closest("button[data-act]");
      if (!btn) return;
      const card = btn.closest(".ch-card");
      const id = card && card.dataset.id;
      if (!id) return;
      const act = btn.dataset.act;
      if (act === "edit") openChannelDlg(id);
      else if (act === "key") openKeyDlg(id);
      else if (act === "test") testChannel(id);
      else if (act === "toggle") toggleChannel(id);
      else if (act === "del") delChannel(id);
      else if (act === "prio-up") bumpPriority(id, 10);
      else if (act === "prio-down") bumpPriority(id, -10);
    });
  }

  /* ================= 渠道表单（F8 CRUD + F12 元数据） ================= */
  function dlgShow(dlg) {
    if (dlg && typeof dlg.showModal === "function") { try { dlg.showModal(); } catch (e) { dlg.setAttribute("open", ""); } }
    else if (dlg) dlg.setAttribute("open", "");
  }
  function dlgClose(dlg) {
    if (dlg && typeof dlg.close === "function") { try { dlg.close(); } catch (e) { dlg.removeAttribute("open"); } }
    else if (dlg) dlg.removeAttribute("open");
  }

  function metaRowEls() { return Array.from(document.querySelectorAll("#ch-meta-rows .mm-row")); }
  function addMetaRow(m) {
    const row = document.createElement("div");
    row.className = "mm-row";
    const kinds = CAP_KINDS;
    row.innerHTML =
      '<input class="mm-name" placeholder="模型名" value="' + (m ? escapeHTML(m.name || "") : "") + '">' +
      '<select class="mm-kind" title="主能力">' + kinds.map(k => '<option value="' + k + '"' + (m && m.kind === k ? " selected" : "") + ">" + CAP_LABEL[k] + "</option>").join("") + "</select>" +
      '<select class="mm-alt" title="次要能力（可空）"><option value="">—</option>' + kinds.map(k => '<option value="' + k + '"' + (m && m.alt_kind === k ? " selected" : "") + ">+" + CAP_LABEL[k] + "</option>").join("") + "</select>" +
      '<input class="mm-prio" type="number" value="' + (m ? (m.priority ?? 50) : 50) + '" title="优先级（大者优先）">' +
      '<label class="mm-en"><input type="checkbox"' + (m && m.enabled === false ? "" : " checked") + ">启用</label>" +
      '<button type="button" class="mm-del" title="删除该行">✕</button>';
    row.querySelector(".mm-del").addEventListener("click", () => { row.remove(); syncModelsFromMeta(); });
    row.querySelectorAll("input, select").forEach(el => el.addEventListener("input", syncModelsFromMeta));
    $("#ch-meta-rows").appendChild(row);
    return row;
  }
  function syncModelsFromMeta() {
    $("#ch-models").value = metaRowEls().map(r => r.querySelector(".mm-name").value.trim()).filter(Boolean).join(", ");
  }
  function readMeta() {
    return metaRowEls().map(r => ({
      name: r.querySelector(".mm-name").value.trim(),
      kind: r.querySelector(".mm-kind").value,
      alt_kind: r.querySelector(".mm-alt").value,
      enabled: r.querySelector(".mm-en input").checked,
      priority: parseInt(r.querySelector(".mm-prio").value || "50", 10) || 50,
    })).filter(m => m.name);
  }
  function renderMetaRows(models, kind) {
    $("#ch-meta-rows").innerHTML = "";
    if (models && models.length) { for (const m of models) addMetaRow(m); }
    else addMetaRow({ name: "", kind: kind === "audio" ? "stt" : "text", priority: 50, enabled: true });
  }
  function syncZhipuHint() {
    const p = ($("#ch-provider").value || "").toLowerCase();
    $("#ch-zhipu-hint").hidden = !p.includes("zhipu");
  }

  function openChannelDlg(id) {
    const c = id ? state.channels.find(x => x.id === id) : null;
    state.editId = c ? c.id : null;
    $("#chDlgTitle").textContent = c ? ("编辑服务商 · " + c.name) : "添加服务商";
    $("#ch-dlg-err").textContent = "";
    $("#ch-provider").value = c ? c.provider : "";
    $("#ch-name").value = c ? c.name : "";
    $("#ch-base").value = c ? c.base_url : "";
    $("#ch-kind").value = c ? c.kind : "chat";
    $("#ch-tier").value = c ? (c.tier || "T1") : "T1";
    $("#ch-priority").value = c ? (c.priority ?? 50) : 50;
    $("#ch-enabled").value = c ? (c.enabled ? "1" : "0") : "1";
    $("#ch-note").value = c ? (c.note || "") : "";
    // Key：新建可填；编辑态不显示原值（轮换走「换 Key」）
    $("#ch-key").value = "";
    $("#ch-key-hint").hidden = !c;
    $("#ch-models").value = c ? (c.models || []).join(", ") : "";
    renderMetaRows(c ? effModels(c) : [], c ? c.kind : "chat");
    syncZhipuHint();
    dlgShow($("#channel-dlg"));
    setTimeout(() => $("#ch-provider").focus(), 80);
  }

  async function saveChannel() {
    const provider = $("#ch-provider").value.trim();
    const name = $("#ch-name").value.trim();
    const base = $("#ch-base").value.trim();
    const key = $("#ch-key").value.trim();
    const errEl = $("#ch-dlg-err");
    errEl.textContent = "";
    if (!provider) { errEl.textContent = "Provider 不能为空"; return; }
    if (!name) { errEl.textContent = "名称不能为空"; return; }
    if (!/^https?:\/\//.test(base)) { errEl.textContent = "Base URL 必须以 http:// 或 https:// 开头"; return; }
    // 前端先拦一次掩码（后端同样会校验）：截图里 5bc8...4bRY 绝不是真 Key
    if (key && looksMaskedKey(key)) { errEl.textContent = "API Key 看起来是掩码（含 *** / ...），请粘贴完整 Key；暂不登记请留空"; return; }
    const meta = readMeta();
    const body = {
      provider, name, base_url: base,
      kind: $("#ch-kind").value,
      tier: $("#ch-tier").value,
      models: meta.length ? meta.map(m => m.name) : $("#ch-models").value.split(/[,，;；\s]+/).map(s => s.trim()).filter(Boolean),
      models_meta: meta,
      priority: parseInt($("#ch-priority").value || "50", 10) || 50,
      enabled: $("#ch-enabled").value === "1",
      note: $("#ch-note").value.trim(),
    };
    if (!state.editId && key) body.api_key = key;
    try {
      if (state.editId) {
        await gw.updateChannel(state.editId, body);
        toast("已更新：" + name, "ok");
      } else {
        await gw.createChannel(body);
        toast("登记成功：" + name, "ok");
      }
      dlgClose($("#channel-dlg"));
      await load();
      onChange();
    } catch (e) {
      errEl.textContent = "保存失败：" + (e.message || e);
    }
  }

  /* ================= Key 轮换（F9） ================= */
  function openKeyDlg(id) {
    const c = state.channels.find(x => x.id === id);
    state.keyId = id;
    $("#key-dlg-err").textContent = "";
    $("#key-current").textContent = c ? (c.api_key_masked || "未登记") : "-";
    $("#key-new").value = "";
    dlgShow($("#key-dlg"));
    setTimeout(() => $("#key-new").focus(), 80);
  }
  async function saveKey() {
    const v = $("#key-new").value.trim();
    const errEl = $("#key-dlg-err");
    errEl.textContent = "";
    if (v && looksMaskedKey(v)) { errEl.textContent = "这看起来是掩码，请粘贴完整新 Key"; return; }
    try {
      await gw.updateChannel(state.keyId, { api_key: v });
      dlgClose($("#key-dlg"));
      toast(v ? "Key 已轮换" : "未修改（留空 = 不改变）", "ok");
      await load();
      onChange();
    } catch (e) {
      errEl.textContent = "轮换失败：" + (e.message || e);
    }
  }

  /* ================= 启停 / 优先级 / 删除 / 测试（F10 / F11） ================= */
  async function toggleChannel(id) {
    const c = state.channels.find(x => x.id === id);
    if (!c) return;
    try {
      await gw.updateChannel(id, { enabled: !c.enabled });
      toast(c.enabled ? "已停用" : "已启用（冷却已清除）", "ok");
      await load();
      onChange();
    } catch (e) { toast("操作失败：" + (e.message || e), "err"); }
  }
  async function bumpPriority(id, delta) {
    const c = state.channels.find(x => x.id === id);
    if (!c) return;
    try {
      await gw.updateChannel(id, { priority: (c.priority || 0) + delta });
      await load();
      onChange();
    } catch (e) { toast("调整失败：" + (e.message || e), "err"); }
  }
  async function delChannel(id) {
    if (!confirm("确定删除渠道 " + id + " ？（Key 一并从账本清除）")) return;
    try {
      await gw.removeChannel(id);
      toast("已删除：" + id, "ok");
      await load();
      onChange();
    } catch (e) { toast("删除失败：" + (e.message || e), "err"); }
  }
  async function testChannel(id) {
    toast("正在探测 " + id + " …");
    try {
      const j = await gw.testChannel(id);
      const ok = j.verdict === "healthy";
      toast(id + "：" + j.verdict + (j.upstream_status ? "（HTTP " + j.upstream_status + "，" + j.latency_ms + "ms）" : "") + (j.note ? " — " + j.note : ""), ok ? "ok" : "err");
      await load();
      onChange();
    } catch (e) { toast("探测失败：" + (e.message || e), "err"); }
  }

  /* ================= 预设（F13） ================= */
  function renderPresets(filter) {
    const el = $("#pvPresets");
    if (!el) return;
    const f = (filter || "").toLowerCase();
    const match = p => !f || ((p.provider || "") + " " + (p.name || "")).toLowerCase().includes(f);
    const list = state.presets.map((p, i) => ({ p, i })).filter(x => match(x.p));
    if (!list.length) {
      el.innerHTML = '<div class="empty">' + (state.presets.length ? "无匹配预设" : "预设加载中…（随渠道列表一同返回）") + "</div>";
      return;
    }
    const card = ({ p, i }) =>
      '<div class="preset"><div class="preset-n">' + escapeHTML(p.name) + ' <span class="tag">' + escapeHTML(p.provider) + '</span><span class="' + tierClass(p.tier) + '">' + escapeHTML(p.tier || "T1") + '</span><span class="tag">' + escapeHTML(p.kind || "chat") + "</span></div>" +
      '<div class="preset-f">' + escapeHTML(p.free_tier || "") + "</div>" +
      '<div class="preset-row"><button class="soft-btn" data-preset="' + i + '">添加</button>' +
      (p.doc ? '<a class="soft-btn" href="' + escapeHTML(p.doc) + '" target="_blank" rel="noopener">文档</a>' : "") + "</div></div>";
    el.innerHTML = list.map(card).join("");
  }
  function presetFill(i) {
    const p = state.presets[i];
    if (!p) return;
    openChannelDlg(null);
    $("#ch-provider").value = p.provider || "";
    $("#ch-name").value = p.name || "";
    $("#ch-base").value = p.base_url || "";
    $("#ch-kind").value = p.kind || "chat";
    $("#ch-tier").value = p.tier || "T1";
    $("#ch-note").value = p.free_tier || "";
    renderMetaRows((p.models || []).map(name => ({
      name,
      kind: (p.kind === "audio") ? "stt" : (/glm-4v|glm-4\.1v|-vl|vision|gemma/i.test(name) ? "vision" : "text"),
      alt_kind: "", priority: 50, enabled: true,
    })), p.kind || "chat");
    syncModelsFromMeta();
    syncZhipuHint();
    setTimeout(() => $("#ch-key").focus(), 80);
  }

  /* ================= 一键载入默认 / 一键导入 / 导出（F14 / F15 / F16） ================= */
  async function seed() {
    try {
      const j = await gw.seed();
      toast("已载入 " + (j.added != null ? j.added : "") + " 条默认渠道（国内优先 + 海外兜底，Key 待登记）", "ok");
      await load();
      onChange();
    } catch (e) { toast("载入失败：" + (e.message || e), "err"); }
  }

  /* 从站点根目录的内置 gateway-channels.json 拉取通道清单并逐条登记。
     已存在的 id 自动跳过（避免重复）；通道不含真实 Key，需到渠道卡补登。 */
  async function importBundled() {
    try {
      const r = await fetch("./gateway-channels.json", { cache: "no-store" });
      if (!r.ok) throw new Error("读取内置通道文件失败（HTTP " + r.status + "）");
      const data = await r.json();
      const list = Array.isArray(data.channels) ? data.channels : [];
      if (!list.length) { toast("内置通道文件为空", "err"); return; }
      const exIds = new Set(state.channels.map(c => c.id));
      let added = 0, skipped = 0;
      for (const ch of list) {
        if (exIds.has(ch.id)) { skipped++; continue; }
        const body = Object.assign({}, ch);
        // 去掉只读/服务端派生字段，交给创建接口重新生成
        delete body.health; delete body.api_key_masked;
        delete body.created_at; delete body.updated_at;
        await gw.createChannel(body);
        added++;
      }
      toast("已导入 " + added + " 个内置通道" + (skipped ? "，跳过 " + skipped + " 个已存在" : ""), "ok");
      await load();
      onChange();
    } catch (e) {
      toast("导入失败：" + (e.message || e), "err");
    }
  }

  function exportJson() {
    const data = { exported_at: new Date().toISOString(), note: "api_key 均为掩码，不含明文", channels: state.channels };
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "gateway-channels.json";
    a.click();
    URL.revokeObjectURL(a.href);
    toast("已导出（Key 为掩码，不含明文）", "ok");
  }

  /* ================= 批量体检（F17） ================= */
  async function selftestAll() {
    if (state.selfBusy) return;
    const seen = new Set();
    const list = [];
    for (const c of state.channels) {
      if (!c.enabled || c.kind !== "chat" || chMissKey(c)) continue;
      for (const m of effModels(c)) {
        if (m.enabled === false) continue;
        if (!m.name || seen.has(m.name)) continue;
        seen.add(m.name); list.push(m.name);
      }
    }
    const body = $("#selftest-body");
    if (!list.length) { body.textContent = "没有可用渠道（需启用且登记 Key）"; dlgShow($("#selftest-dlg")); return; }
    const targets = list.slice(0, SELF_LIMIT);
    state.selfBusy = true;
    dlgShow($("#selftest-dlg"));
    body.textContent = "体检 " + targets.length + " 个模型…";
    const out = [];
    for (let i = 0; i < targets.length; i++) {
      const model = targets[i];
      body.textContent = "体检 " + (i + 1) + "/" + targets.length + "：" + model + " …";
      try {
        const j = await gw.testModel(model);
        const r = (j.results || [])[0] || {};
        const bits = [j.verdict];
        if (r.http_status) bits.push("HTTP " + r.http_status);
        if (r.latency_ms != null) bits.push(r.latency_ms + "ms");
        if (r.served_model && !r.served_matches) bits.push("上游换成 " + r.served_model);
        if (r.note) bits.push(r.note);
        if (r.error) bits.push(r.error);
        out.push((j.verdict === "ok" ? "✅" : j.verdict === "channel-not-ready" ? "⏸" : "❌") + " " + model + "  —  " + bits.join(" · "));
      } catch (e) {
        out.push("❌ " + model + "  —  " + (e.message || e));
      }
    }
    const ok = out.filter(s => s.startsWith("✅")).length;
    body.textContent = "可用 " + ok + "/" + targets.length + "\n\n" + out.join("\n");
    toast("体检完成：" + ok + "/" + targets.length + " 个模型可用", ok ? "ok" : "warn");
    state.selfBusy = false;
  }

  /* ================= 用量与对账（F18） ================= */
  async function loadUsage() {
    const el = $("#usageTables");
    if (!el) return;
    el.innerHTML = '<div class="empty">加载中…</div>';
    try {
      const d = await gw.usage();
      renderUsage(d);
    } catch (e) {
      el.innerHTML = '<div class="empty">用量加载失败：' + escapeHTML(e.message || e) + "</div>";
    }
  }
  function usageTable(caption, rows, keyHeader) {
    if (!rows || !rows.length) return '<span class="usage-cap">' + escapeHTML(caption) + '</span><div class="empty">暂无数据</div>';
    const body = rows.map(o => {
      const v = o.value || {};
      const cost = fmtCost(v.cost) + (v.currency ? " " + escapeHTML(v.currency) : "") + (v.mixed_currency ? " ⚠混币种" : "");
      return "<tr><td>" + escapeHTML(o.key) + '</td><td class="num">' + fmtInt(v.requests) + '</td><td class="num">' +
        (Number(v.success_rate || 0) * 100).toFixed(1) + '%</td><td class="num">' + fmtInt(v.total_tokens) + '</td><td class="num">' +
        fmtInt(v.cache_hit_tokens) + '</td><td class="num">' + (v.avg_latency_ms || 0) + 'ms</td><td class="num">' + cost + "</td></tr>";
    }).join("");
    return '<span class="usage-cap">' + escapeHTML(caption) + '</span><div class="usage-tbl-wrap"><table class="usage-tbl"><thead><tr><th>' +
      escapeHTML(keyHeader) + "</th><th>请求</th><th>成功率</th><th>token</th><th>缓存命中</th><th>平均延迟</th><th>估算成本</th></tr></thead><tbody>" +
      body + "</tbody></table></div>";
  }
  function renderUsage(d) {
    const t = d.totals || {};
    const cards = $("#usageCards");
    if (cards) {
      cards.innerHTML =
        statCard("请求", fmtInt(t.requests)) +
        statCard("成功率", t.requests ? (Number(t.success_rate || 0) * 100).toFixed(1) + "%" : "—") +
        statCard("token", fmtInt(t.total_tokens)) +
        statCard("缓存命中", fmtInt(t.cache_hit_tokens)) +
        statCard("平均延迟", t.requests ? (t.avg_latency_ms + "ms") : "—") +
        statCard("估算成本", fmtCost(t.cost) + (t.currency ? " " + t.currency : ""), t.mixed_currency ? "存在多种币种，成本不可直接相加" : "");
    }
    state.usageRaw = { provider: d.by_provider || [], model: d.by_model || [], day: d.by_day || [] };
    state.usagePage = 1;
    renderUsageTables();
  }
  function statCard(label, value, title) {
    return '<div class="usage-card"' + (title ? ' title="' + escapeHTML(title) + '"' : "") + '><span class="usage-card-v">' + escapeHTML(value) + '</span><span class="usage-card-l">' + escapeHTML(label) + "</span></div>";
  }
  function renderUsageTables() {
    const box = $("#usageTables");
    if (!box) return;
    const n = Math.max(state.usageRaw.provider.length, state.usageRaw.model.length, state.usageRaw.day.length);
    const pages = Math.max(1, Math.ceil(n / US_PAGE_SIZE));
    if (state.usagePage > pages) state.usagePage = pages;
    if (state.usagePage < 1) state.usagePage = 1;
    const cur = state.usagePage;
    const sl = arr => (arr || []).slice((cur - 1) * US_PAGE_SIZE, cur * US_PAGE_SIZE);
    box.innerHTML =
      usageTable("按源", sl(state.usageRaw.provider), "源") +
      usageTable("按 源 × 模型", sl(state.usageRaw.model), "源|模型") +
      usageTable("按自然日", sl(state.usageRaw.day), "日期");
    const pg = $("#usagePager");
    if (pg) {
      if (pages > 1) {
        pg.hidden = false;
        $("#usagePageN").textContent = cur + " / " + pages + " · 每页 " + US_PAGE_SIZE + " 行";
        $("#usagePrev").disabled = cur <= 1;
        $("#usageNext").disabled = cur >= pages;
      } else pg.hidden = true;
    }
  }
  async function usageExportCsv() {
    try {
      const csv = await gw.usageExportCsv();
      const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = "gateway-usage-export.csv";
      a.click();
      URL.revokeObjectURL(a.href);
      toast("已导出对账 CSV", "ok");
    } catch (e) { toast("导出失败：" + (e.message || e), "err"); }
  }

  /* ================= 装配 ================= */
  function init(onChangeCb) {
    if (typeof onChangeCb === "function") onChange = onChangeCb;
    $("#pvAdd").onclick = () => openChannelDlg(null);
    $("#pvRefresh").onclick = () => { load(); };
    $("#pvSeed").onclick = seed;
    $("#pvImport").onclick = importBundled;
    $("#pvExport").onclick = exportJson;
    $("#pvSelftest").onclick = selftestAll;
    $("#pvUsageExport").onclick = usageExportCsv;
    $("#pvPresetSearch").addEventListener("input", e => renderPresets(e.target.value));
    $("#pvPresets").addEventListener("click", e => {
      const b = e.target.closest("button[data-preset]");
      if (b) presetFill(Number(b.dataset.preset));
    });
    // 渠道对话框
    $("#ch-save").onclick = saveChannel;
    $("#ch-cancel").onclick = () => dlgClose($("#channel-dlg"));
    $("#ch-close").onclick = () => dlgClose($("#channel-dlg"));
    $("#ch-meta-add").onclick = () => { addMetaRow(null); syncModelsFromMeta(); };
    $("#ch-provider").addEventListener("input", syncZhipuHint);
    // 换 Key 对话框
    $("#key-save").onclick = saveKey;
    $("#key-cancel").onclick = () => dlgClose($("#key-dlg"));
    $("#key-close").onclick = () => dlgClose($("#key-dlg"));
    // 批量体检对话框
    $("#selftest-close").onclick = () => dlgClose($("#selftest-dlg"));
    // 用量翻页
    $("#usagePrev").onclick = () => { state.usagePage--; renderUsageTables(); };
    $("#usageNext").onclick = () => { state.usagePage++; renderUsageTables(); };
    bindCardOps();
  }

  global.QuakeProviders = {
    state, init, load,
    // 供测试/调试直接调用
    openChannelDlg, saveChannel, openKeyDlg, saveKey, toggleChannel, delChannel, testChannel,
    seed, importBundled, exportJson, selftestAll, healthOf, looksMaskedKey,
  };
})(window);
