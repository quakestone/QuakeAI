/* ============================================================================
   QuakeWorkflow —— 工作流（B3 / F24 工作流参数）
   ----------------------------------------------------------------------------
   与旧 UI（admin_ui.html WfCfg + WF 插件）同口径迁移：
   - WfCfg：localStorage `gw_wf_cfg`（键与字段不变，旧数据无缝读取）
   - 四个开关全部接真实行为（无占位）：
     ① usage  用量记账：消息元信息 token 徽标 + 会话累计（app.js 读 cfg.usage）
     ② title  LLM 总结历史标题（answers≥2 且非用户手动命名；失败保留截断兜底）
     ③ memory 会话记忆压缩：token 超阈时 LLM 压缩 [summary_pointer, len-keep_recent)
               区域为长期记忆，保留最近若干条原文
     ④ format 附件显示规格：缩略图/视频容器尺寸 → CSS 变量实时下发
   - 模型调用走网关自身 chat 端点（非流式，计入用量账本）；服务端零存储。
   - 说明：旧架构的 WASM 工作流内核（kernel.wasm，CF Worker 托管）在纯静态
     部署下不存在；FSM 的「何时压缩」决策在此以同等阈值的 JS 逻辑实现，
     阈值参数与持久化形状不变（memfsm 状态字段兼容旧数据）。
   依赖：adapters（chat）/ components（toast）。
   脚本加载序：… → prompts → voice → workflow → app。
   ==========================================================================*/
(function (global) {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const { toast } = global.QuakeComponents;
  const gw = global.QuakeAdapters.gateway;

  const WfCfg = {
    KEY: "gw_wf_cfg",
    def: {
      usage: true, title: true, memory: true, format: true,
      model: "",                       // 标题/记忆模型（留空 = 网关自动选路）
      thumb: 96, vw: 260, vh: 190,     // 附件显示规格
      memMaxTokens: 4000,              // 软阈：超此触发记忆压缩
      memHardMax: 16000,               // 硬上限：超此强制压缩（不等软阈）
      memKeepRecent: 8,                // 压缩时保留最近 N 条原文
    },
    load() {
      try {
        const raw = JSON.parse(localStorage.getItem(this.KEY) || "{}");
        return { ...this.def, ...raw };
      } catch { return { ...this.def }; }
    },
    save(c) { try { localStorage.setItem(this.KEY, JSON.stringify(c)); } catch { /* 存储不可用：仅本次生效 */ } },
  };
  let wfCfg = WfCfg.load();

  /* ================= 附件显示规格（CSS 变量实时下发） ================= */
  function applyFormat(thumb, vw, vh) {
    const r = document.documentElement.style;
    r.setProperty("--att-thumb", thumb + "px");
    r.setProperty("--att-video-w", vw + "px");
    r.setProperty("--att-video-h", vh + "px");
  }
  /// 旧 UI content_format 插件同口径：带视频时视频容器适度放大（≥300px）
  function applyFormatFor(atts) {
    const hasVideo = (atts || []).some(a => a.kind === "video");
    const vw = hasVideo ? Math.max(Number(wfCfg.vw) || 260, 300) : (Number(wfCfg.vw) || 260);
    applyFormat(Number(wfCfg.thumb) || 96, vw, Number(wfCfg.vh) || 190);
  }

  /* ================= LLM 调用（走网关自身，计入账本；失败返回 ""） ================= */
  async function wfChat(messages, model) {
    const body = { messages, stream: false };
    if (model) body.model = model;
    try {
      const r = await gw.chat({ messages: body.messages, model: body.model });
      return r.content || "";
    } catch (e) { return ""; }
  }

  /* ================= 插件①：用量记账（B1 已累加 c.tokens/c.answers；此处供状态查询） ================= */
  function usageOf(c) {
    return { total: (c && c.tokens) || 0, answers: (c && c.answers) || 0 };
  }

  /* ================= 插件②：LLM 总结历史标题 ================= */
  async function autoTitle(c) {
    if (!wfCfg.title) return { skipped: "off" };
    if (!c) return { skipped: "no-convo" };
    if (c.title_auto === false) return { skipped: "用户已手动命名" }; // 不覆盖用户命名
    const answers = Number(c.answers) || 0;
    if (answers < 2) return { skipped: "answers<2" };                // 至少两轮再总结
    const msgs = (c.msgs || []).filter(m => (m.role === "user" || m.role === "assistant") && m.content && !m.error);
    const transcript = msgs.slice(-12).map(m => (m.role === "user" ? "用户" : "AI") + "：" + m.content).join("\n");
    const sys = "用不超过 18 个汉字概括这段对话的核心主题，只输出标题本身，不要标点、不要引号、不要解释、不要换行。";
    const title = (await wfChat([{ role: "system", content: sys }, { role: "user", content: transcript }], wfCfg.model))
      .replace(/[《》""''‘’「」【】\s]/g, "").slice(0, 20);
    if (title) { c.title = title; c.title_auto = true; }
    return { title: title || (c.title || "") };
  }

  /* ================= 插件③：会话记忆更新（token 阈值自适应压缩） =================
     FSM 决策（memfsm）在纯静态部署下以同等阈值的 JS 逻辑实现：
     token 计数跨轮累加；软阈触发、硬阈强制；压缩区域 [summary_pointer,
     len - memKeepRecent)，LLM 压缩为 ≤400 字要点并与旧记忆合并。 */
  async function memoryUpdate(c, turnTokens) {
    if (!wfCfg.memory) return { skipped: "off" };
    if (!c) return { skipped: "no-convo" };
    const fsm = c.memfsm || { token_count: 0, summary_pointer: 0, msg_count: 0 };
    fsm.token_count = (Number(fsm.token_count) || 0) + (Number(turnTokens) || 0);
    const soft = Number(wfCfg.memMaxTokens) || 4000;
    const hard = Number(wfCfg.memHardMax) || 16000;
    if (fsm.token_count < soft && fsm.token_count < hard) {
      c.memfsm = fsm;
      return { ok: true, compressed: false, token_count: fsm.token_count };
    }
    const filtered = (c.msgs || []).filter(m => (m.role === "user" || m.role === "assistant") && m.content && !m.error);
    const keepFrom = Math.max(Number(fsm.summary_pointer) || 0, filtered.length - (Number(wfCfg.memKeepRecent) || 8));
    if (keepFrom > (Number(fsm.summary_pointer) || 0)) {
      const region = filtered.slice(Number(fsm.summary_pointer) || 0, keepFrom);
      const transcript = region.map(m => (m.role === "user" ? "用户" : "AI") + "：" + m.content).join("\n");
      const prev = c.memory ? ("已有记忆：\n" + c.memory + "\n\n") : "";
      const sys = "你是会话记忆压缩器。从对话中提取长期有用的信息（用户偏好、项目背景、待办、事实），用不超过 200 字的中文要点列表记录。只输出要点，不要寒暄。";
      const mem = (await wfChat([{ role: "system", content: sys }, { role: "user", content: prev + "新对话：\n" + transcript }], wfCfg.model)).trim().slice(0, 400);
      if (mem) { c.memory = mem; c.memory_at = Date.now(); }
      // LLM 失败：仍推进边界、减半计数，避免卡在阈值反复触发
      fsm.summary_pointer = keepFrom;
      fsm.token_count = mem ? Math.max(1, Math.round(mem.length / 1.6)) : Math.max(1, Math.round(fsm.token_count * 0.5));
      fsm.msg_count = filtered.length;
      c.memfsm = fsm;
      return { ok: true, compressed: !!mem, token_count: fsm.token_count };
    }
    // 阈值超了但无新增可折叠内容（keep_recent 很大）：只压计数，避免死循环
    fsm.token_count = Math.max(1, Math.round(fsm.token_count * 0.5));
    c.memfsm = fsm;
    return { ok: true, compressed: false, token_count: fsm.token_count };
  }

  /* ================= chat.completed 统一入口（app.js 在回答完成后调用） ================= */
  async function runWorkflow(o) {
    o = o || {};
    const c = o.convo;
    if (!c) return;
    try {
      // ④ 附件显示规格：立即生效（CSS 变量）
      if (wfCfg.format) applyFormatFor(o.atts || []);
      // ② LLM 总结标题
      await autoTitle(c);
      // ③ 会话记忆压缩
      await memoryUpdate(c, (o.usage && o.usage.total_tokens) || 0);
    } catch (e) { /* 工作流失败绝不打断聊天主链路 */ }
  }

  /* ================= 视图 ================= */
  function fillUI() {
    const c = wfCfg;
    $("#wf-on-usage").checked = !!c.usage;
    $("#wf-on-title").checked = !!c.title;
    $("#wf-on-memory").checked = !!c.memory;
    $("#wf-on-format").checked = !!c.format;
    $("#wf-mem-maxtok").value = c.memMaxTokens;
    $("#wf-mem-hard").value = c.memHardMax;
    $("#wf-mem-keep").value = c.memKeepRecent;
    $("#wf-thumb").value = c.thumb;
    $("#wf-vw").value = c.vw;
    $("#wf-vh").value = c.vh;
    fillModelSelect();
    showMemory();
  }
  /// 标题/记忆模型下拉：从可用文本模型填充（与对话页模型选择同源）
  function fillModelSelect() {
    const sel = $("#wf-model-sel");
    if (!sel) return;
    const models = (global.QuakeAdapters.usableChatModels ? global.QuakeAdapters.usableChatModels(global.QuakeProviders.state.channels || []) : []) || [];
    sel.innerHTML = '<option value="">自动（网关选路）</option>' +
      models.map(m => '<option value="' + String(m.name).replace(/[&<>"']/g, s => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[s])) + '">' + String(m.name).replace(/[&<>"']/g, s => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[s])) + "</option>").join("");
    sel.value = wfCfg.model || "";
  }
  function showMemory() {
    const el = $("#wf-mem-view");
    if (!el) return;
    const c = (global.QuakeApp && global.QuakeApp.currentConvo && global.QuakeApp.currentConvo()) || null;
    el.textContent = (c && c.memory) ? c.memory : "（无）";
  }
  function clampNum(v, min, max, def) {
    const n = Number(v);
    if (!isFinite(n)) return def;
    return Math.min(max, Math.max(min, Math.round(n)));
  }
  function save() {
    wfCfg = {
      ...wfCfg,
      usage: $("#wf-on-usage").checked,
      title: $("#wf-on-title").checked,
      memory: $("#wf-on-memory").checked,
      format: $("#wf-on-format").checked,
      model: $("#wf-model-sel").value || "",
      thumb: clampNum($("#wf-thumb").value, 48, 200, 96),
      vw: clampNum($("#wf-vw").value, 120, 640, 260),
      vh: clampNum($("#wf-vh").value, 90, 640, 190),
      memMaxTokens: clampNum($("#wf-mem-maxtok").value, 256, 32000, 4000),
      memHardMax: clampNum($("#wf-mem-hard").value, 512, 64000, 16000),
      memKeepRecent: clampNum($("#wf-mem-keep").value, 2, 40, 8),
    };
    WfCfg.save(wfCfg);
    if (wfCfg.format) applyFormatFor([]); // 立即生效
    fillUI();
    toast("工作流设置已保存", "ok");
  }
  /// 进入视图时刷新：参数回填 + 模型下拉 + 当前会话记忆
  function refresh() { fillUI(); }

  function init() {
    $("#wfSave").onclick = save;
    $("#wfRefresh").onclick = refresh;
    if (wfCfg.format) applyFormatFor([]); // 启动即按持久化规格生效
  }

  global.QuakeWorkflow = {
    state: wfCfg, init, refresh, save, runWorkflow, applyFormatFor, wfChat,
    get cfg() { return wfCfg; },
    // 供测试/调试直接调用
    autoTitle, memoryUpdate,
  };
  // app.js 装配后注入 currentConvo 查询（工作流页「当前会话记忆」展示用）
  global.QuakeApp = global.QuakeApp || {};
})(window);
