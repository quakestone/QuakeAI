/* ============================================================
   QuakeAI app.js —— 壳状态 / 视图 / 真实业务接线（B1 对话全量）
   边界：只调 QuakeAdapters.gateway（适配层）、QuakeStore（持久化）与
   各业务模块（Points/Skills/Attachments/Share/Magic），不直接碰 WASM。
   B1 范围：流式/停止/重试/重生成/修改/模型选择（积分排序）/附件
   （图音视文 + 上限 + 缩略图 + IDB）/skills 三流/上下文边界/积分账本/
   魔法指令/调试浮层/控制台面板/分享（链接 + 二维码）。
   ============================================================ */
(function () {
  "use strict";

  const $ = s => document.querySelector(s), $$ = s => Array.from(document.querySelectorAll(s));
  const { gateway, usableChatModels } = window.QuakeAdapters;
  const { message, streamMessage, toast, escapeHTML, healthBadge } = window.QuakeComponents;
  const { Points, AUTO_KEY, applyRoutePoints, ctxResetOn } = window.QuakePoints;
  const Magic = window.QuakeMagic;
  const Skills = window.QuakeSkills;
  const Att = window.QuakeAttachments;
  const Share = window.QuakeShare;
  const Providers = window.QuakeProviders;
  const Prompts = window.QuakePrompts;
  const Voice = window.QuakeVoice;
  const Workflow = window.QuakeWorkflow;
  const Upstream = window.QuakeUpstream;
  const NetMon = window.QuakeNetMon;
  const Store = window.QuakeStore;
  const dbg = (...a) => Magic.dbg(...a);

  const VIEW_NAMES = { home: "智能首页", chat: "AI 对话", providers: "服务商", prompts: "提示词", voice: "语音与多媒体", workflow: "工作流", upstream: "上游与接入" };
  const state = {
    view: "home",
    currentId: null,
    conversations: [],       // 内存全量（IDB hydrate，按 updated 倒序）
    channels: [],
    models: [],              // usableChatModels(channels)
    model: Store.config.get("gw_model", ""),
    health: null,
    streaming: false,
    aborter: null,
    modalSave: null,
    modalReturn: null,       // 浮层打开前的焦点元素（关闭时还原，a11y）
    jarvisReturn: null,      // 同上：Jarvis 浮层
    attachments: [],         // 输入区附件实例（含 file/dataUrl，瞬态）
    editCtx: null,           // 修改态 { convoId, idx }
    streamGen: 0,            // 流代号：新会话/新发送抢占时，旧流 finally 只落库不碰 UI
    stream: null,            // 当前 live 流式节点（renderMessages 对流式消息只挂它，单节点原则）
    speakReply: false,       // 语音助手浮层发起的对话：回答完成按 TTS 链播报
    micBusy: false,          // 语音助手浮层麦克风状态
  };

  /* ================= 持久化 ================= */
  function persistConvo(c) { Store.IDB.putConvo(c); }
  function persistMsg(c, idx, m) { Store.IDB.putMsg(c.id, idx, m); }
  const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
  const persistMsgThrottled = debounce((c, idx, m) => persistMsg(c, idx, m), 700);

  function id() { return "c_" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }
  function current() { return state.conversations.find(c => c.id === state.currentId) || null; }
  function fmtTok(n) { return n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n); }

  /* ---------- 截断 idx 及其后所有消息（含落库）。
     重试 / 修改 / 重新生成都靠它把「这条之后」清掉再重发，
     否则会再追加一条同样的提问。IDB 位置键：删掉旧尾，同位置重写即覆盖。 ---------- */
  function truncateFrom(c, idx) {
    if (!c || idx < 0 || idx >= c.msgs.length) return;
    for (let k = idx; k < c.msgs.length; k++) Store.IDB.deleteMsg(c.id, k);
    c.msgs = c.msgs.slice(0, idx);
    // 上下文边界若落在被删区间，前移到截断点：否则 history 切片会退化成「只剩最后一条」
    if ((c.ctxStart || 0) > idx) c.ctxStart = idx;
    c.updated = Date.now();
    persistConvo(c);
  }

  function ensureConvo() {
    let c = current();
    if (!c) {
      c = { id: id(), title: "新对话", model: state.model, ctxStart: 0, msgs: [], updated: Date.now() };
      state.conversations.unshift(c);
      state.currentId = c.id;
      persistConvo(c);
    }
    return c;
  }

  /* ================= 渲染：会话列表 / 首页最近 ================= */
  function renderConversations(filter) {
    filter = (filter || "").toLowerCase();
    const list = $("#conversationList");
    list.innerHTML = "";
    state.conversations
      .filter(c => (c.title + " " + (c.msgs || []).map(m => m.content || "").join(" ")).toLowerCase().includes(filter))
      .forEach(c => {
        const b = document.createElement("button");
        b.className = "conversation-item" + (c.id === state.currentId ? " active" : "");
        b.innerHTML = "<strong>" + escapeHTML(c.title || "新对话") + "</strong><span>" + escapeHTML((c.msgs && c.msgs.at(-1) && c.msgs.at(-1).content) || "开始一段新对话") + "</span>";
        b.onclick = () => openConversation(c.id);
        list.append(b);
      });
    renderHomeRecent();
  }

  function renderHomeRecent() {
    const el = $("#homeRecent");
    if (!el) return;
    el.innerHTML = "";
    const recent = state.conversations.slice(0, 4);
    if (!recent.length) {
      el.innerHTML = '<div class="continue-item"><div class="continue-icon">✳</div><div><strong>你的对话会显示在这里</strong><span>发送第一条消息，开始构建你的工作记录。</span></div></div>';
      return;
    }
    recent.forEach(c => {
      const b = document.createElement("button");
      b.className = "continue-item";
      b.innerHTML = '<div class="continue-icon">◌</div><div><strong>' + escapeHTML(c.title || "新对话") + "</strong><span>" + escapeHTML((c.msgs && c.msgs.at(-1) && c.msgs.at(-1).content) || "尚无消息") + "</span></div>";
      b.onclick = () => openConversation(c.id);
      el.append(b);
    });
  }

  /* ================= 渲染：消息列 ================= */
  function renderMessages() {
    const el = $("#messages");
    el.innerHTML = "";
    const c = current();
    $("#chatTitle").textContent = (c && c.title) || "新对话";
    if (!c || !c.msgs.length) {
      el.innerHTML = '<div class="empty-state"><div class="empty-icon">✳</div><h2>从一个问题开始</h2><p>QuakeAI 会把问题路由到可用的免费模型渠道。</p></div>';
      return;
    }
    c.msgs.forEach((m, i) => {
      // 流式中的消息：DOM 唯一来源是 live stream 节点（单节点原则）——
      // 渲染发生在建流前 → 跳过（sendWith 随后 append）；渲染重入于流式中 →
      // 把 live 节点挂回正确位置（append 会移动已连接的节点）。
      if (m.streaming) {
        if (state.stream && m.id === state.stream.id) el.append(state.stream.el);
        return;
      }
      if (m.role === "divider") {
        el.append(message({ role: "divider", text: m.text, id: m.id }));
        return;
      }
      const meta = [];
      if (m.role === "assistant") {
        if (m.channel) meta.push("via " + escapeHTML(m.channel));
        if (m.model) meta.push("模型 " + escapeHTML(m.model));
        if (m.attempts && String(m.attempts).split(";").length > 1) meta.push("⤻ 已自动切换");
        if (Workflow.cfg.usage && m.usage && m.usage.total_tokens != null) meta.push("⛁ " + fmtTok(m.usage.total_tokens));
        if (m.ttfb != null) meta.push("首字 " + (m.ttfb / 1000).toFixed(1) + "s");
      }
      if (m.error) meta.push('<span class="hb bad">' + escapeHTML(m.error) + "</span>");
      const retry = m.role === "assistant" && i >= 1 && c.msgs[i - 1].role === "user";
      el.append(message({
        role: m.role, text: m.content || "", id: m.id,
        meta: meta.length ? meta.join(" · ") : "",
        atts: m.attachments, retry,
      }));
    });
    el.scrollTop = el.scrollHeight;
  }

  /* ================= 视图 ================= */
  function setView(view) {
    state.view = view;
    $("#homeView").classList.toggle("hidden", view !== "home");
    $("#chatView").classList.toggle("hidden", view !== "chat");
    $("#providersView").classList.toggle("hidden", view !== "providers");
    $("#promptsView").classList.toggle("hidden", view !== "prompts");
    $("#voiceView").classList.toggle("hidden", view !== "voice");
    $("#workflowView").classList.toggle("hidden", view !== "workflow");
    $("#upstreamView").classList.toggle("hidden", view !== "upstream");
    $("#breadcrumb").textContent = VIEW_NAMES[view] || "工作台";
    $$(".nav-item[data-view]").forEach(b => {
      b.classList.toggle("active", b.dataset.view === view);
      if (b.dataset.view === view) b.setAttribute("aria-current", "page");
      else b.removeAttribute("aria-current");
    });
    if (view === "chat") renderMessages();
    if (view === "providers") Providers.load();   // 每次进入拉最新账本（load 有 in-flight 去重）
    if (view === "prompts") Prompts.load();       // 每次进入拉最新账本（含使用计数）
    if (view === "voice") Voice.load();           // 链路状态 + 能力模型下拉刷新
    if (view === "workflow") Workflow.refresh();  // 记忆展示 + 参数回填
    if (view === "upstream") Upstream.load();     // 代理/鉴权/设备 Key 状态 + 行渲染
    closeSidebar();
    window.scrollTo({ top: 0, behavior: "smooth" });
  }
  function openConversation(cid) {
    if (state.editCtx) cancelEdit(true);       // 切会话：被动撤修改态（清回填内容）
    state.currentId = cid;
    renderConversations();
    setView("chat");
  }

  /* ================= 网关状态（真实数据） ================= */
  function gwDot(cls) {
    $("#gwSideDot").className = "status-dot " + (cls || "");
    $("#gwTopDot").className = "status-dot " + (cls || "");
  }
  async function refreshGateway() {
    $("#gwSideStatusText").textContent = "网关启动中…";
    $("#gwTopLabel").textContent = "网关启动中…";
    try {
      const gw = await gateway.init();
      const [health, channels] = await Promise.all([gateway.health(), gateway.channels()]);
      state.health = health;
      state.channels = channels;
      state.models = usableChatModels(channels);
      renderModelButtons();
      renderHealthCards();
      const usable = usableCount();
      const ok = state.models.length > 0;
      gwDot(ok ? "ok" : "warn");
      $("#gwSideStatusText").textContent = "网关就绪 · " + usable.usable + "/" + usable.total + " 渠道可用";
      $("#gwTopLabel").textContent = ok ? "网关就绪 · " + state.models.length + " 个可用模型" : "网关就绪 · 暂无可用渠道";
    } catch (e) {
      dbg("[load] 网关异常:", e && (e.stack || e.message) || e);
      gwDot("bad");
      $("#gwSideStatusText").textContent = "网关异常：" + e.message;
      $("#gwTopLabel").textContent = "网关异常";
      renderHealthCards(e);
    }
  }
  function usableCount() {
    const now = Date.now();
    const usable = state.channels.filter(c => c.enabled && c.kind === "chat" && c.api_key_masked && c.api_key_masked !== "未登记"
      && !(c.health && c.health.cooldown_until_ms && c.health.cooldown_until_ms > now)).length;
    return { usable, total: state.channels.length };
  }
  function renderHealthCards(err) {
    const grid = $("#healthGrid");
    if (err) {
      grid.innerHTML =
        '<article class="insight-card card"><div class="card-label"><span>◌</span> 网关状态</div><strong class="insight-title">加载失败</strong><p>' + escapeHTML(err.message) + '</p></article>' +
        '<article class="insight-card card"><div class="card-label"><span>↗</span> 下一步</div><strong class="insight-title">检查 WASM 产物</strong><p>确认 build/assets/wasm/ 随页面一起部署，然后刷新重试。</p></article>';
      return;
    }
    const { usable, total } = usableCount();
    const enabled = state.channels.filter(c => c.enabled).length;
    const healthy = state.channels.filter(c => c.health && c.health.status === "healthy").length;
    const degraded = state.channels.filter(c => c.health && c.health.status === "degraded").length;
    const down = state.channels.filter(c => c.health && c.health.status === "down").length;
    const proxy = Store.config.get("gw_upstream_proxy", "");
    const tokenSet = !!Store.config.get("gw_admin_token", "");
    const dkSet = !!Store.config.get("gw_device_keys", "");
    grid.innerHTML =
      '<article class="insight-card card"><div class="card-label"><span>◌</span> 渠道健康</div>' +
      '<div class="gw-stat-row"><strong>' + usable + '</strong><span>/ ' + total + ' 个渠道可用于对话</span></div>' +
      '<div class="gw-stat-list">' +
      '<div class="gw-stat-row"><strong>' + enabled + '</strong><span>已启用</span></div>' +
      '<div class="gw-stat-row">' + healthBadge("healthy", "健康 " + healthy) + healthBadge("degraded", "降级 " + degraded) + healthBadge("down", "故障 " + down) + '</div>' +
      '</div></article>' +
      '<article class="insight-card card"><div class="card-label"><span>↗</span> 网关能力</div>' +
      '<div class="gw-stat-row"><strong>' + state.models.length + '</strong><span>个文本模型可选</span></div>' +
      '<div class="gw-cap-list">' +
      '<div><i class="mini-dot green-dot"></i> WASM 网关' + healthBadge("healthy", "已就绪") + '</div>' +
      '<div><i class="mini-dot ' + (proxy ? "green-dot" : "gray-dot") + '"></i> 上游转发代理' + healthBadge(proxy ? "healthy" : "unknown", proxy ? "已配置" : "未配置") + '</div>' +
      '<div><i class="mini-dot ' + (dkSet ? "green-dot" : "gray-dot") + '"></i> 本设备 Key' + healthBadge(dkSet ? "healthy" : "unknown", dkSet ? "已绑定" : "未绑定") + '</div>' +
      '<div><i class="mini-dot ' + (tokenSet ? "green-dot" : "gray-dot") + '"></i> 管理面口令' + healthBadge(tokenSet ? "healthy" : "unknown", tokenSet ? "已设置" : "未设置") + '</div>' +
      '</div></article>' +
      '<article class="insight-card card"><div class="card-label"><span>◇</span> 快速开始</div>' +
      (state.models.length
        ? '<strong class="insight-title">可以直接提问</strong><p>当前有 ' + state.models.length + ' 个可用模型，在下方输入框开始对话。</p>'
        : '<strong class="insight-title">先登记一个渠道</strong><p>还没有可用渠道。请到「服务商与模型」登记带 Key 的渠道，或导入内置通道参数。</p>') +
      '</article>';
  }

  /* ================= 模型选择（积分倒序 + sel 记账 + ctx 边界） ================= */
  function modelLabel() { return state.model || "自动选择"; }
  function renderModelButtons() {
    $("#homeModelName").textContent = modelLabel();
    $("#chatModelName").textContent = modelLabel();
  }
  function openModelPicker() {
    // 积分倒序，并列按名称排——与旧 UI 同一口径（个人偏好排序）
    const items = state.models.slice().sort((a, b) =>
      (Points.score(b.name) - Points.score(a.name)) || String(a.name).localeCompare(String(b.name)));
    let html = '<div class="gw-model-list">';
    html += '<button class="gw-model-item' + (state.model ? "" : " active") + '" data-m=""><span class="gw-model-tick">' + (state.model ? "○" : "●") + '</span><span class="gw-model-name">自动选择</span><span class="gw-model-pts">积分 ' + Points.score(AUTO_KEY) + '</span></button>';
    if (!items.length) {
      html += '<div class="gw-model-empty">暂无可用模型——请先登记渠道并配置 Key。</div>';
    }
    items.forEach(m => {
      const active = state.model === m.name;
      html += '<button class="gw-model-item' + (active ? " active" : "") + '" data-m="' + escapeHTML(m.name) + '"><span class="gw-model-tick">' + (active ? "●" : "○") + '</span><span class="gw-model-name">' + escapeHTML(m.name) + '</span><span class="gw-model-ch">' + escapeHTML(m.channel) + '</span><span class="gw-model-pts">' + Points.score(m.name) + '</span></button>';
    });
    html += "</div>";
    openModal("选择模型", html, null);
    $$("#modalBody .gw-model-item").forEach(b => b.onclick = () => pickModel(b.dataset.m));
  }
  function pickModel(m) {
    const changed = state.model !== m;
    state.model = m;
    Store.config.set("gw_model", m);
    if (changed) {
      Points.add("sel", m || AUTO_KEY);
      markCtxBoundary(m);
    }
    renderModelButtons();
    closeModal();
    toast(m ? "已切换模型：" + m : "已恢复自动选择", "ok");
  }

  /// 换模型时在当前会话里打一条上下文边界。
  /// 为什么必须做：网关会把整段 messages 原样发给上游，小模型极易照抄上一条
  /// assistant 的自述（「我的模型名称是 ChatGLM」）。打点后请求只带边界之后的
  /// 消息——用户自己的提问保留，上一个模型说过的话不再喂进去。
  function markCtxBoundary(m) {
    if (!ctxResetOn()) return;
    const c = current();
    if (!c || !c.msgs.length) return;           // 空会话没有可重置的上下文
    const last = c.msgs[c.msgs.length - 1];
    if (last && last.role === "divider" && (last.model || "") === (m || "")) return;  // 不重复打点
    const idx = c.msgs.length;
    c.msgs.push({
      id: id(), role: "divider", model: m || "",
      text: m ? "已切换到 " + m + " · 上下文已重置" : "已切换到自动选路 · 上下文已重置",
      ts: Date.now(),
    });
    c.ctxStart = idx;
    c.updated = Date.now();
    persistMsg(c, idx, c.msgs[idx]);
    persistConvo(c);
    renderMessages();
  }

  /* ================= 发送 / 流式 / 停止 ================= */
  /// 统一入口（首页 / 对话 / 语音助手 / 快捷卡都走这里）。
  /// 魔法指令命中即消费（不发不落库）；限额硬拦；修改态先截断再发。
  function submitText(text, atts) {
    text = String(text || "").trim();
    atts = atts || [];
    if (!text && !atts.length) return;
    if (state.streaming) { toast("正在生成中，可先停止当前回答"); return; }
    if (!atts.length && Magic.handleMagicCommand(text)) return;   // 魔法指令：消费后直接返回
    const bad = Att.validateSendPayload(text, atts);
    if (bad) { toast(bad, "err"); return; }
    if (state.editCtx) {
      const c = current();
      if (c && c.id === state.editCtx.convoId) { truncateFrom(c, state.editCtx.idx); renderMessages(); }
      dropEditCtx();
    }
    sendWith(text, atts).catch(e => {
      dbg("[send] 失败:", e && (e.stack || e.message) || e);
      toast("发送未开始：" + ((e && e.message) || e), "err");
    });
  }

  /// 实际发送链路。atts 为内存对象（含 file/dataUrl 的浅拷贝）。
  async function sendWith(text, atts) {
    // 纵深防御：哪怕绕过入口拦截（旧缓存 / 其它入口），魔法指令也绝不生成气泡
    if (!atts || !atts.length) { if (Magic.parseMagic(text)) return; }

    const c = ensureConvo();
    const convoId = c.id;
    const userIdx = c.msgs.length;
    const userMsg = { id: id(), role: "user", content: text, ts: Date.now(), attachments: atts };
    c.msgs.push(userMsg);
    const asstIdx = c.msgs.length;
    const asst = { id: id(), role: "assistant", content: "", reasoning: "", streaming: true, ts: Date.now() };
    c.msgs.push(asst);
    if (!c.title || c.title === "新对话") c.title = (text || "[附件]").slice(0, 26);
    c.updated = Date.now();
    setView("chat");
    renderConversations();
    renderMessages();

    // 流式节点即该消息的唯一 DOM（renderMessages 对 streaming 消息只挂 live 节点，
    // 不建静态占位——杜绝同一消息两个气泡）。retry:true 让终态带「重新生成」。
    const stream = streamMessage({ role: "assistant", id: asst.id, retry: true });
    state.stream = stream;
    $("#messages").append(stream.el);
    $("#messages").scrollTop = $("#messages").scrollHeight;

    persistMsg(c, userIdx, userMsg);
    persistMsg(c, asstIdx, asst);
    persistConvo(c);

    state.streaming = true;
    state.aborter = new AbortController();
    const myGen = ++state.streamGen;
    setStreamingUI(true);

    try {
      // 历史消息：从**上下文边界**往后取（换过模型就只剩边界后的消息），
      // 排除 divider 与错误消息；附件消息（可能无文字）也要带上。
      const ctxStart = Math.min(c.ctxStart || 0, Math.max(0, c.msgs.length - 1));
      const history = c.msgs.slice(ctxStart, -1)
        .filter(m => (m.role === "user" || m.role === "assistant") && !m.error && (m.content || (m.attachments && m.attachments.length)))
        .map(m => ({ role: m.role, content: Att.buildApiContent(m) }));
      // skills 三流合并（系统级 → 默认流 → 手选）：system 消息注入（全渠道保底）
      // + x-gw-features（网关按渠道注入真参数）；>100KB 的单条在收集时跳过并提示
      const sk = Skills.collectSkillMsgs();
      if (sk.list.length) {
        history.unshift(...sk.list.map(s => ({ role: "system", content: s.prompt })));
        for (const s of sk.list) Skills.api.use(s.id).catch(() => {});
      }
      // F23 视觉/视频选路：本轮带图片/视频附件 → 用设置里锁定的视觉/视频模型覆盖；
      // 「always」范围仅在自动选路（未显式选模型）下允许无附件覆盖。
      const mmModel = Voice.mmModelFor(Att.attHasMedia(atts), state.model);
      const result = await gateway.chat({
        messages: history,
        model: mmModel || state.model || undefined,
        features: sk.feats,
        signal: state.aborter.signal,
        onDelta: ({ content }) => { stream.setText(content); $("#messages").scrollTop = $("#messages").scrollHeight; },
      });
      // 积分记账：使用 +1（命中模型；自动模式下自动条目也 +1）、降权 −20
      applyRoutePoints(result, state.model);
      asst.content = result.content;
      asst.reasoning = result.reasoning || "";
      asst.channel = result.channel || "";
      asst.attempts = result.attempts || "";
      asst.model = result.model || "";
      asst.ttfb = result.ttfbMs || null;
      asst.usage = result.usage || null;
      asst.streaming = false;
      stream.finish();
      const meta = [];
      if (result.channel) meta.push("via " + escapeHTML(result.channel));
      if (result.model) meta.push("模型 " + escapeHTML(result.model));
      if (result.attempts && String(result.attempts).split(";").length > 1) meta.push("⤻ 已自动切换");
      if (Workflow.cfg.usage && result.usage && result.usage.total_tokens != null) meta.push("⛁ " + fmtTok(result.usage.total_tokens));
      if (result.ttfbMs) meta.push("首字 " + result.ttfbMs + "ms");
      stream.setMeta(meta.join(" · "));
      if (!asst.content && !asst.reasoning) {
        stream.setError("（空响应）");
        asst.error = "（空响应）";
      }
      // token 账本（与旧 UI 同一口径）：累加进会话
      if (!asst.error && result.usage && result.usage.total_tokens != null) {
        c.tokens = (c.tokens || 0) + result.usage.total_tokens;
        c.answers = (Number(c.answers) || 0) + 1;
      }
      // F24 工作流（chat.completed）：附件显示规格 / LLM 总结标题 / 会话记忆压缩。
      // 失败绝不打断聊天主链路；标题变了要刷新侧栏与会话列表。
      const titleBefore = c.title;
      Workflow.runWorkflow({ convo: c, atts, usage: result.usage }).then(() => {
        if (c.title !== titleBefore) { c.updated = Date.now(); persistConvo(c); renderConversations(); }
      }).catch(() => {});
      // 语音助手浮层发起的对话：回答完成后按 TTS 链播报
      if (state.speakReply) {
        state.speakReply = false;
        const reply = asst.content || "";
        if (reply) {
          $("#jarvisTranscript").textContent = reply;
          $("#jarvisState").textContent = "播报中";
          Voice.ttsPlaySentence(reply).then(() => { $("#jarvisState").textContent = "我在听"; })
            .catch(e => {
              $("#jarvisState").textContent = "我在听";
              toast("播报失败：" + (e.message || e), "err");
            });
        }
      }
    } catch (e) {
      asst.streaming = false;
      if (e && e.name === "AbortError") {
        const msg = asst.content ? "已停止生成" : "已取消";
        asst.error = msg;
        stream.setText(asst.content);
        stream.setError(msg);
      } else {
        asst.error = "请求失败：" + (e && e.message ? e.message : String(e));
        stream.setError(asst.error);
        // F25：代理未配时浏览器直连上游必被 CORS 拦截——给一次明确引导
        // （已配代理仍失败则不唠叨，错误信息本身已足够）
        Upstream.hintIfProxyMissing();
      }
    } finally {
      state.streaming = false;
      state.aborter = null;
      setStreamingUI(false);
      c.updated = Date.now();
      persistMsg(c, asstIdx, asst);
      persistConvo(c);
      if (myGen === state.streamGen) renderConversations();   // 被抢占的旧流只落库不碰 UI
    }
  }

  function setStreamingUI(on) {
    $("#chatSend").classList.toggle("hidden", on);
    $("#chatStop").hidden = !on;
  }
  function stopStreaming() {
    if (state.aborter) state.aborter.abort();
  }

  /* ================= 附件（输入区） ================= */
  function renderAttachments() {
    const bar = $("#attachBar");
    const list = state.attachments;
    bar.innerHTML = list.map((a, i) => {
      const thumb = a.dataUrl || a.poster;   // 图片 dataUrl / 视频首帧 → 缩略图
      const tip = ' title="' + escapeHTML(a.name) + (a.desc ? " · 理解：" + escapeHTML(a.desc.slice(0, 80)) : "") + '"';
      const x = '<button class="att-x" data-attx="' + i + '" aria-label="移除">×</button>';
      return thumb
        ? '<div class="att-chip att-pic" data-st=""' + tip + '><img src="' + thumb + '" alt="">' + x + "</div>"
        : '<div class="att-chip att-file" data-st=""' + tip + '><span class="att-ic">' + (a.kind === "audio" ? "🎵" : a.kind === "video" ? "🎬" : a.kind === "image" ? "🖼" : "📎") + '</span><span class="att-nm">' + escapeHTML(a.name) + "</span>" + x + "</div>";
    }).join("");
    bar.hidden = list.length === 0;
    renderAttCount();
  }
  function renderAttCount() {
    const el = $("#attCount");
    const list = state.attachments;
    if (!list.length) { el.hidden = true; el.innerHTML = ""; return; }
    const parts = [];
    for (const k of ["image", "audio", "video", "file"]) {
      const n = Att.attCount(list, k), cap = Att.KIND_CAPS[k];
      parts.push('<span class="ac-i' + (n >= cap ? " full" : "") + '">' + Att.KIND_LABEL[k] + " " + n + "/" + cap + "</span>");
    }
    const mb = Att.mediaBytes(list);
    parts.push('<span class="ac-i' + (mb > Att.LIMITS.media ? " full" : "") + '">多媒体 ' + (mb / 1048576).toFixed(1) + "/" + (Att.LIMITS.media / 1048576).toFixed(0) + "MB</span>");
    el.innerHTML = parts.join("");
    el.hidden = false;
  }
  function removeAttachment(i) { state.attachments.splice(i, 1); renderAttachments(); }
  function pickFiles() {
    // 全部类别都满员时不再弹选择器（省一次白选）
    const full = Object.keys(Att.KIND_CAPS).every(k => Att.attCount(state.attachments, k) >= Att.KIND_CAPS[k]);
    if (full) { toast("附件已达上限（图片 5 / 音频 3 / 视频 1 / 文件 5），先移除一些再添加", "err"); return; }
    $("#fileInput").click();
  }
  async function onFilesPicked(files) {
    await Att.pickFiles(files, state.attachments, () => renderAttachments());
    renderAttachments();
  }

  /* ================= skills 三流条 ================= */
  function renderSkillsBar() {
    $("#skSysThink").dataset.on = Skills.sysSkillOn("think") ? "1" : "0";
    $("#skSysSearch").dataset.on = Skills.sysSkillOn("search") ? "1" : "0";
    $("#skSkip").dataset.on = Skills.skipDefault() ? "1" : "0";
    $("#skillDefaultList").innerHTML = Skills.defaultSkillIds().map(sid => {
      const s = Skills.state.list.find(x => x.id === sid);
      if (!s) return "";
      return '<button class="skill-chip def" data-def="' + escapeHTML(sid) + '" title="默认对话 skill（每次发送必带）· 点 × 移出">✦ ' + escapeHTML(s.title) + '<span class="def-x" data-defx="' + escapeHTML(sid) + '">×</span></button>';
    }).join("");
    const pick = $("#skPick");
    if (Skills.state.active) {
      pick.dataset.on = "1";
      pick.textContent = "✦ " + Skills.state.active.title;
      pick.title = "本轮手选：" + Skills.state.active.title + (Skills.state.active.summary ? " · " + Skills.state.active.summary : "") + "（再点取消）";
    } else {
      pick.dataset.on = "0";
      pick.textContent = "✦ 提示词";
      pick.title = "选择本轮提示词（SKILL）";
    }
  }
  function openSkillPicker() {
    const listable = Skills.state.list.filter(Skills.skillListable);
    if (!listable.length) {
      openModal("提示词", '<p class="muted-text">暂无可用提示词。提示词管理（新建 / 编辑 / 分类）在 B3「提示词」页提供。</p>', null);
      return;
    }
    const html = '<div class="gw-model-list">' + listable.map(s => {
      const active = Skills.state.active && Skills.state.active.id === s.id;
      return '<button class="gw-model-item' + (active ? " active" : "") + '" data-sk="' + escapeHTML(s.id) + '"><span class="gw-model-tick">' + (active ? "●" : "○") + '</span><span class="gw-model-name">' + escapeHTML(s.title) + '</span><span class="gw-model-ch">' + escapeHTML(Skills.skillCat(s)) + " · " + escapeHTML(s.summary || "") + "</span></button>";
    }).join("") + "</div>";
    openModal("选择本轮提示词", html, null);
    $$("#modalBody .gw-model-item").forEach(b => b.onclick = () => {
      const s = Skills.state.list.find(x => x.id === b.dataset.sk);
      Skills.state.active = (Skills.state.active && Skills.state.active.id === s.id) ? null : s;
      renderSkillsBar();
      closeModal();
      toast(Skills.state.active ? "本轮将带上：「" + s.title + "」" : "已取消本轮提示词", Skills.state.active ? "ok" : "");
    });
  }

  /* ================= 导出 ================= */
  function exportChat() {
    const c = current();
    if (!c || !c.msgs.length) { toast("当前没有可导出的对话"); return; }
    const body = "# " + (c.title || "新对话") + "\n\n" + c.msgs
      .filter(m => m.role !== "divider")
      .map(m => "## " + (m.role === "user" ? "你" : "QuakeAI") + "\n\n" + (m.content || m.error || "")).join("\n\n---\n\n");
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([body], { type: "text/markdown;charset=utf-8" }));
    a.download = (c.title || "chat").replace(/[\\/:*?"<>|]/g, "_") + ".md";
    a.click();
    URL.revokeObjectURL(a.href);
    toast("对话已导出为 Markdown", "ok");
  }

  /* ================= 分享（链接 + 二维码，积分 share +10） ================= */
  async function shareMsg(m) {
    const text = m.content || "";
    if (!text.trim()) { toast("没有可分享的内容", "err"); return; }
    Points.add("share", m.model || AUTO_KEY);   // 归给这条回答实际命中的模型
    let url = null;
    try { url = await Share.buildShareUrl(text, m.model || ""); } catch { /* 压缩不可用：降级走纯文本 */ }
    if (url && navigator.share) {
      try { await navigator.share({ title: "QuakeAI 的回答", text: "看看这个 AI 回答", url }); return; }
      catch (e) { if (e && e.name === "AbortError") return; /* 其余失败落级对话框 */ }
    }
    if (!url) {
      if (navigator.share) {
        try { await navigator.share({ title: "QuakeAI 的回答", text }); return; }
        catch (e) { if (e && e.name === "AbortError") return; }
      }
      try { await navigator.clipboard.writeText(text); toast("当前环境不支持链接分享，已复制全文", "ok"); } catch { toast("分享失败", "err"); }
      return;
    }
    openShareModal(url);
  }
  function openShareModal(url) {
    const html =
      '<p class="muted-text">内容压缩后放进链接本身（服务端零存储）。微信里：点右上角 ⋮ → 发送给朋友；或复制链接粘贴到聊天。</p>' +
      '<div class="share-url-row"><input id="shareUrl" readonly value="' + escapeHTML(url) + '" title="点击全选"></div>' +
      '<div class="share-actions">' +
      '<button class="soft-btn" id="shareCopy">复制链接</button>' +
      (navigator.share ? '<button class="soft-btn" id="shareSys">系统分享</button>' : "") +
      '<button class="soft-btn" id="shareQr">二维码</button>' +
      '<button class="soft-btn" id="shareOpen">打开分享页</button>' +
      "</div>" +
      '<div class="share-qr" id="shareQrBox" hidden><img id="shareQrImg" alt="分享二维码"></div>';
    openModal("分享这条回答", html, null, "关闭");
    $("#shareCopy").onclick = () => {
      const u = $("#shareUrl").value;
      (navigator.clipboard ? navigator.clipboard.writeText(u) : Promise.reject())
        .then(() => toast("链接已复制", "ok")).catch(() => { $("#shareUrl").select(); toast("请手动复制（Ctrl/Cmd+C）", ""); });
    };
    const sys = $("#shareSys");
    if (sys) sys.onclick = () => navigator.share({ title: "QuakeAI 的回答", text: "看看这个 AI 回答", url }).catch(() => {});
    $("#shareOpen").onclick = () => window.open(url, "_blank", "noopener");
    $("#shareQr").onclick = () => {
      const box = $("#shareQrBox");
      if (!box.hidden) { box.hidden = true; return; }
      try {
        const qr = QRLIB(0, "M");
        qr.addData(url);
        qr.make();
        $("#shareQrImg").src = qr.createDataURL(6, 2);
        box.hidden = false;
      } catch (e) { toast("二维码生成失败：" + e.message, "err"); }
    };
  }

  /* ================= 弹层 ================= */
  // 焦点管理（a11y）：打开时焦点移入浮层、关闭时还原到触发元素——
  // 原生 <dialog>（showModal）自带焦点圈闭，这里只管两个自定义容器。
  const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
  function overlayFocusIn(el) {
    const first = el.querySelector(FOCUSABLE);
    if (first) first.focus();
    else { el.tabIndex = -1; el.focus(); }
  }
  function openModal(title, html, onSave, saveLabel) {
    $("#modalTitle").textContent = title;
    $("#modalBody").innerHTML = html;
    state.modalSave = onSave;
    state.modalReturn = document.activeElement;
    $("#saveModal").textContent = saveLabel || "保存";
    $("#saveModal").hidden = !onSave;
    $("#cancelModal").textContent = onSave ? "取消" : "关闭";
    $("#modalBackdrop").classList.remove("hidden");
    overlayFocusIn($("#modalBackdrop"));
  }
  function closeModal() {
    $("#modalBackdrop").classList.add("hidden");
    state.modalSave = null;
    const ret = state.modalReturn;
    if (ret && ret.isConnected) { try { ret.focus(); } catch (e) {} }
    state.modalReturn = null;
  }

  /* ================= 侧栏 / 语音助手（Jarvis 浮层：F22 STT 链 + F21 播报） ================= */
  function closeSidebar() { $("#sidebar").classList.remove("open"); $("#sidebarScrim").classList.remove("show"); }
  function openSidebar() { $("#sidebar").classList.add("open"); $("#sidebarScrim").classList.add("show"); }
  function openJarvis() {
    // 链路提示随当前 STT 模式变化（浏览器识别零上传 / 网关录音转写）
    const cfg = Voice.cfg;
    $("#jarvisHint").textContent = cfg.sttMode === "gateway"
      ? "网关转写模式：点击麦克风录音，松手后上传转写。也可直接输入文字。"
      : "点击圆形按钮开始语音输入（浏览器实时识别），或直接输入文字。";
    state.jarvisReturn = document.activeElement;
    $("#jarvisOverlay").classList.remove("hidden");
    $("#jarvisText").focus();
  }
  function closeJarvis() {
    stopRecognition(); Voice.ttsStopAll();
    $("#jarvisOverlay").classList.add("hidden");
    const ret = state.jarvisReturn;
    if (ret && ret.isConnected) { try { ret.focus(); } catch (e) {} }
    state.jarvisReturn = null;
  }
  function stopRecognition() {
    Voice.VoiceInput.abort("jarvis");
    state.micBusy = false;
    $("#jarvisMic").classList.remove("listening");
    $("#jarvisState").textContent = "我在听";
  }
  /// F22 STT 递降链：浏览器 SpeechRecognition（实时 partial）→ 网关转写兜底。
  /// VoiceInput 内部保证两个麦克风消费者绝不并发（iOS/Android 互斥）。
  async function startRecognition() {
    if (state.micBusy) { stopRecognition(); return; }
    state.micBusy = true;
    $("#jarvisMic").classList.add("listening");
    $("#jarvisState").textContent = "正在聆听…";
    $("#jarvisHint").textContent = "说完后会自动填入识别结果。";
    try {
      const mode = await Voice.VoiceInput.start("jarvis", p => {
        $("#jarvisTranscript").textContent = p;
        $("#jarvisText").value = p;
      });
      $("#jarvisHint").textContent = mode === "browser"
        ? "浏览器实时识别中（零上传）…说完自动填入。"
        : "录音中…点击麦克风结束并转写。";
      if (mode === "recorder") {
        // 录音模式：再点一次麦克风 = 停止并转写
        $("#jarvisMic").onclick = stopAndTranscribe;
      }
    } catch (e) {
      state.micBusy = false;
      $("#jarvisMic").classList.remove("listening");
      $("#jarvisState").textContent = "我在听";
      $("#jarvisHint").textContent = "语音识别不可用（" + (e.message || e) + "），请使用文字输入。";
      toast("语音识别暂不可用，可直接输入文字");
    }
  }
  async function stopAndTranscribe() {
    $("#jarvisState").textContent = "转写中…";
    try {
      const r = await Voice.VoiceInput.stop("jarvis");
      state.micBusy = false;
      $("#jarvisMic").classList.remove("listening");
      $("#jarvisMic").onclick = startRecognition;
      $("#jarvisState").textContent = "我在听";
      if (r.text) { $("#jarvisTranscript").textContent = r.text; $("#jarvisText").value = r.text; }
      else { $("#jarvisHint").textContent = "没有识别到内容，再试一次或直接输入文字。"; }
    } catch (e) {
      state.micBusy = false;
      $("#jarvisMic").classList.remove("listening");
      $("#jarvisMic").onclick = startRecognition;
      $("#jarvisState").textContent = "我在听";
      toast("转写失败：" + (e.message || e), "err");
    }
  }
  function sendJarvis() {
    const t = $("#jarvisText").value.trim();
    if (!t) return;
    if (state.streaming) { toast("正在生成中，可先停止当前回答"); return; }
    stopRecognition();
    $("#jarvisTranscript").textContent = "已提交：" + t;
    $("#jarvisText").value = "";
    $("#jarvisState").textContent = "思考中…";
    state.speakReply = true;   // 回答完成后按 TTS 链播报（F21）
    submitText(t, []);
  }

  /* ================= 修改态 ================= */
  function renderEditChip() {
    const chip = $("#editChip");
    if (state.editCtx) {
      $("#editChipTitle").textContent = "修改中 · 第 " + (state.editCtx.idx + 1) + " 条消息（发送后替换原消息）";
      chip.hidden = false;
    } else {
      chip.hidden = true;
    }
  }
  function dropEditCtx() { state.editCtx = null; renderEditChip(); }
  function cancelEdit(silent) {
    if (!state.editCtx) return;
    dropEditCtx();
    const input = $("#chatPrompt");
    if (input) { input.value = ""; input.style.height = "auto"; }
    state.attachments = []; renderAttachments();
    if (!silent) toast("已取消修改", "");
  }

  /* ================= 主题 ================= */
  // 同步三处：html data-theme（CSS 变量切换）/ theme-color meta（浏览器 UI 色，
  // 安装态 PWA  splash 之外唯一可控处）/ 无持久化副作用（写盘在 themeToggle）。
  const THEME_COLORS = { light: "#f7f8fa", dark: "#15171b" };
  function applyTheme(v) {
    const dark = v === "dark";
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    const meta = document.getElementById("metaTheme");
    if (meta) meta.content = THEME_COLORS[dark ? "dark" : "light"];
  }

  /* ================= 跨模块查询（工作流页「当前会话记忆」用） ================= */
  // refreshGateway 一并导出：测试/调试可在渠道账本被外部改动后手动触发模型清单刷新
  // （正常流程由 Providers 的 onChange 回调驱动，见 init 装配）。
  window.QuakeApp = { currentConvo: () => current(), refreshGateway };

  /* ================= 事件装配 ================= */
  function init() {
    applyTheme(Store.theme.get());
    $("#dateChip").textContent = new Intl.DateTimeFormat("zh-CN", { month: "long", day: "numeric", weekday: "short" }).format(new Date());
    $("#buildVersion").textContent = "QuakeAI " + (window.__BUILD_VERSION__ || "dev");
    ["#pvBuildVersion", "#prBuildVersion", "#vcBuildVersion", "#wfBuildVersion", "#upBuildVersion"].forEach(sel => {
      const el = $(sel);
      if (el) el.textContent = "QuakeAI " + (window.__BUILD_VERSION__ || "dev");
    });

    // 服务商页：渠道账本变更后联动刷新（模型清单 / 顶栏 / 侧栏状态）
    // + 上游页「本设备 Key」行按 provider 重建
    Providers.init(() => { refreshGateway(); if (global.QuakeUpstream) QuakeUpstream.load(); });
    // 提示词页：账本变更后联动刷新对话页三流条（默认流 chips / 手选态）
    Prompts.init(() => { renderSkillsBar(); });
    // 语音页：链路状态 + 能力模型下拉（内部自装载 channels）
    Voice.init();
    // 工作流页：参数持久化 + 格式实时生效 + chat 完成钩子由 app.js 调用
    Workflow.init();
    // 上游与接入页：代理 / 鉴权 / 本设备 Key / 部署自检
    Upstream.init();
    // 网络监视（F29）：全局状态条 + 心跳 + online/offline
    NetMon.start();

    Store.loadConversations().then(convos => {
      state.conversations = convos;
      state.currentId = convos.length ? convos[0].id : null;
      renderConversations();
      renderMessages();
    }).catch(e => {
      toast("会话数据读取失败：" + e.message, "err");
      renderConversations();
    });

    refreshGateway();
    // skills 数据层装载（种子化 + 系统级补种）后渲染三流条
    Skills.load().then(renderSkillsBar).catch(() => renderSkillsBar());
    // 魔法指令 / 调试浮层 / 控制台面板：按持久化开关恢复顶栏按钮
    Magic.setStateProvider(() => ({
      channels: state.channels,
      models: state.models.map(m => m.name),
      model: state.model || "",
      skills: {
        active: Skills.state.active ? Skills.state.active.title : null,
        defaults: Skills.defaultSkillIds(),
        sys: { think: Skills.sysSkillOn("think"), search: Skills.sysSkillOn("search") },
        skipDefault: Skills.skipDefault(),
      },
    }));
    Magic.init();
    renderSkillsBar();
    renderAttachments();

    $$("[data-view]").forEach(b => b.addEventListener("click", () => setView(b.dataset.view)));
    $("#newChat").onclick = () => {
      if (state.editCtx) cancelEdit(true);       // 新会话不继承「修改中」的输入框内容
      if (state.streaming) {                     // 旧流干净收尾，否则发送键还是旧流的停止键
        state.streamGen++;
        try { state.aborter && state.aborter.abort(); } catch (e) {}
        state.streaming = false; state.aborter = null; setStreamingUI(false);
      }
      state.currentId = null;
      renderConversations(); renderMessages();
      setView("chat");
      $("#chatPrompt").focus();
    };
    $("#homeSend").onclick = () => { const v = $("#homePrompt").value; $("#homePrompt").value = ""; submitText(v, []); };
    $("#homePrompt").addEventListener("keydown", e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("#homeSend").click(); } });
    $("#chatSend").onclick = () => {
      const v = $("#chatPrompt").value;
      $("#chatPrompt").value = ""; $("#chatPrompt").style.height = "auto";
      submitText(v, state.attachments.slice());
      state.attachments = []; renderAttachments();
    };
    $("#chatStop").onclick = stopStreaming;
    $("#chatAttach").onclick = pickFiles;
    $("#fileInput").addEventListener("change", e => { onFilesPicked(e.target.files); e.target.value = ""; });
    $("#attachBar").addEventListener("click", e => {
      const x = e.target.closest("[data-attx]");
      if (x) removeAttachment(Number(x.dataset.attx));
    });
    $("#chatPrompt").addEventListener("keydown", e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("#chatSend").click(); } });
    $("#chatPrompt").addEventListener("input", e => { e.target.style.height = "auto"; e.target.style.height = Math.min(e.target.scrollHeight, 180) + "px"; });
    $$(".quick-card").forEach(b => b.onclick = () => submitText(b.dataset.prompt, []));
    $("#homeModelBtn").onclick = openModelPicker;
    $("#chatModelBtn").onclick = openModelPicker;
    $("#refreshHealth").onclick = refreshGateway;
    $("#searchChats").oninput = e => renderConversations(e.target.value);
    $("#clearSearch").onclick = () => { $("#searchChats").value = ""; renderConversations(); };
    $("#themeToggle").onclick = () => {
      const v = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      applyTheme(v); Store.theme.set(v);
    };
    $("#exportChat").onclick = exportChat;
    $("#chatMore").onclick = () => openModal("对话选项",
      '<div class="gw-model-list">' +
      '<button class="gw-model-item" data-act="export"><span class="gw-model-tick">↧</span><span class="gw-model-name">导出对话</span><span class="gw-model-ch">保存为 Markdown 文件</span></button>' +
      '<button class="gw-model-item" data-act="ctxreset"><span class="gw-model-tick">' + (ctxResetOn() ? "●" : "○") + '</span><span class="gw-model-name">换模型重置上下文</span><span class="gw-model-ch">' + (ctxResetOn() ? "已开启：切换模型后只带新消息" : "已关闭：切换模型保留全部历史") + "</span></button>" +
      '<button class="gw-model-item" data-act="delete"><span class="gw-model-tick">✕</span><span class="gw-model-name">删除当前对话</span><span class="gw-model-ch">不可恢复，可先导出留存</span></button>' +
      "</div>", null, "关闭");
    $("#modalBody").addEventListener("click", e => {
      const b = e.target.closest("button[data-act]");
      if (!b) return;
      const act = b.dataset.act;
      if (act === "export") { closeModal(); exportChat(); }
      else if (act === "ctxreset") {
        const { setCtxReset } = window.QuakePoints;
        setCtxReset(!ctxResetOn());
        closeModal();
      } else if (act === "delete") {
        const c = current();
        if (!c) return;
        Store.IDB.deleteConvo(c.id);
        state.conversations = state.conversations.filter(x => x.id !== c.id);
        state.currentId = null;
        renderConversations(); renderMessages();
        closeModal();
        toast("当前对话已删除", "ok");
      }
    });
    /* ---------- skills 三流条 ---------- */
    $("#skSysThink").onclick = () => { Skills.toggleSysSkill("think"); renderSkillsBar(); };
    $("#skSysSearch").onclick = () => { Skills.toggleSysSkill("search"); renderSkillsBar(); };
    $("#skSkip").onclick = () => { Skills.toggleSkipDefault(); renderSkillsBar(); };
    $("#skPick").onclick = openSkillPicker;
    $("#skillsBar").addEventListener("click", e => {
      const defx = e.target.closest("[data-defx]");
      if (defx) { e.stopPropagation(); Skills.toggleDefaultSkill(defx.dataset.defx); renderSkillsBar(); return; }
      const def = e.target.closest("[data-def]");
      if (def) {
        const s = Skills.state.list.find(x => x.id === def.dataset.def);
        if (s) { Skills.state.active = s; renderSkillsBar(); toast("本轮将带上：「" + s.title + "」", "ok"); }
      }
    });
    /* ---------- 修改态 ---------- */
    $("#editChipCancel").onclick = () => cancelEdit(false);
    $("#openJarvis").onclick = openJarvis; $("#closeJarvis").onclick = closeJarvis;
    $("#jarvisBackdrop").onclick = closeJarvis; $("#jarvisMic").onclick = startRecognition;
    $("#jarvisSend").onclick = sendJarvis;
    $("#jarvisText").addEventListener("keydown", e => { if (e.key === "Enter") { e.preventDefault(); sendJarvis(); } });
    $("#openSidebar").onclick = openSidebar; $("#closeSidebar").onclick = closeSidebar; $("#sidebarScrim").onclick = closeSidebar;
    $("#closeModal").onclick = closeModal; $("#cancelModal").onclick = closeModal;
    $("#saveModal").onclick = () => { if (state.modalSave) state.modalSave(); closeModal(); };
    $("#modalBackdrop").addEventListener("click", e => { if (e.target === $("#modalBackdrop")) closeModal(); });
    $("#messages").addEventListener("click", e => {
      const btn = e.target.closest("button[data-action]");
      if (!btn) return;
      const msg = btn.closest(".message");
      const c = current();
      const m = c && c.msgs.find(x => x.id === msg.dataset.messageId);
      if (!m) return;
      const act = btn.dataset.action;
      if (act === "copy") {
        (navigator.clipboard ? navigator.clipboard.writeText(m.content || "") : Promise.reject())
          .then(() => toast("已复制", "ok")).catch(() => toast("复制失败，请手动选择文本", "err"));
      } else if (act === "share") {
        if (state.streaming) { toast("正在生成中，请先停止"); return; }
        shareMsg(m);
      } else if (act === "speak") {
        // F21 TTS 递降链：网关 → 自定义端点 → 浏览器语音
        if (!m.content || !m.content.trim()) { toast("这条消息没有可播报的内容"); return; }
        Voice.ttsStopAll();
        Voice.ttsPlaySentence(m.content).catch(e => {
          if (String(e && e.message) !== "stopped") toast("播报失败：" + (e.message || e), "err");
        });
      } else if (act === "retry") {
        if (state.streaming) { toast("正在生成中，请先停止"); return; }
        const idx = c.msgs.indexOf(m);
        if (m.role === "user") {
          // 重试（用户消息）：不新增消息——以这条为最后一条重新请求，截断其后的回答
          if (idx < 0) return;
          const atts = (m.attachments || []).map(a => ({ ...a }));
          truncateFrom(c, idx);
          renderMessages();
          submitText(m.content || "", atts);
        } else if (m.role === "assistant") {
          // 重新生成（助手消息）：回退到对应的提问，截断后重发
          let j = idx - 1;
          while (j >= 0 && c.msgs[j].role !== "user") j--;
          if (j < 0) { toast("找不到对应的提问", "err"); return; }
          const um = c.msgs[j];
          const atts = (um.attachments || []).map(a => ({ ...a }));
          truncateFrom(c, j);
          renderMessages();
          submitText(um.content || "", atts);
        }
      } else if (act === "edit") {
        // 修改（用户消息）：内容打回输入框，不删任何东西；发送那一刻才截断
        if (state.streaming) { toast("正在生成，稍候再改", ""); return; }
        const idx = c.msgs.indexOf(m);
        if (idx < 0 || m.role !== "user") return;
        if (state.editCtx && state.editCtx.idx !== idx) cancelEdit(true);
        state.editCtx = { convoId: c.id, idx };
        const input = $("#chatPrompt");
        input.value = m.content || "";
        state.attachments = (m.attachments || []).map(a => ({ ...a }));
        renderAttachments();
        input.style.height = "auto"; input.style.height = Math.min(input.scrollHeight, 180) + "px";
        renderEditChip();
        setView("chat");
        input.focus();
        try { input.setSelectionRange(input.value.length, input.value.length); } catch (e) {}
        input.scrollIntoView({ block: "nearest", behavior: "smooth" });
      }
    });
    document.addEventListener("keydown", e => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); $("#newChat").click(); }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "j") { e.preventDefault(); $("#themeToggle").click(); }
      if (e.key === "Escape") { closeModal(); closeJarvis(); closeSidebar(); }
    });
  }
  init();
})();
