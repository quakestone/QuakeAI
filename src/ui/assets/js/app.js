/* ============================================================
   QuakeAI app.js —— 壳状态 / 视图 / 真实业务接线
   边界：只调 QuakeAdapters.gateway（适配层）与 QuakeStore（持久化），
   不直接碰 WASM / gw.* / fetch 细节。
   B0 范围：首页（真实网关状态）+ 对话（真实一问一答 / 停止 / 重生成 /
   导出 / 模型选择）+ 语音助手浮层（文字走真实对话、麦克风走 Web Speech）。
   ============================================================ */
(function () {
  "use strict";

  const $ = s => document.querySelector(s), $$ = s => Array.from(document.querySelectorAll(s));
  const { gateway, usableChatModels } = window.QuakeAdapters;
  const { message, streamMessage, toast, escapeHTML, healthBadge } = window.QuakeComponents;
  const Store = window.QuakeStore;

  const VIEW_NAMES = { home: "智能首页", chat: "AI 对话" };
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
    recognition: null,
    modalSave: null,
  };

  /* ================= 持久化 ================= */
  function persistConvo(c) { Store.IDB.putConvo(c); }
  function persistMsg(c, idx, m) { Store.IDB.putMsg(c.id, idx, m); }
  const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
  const persistMsgThrottled = debounce((c, idx, m) => persistMsg(c, idx, m), 700);

  function id() { return "c_" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }
  function current() { return state.conversations.find(c => c.id === state.currentId); }

  function createConversation(firstText) {
    const c = { id: id(), title: firstText ? firstText.slice(0, 26) : "新对话", model: state.model, updated: Date.now(), msgs: [] };
    state.conversations.unshift(c);
    state.currentId = c.id;
    persistConvo(c);
    renderConversations();
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
      const meta = [];
      if (m.channel) meta.push(escapeHTML(m.channel));
      if (m.model) meta.push(escapeHTML(m.model));
      if (m.error) meta.push('<span class="hb bad">' + escapeHTML(m.error) + "</span>");
      el.append(message({ role: m.role, text: m.content || "", id: m.id, meta: meta.length ? meta.join(" · ") : "" }));
    });
    el.scrollTop = el.scrollHeight;
  }

  /* ================= 视图 ================= */
  function setView(view) {
    state.view = view;
    $("#homeView").classList.toggle("hidden", view !== "home");
    $("#chatView").classList.toggle("hidden", view !== "chat");
    $("#breadcrumb").textContent = VIEW_NAMES[view] || "工作台";
    $$(".nav-item[data-view]").forEach(b => b.classList.toggle("active", b.dataset.view === view));
    if (view === "chat") renderMessages();
    closeSidebar();
    window.scrollTo({ top: 0, behavior: "smooth" });
  }
  function openConversation(cid) {
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

  /* ================= 模型选择 ================= */
  function modelLabel() { return state.model || "自动选择"; }
  function renderModelButtons() {
    $("#homeModelName").textContent = modelLabel();
    $("#chatModelName").textContent = modelLabel();
  }
  function openModelPicker() {
    const items = state.models;
    let html = '<div class="gw-model-list">';
    html += '<button class="gw-model-item' + (state.model ? "" : " active") + '" data-m=""><span class="gw-model-tick">' + (state.model ? "○" : "●") + '</span><span class="gw-model-name">自动选择</span><span class="gw-model-ch">由网关按优先级路由</span></button>';
    if (!items.length) {
      html += '<div class="gw-model-empty">暂无可用模型——请先登记渠道并配置 Key。</div>';
    }
    items.forEach(m => {
      const active = state.model === m.name;
      html += '<button class="gw-model-item' + (active ? " active" : "") + '" data-m="' + escapeHTML(m.name) + '"><span class="gw-model-tick">' + (active ? "●" : "○") + '</span><span class="gw-model-name">' + escapeHTML(m.name) + '</span><span class="gw-model-ch">' + escapeHTML(m.channel) + "</span></button>";
    });
    html += "</div>";
    openModal("选择模型", html, null);
    $$("#modalBody .gw-model-item").forEach(b => b.onclick = () => {
      state.model = b.dataset.m;
      Store.config.set("gw_model", state.model);
      renderModelButtons();
      closeModal();
      toast(state.model ? "已切换模型：" + state.model : "已恢复自动选择", "ok");
    });
  }

  /* ================= 发送 / 流式 / 停止 / 重生成 ================= */
  async function send(text) {
    text = String(text || "").trim();
    if (!text || state.streaming) return;
    let c = current();
    if (!c) c = createConversation(text);
    if (!c.msgs.length) c.title = text.slice(0, 26) + (text.length > 26 ? "…" : "");
    const userMsg = { id: id(), role: "user", content: text, ts: Date.now() };
    c.msgs.push(userMsg);
    c.updated = Date.now();
    persistMsg(c, c.msgs.length - 1, userMsg);
    persistConvo(c);
    setView("chat");
    renderConversations();
    renderMessages();

    const asst = { id: id(), role: "assistant", content: "", reasoning: "", streaming: true, ts: Date.now() };
    c.msgs.push(asst);
    const asstIdx = c.msgs.length - 1;
    persistMsg(c, asstIdx, asst);

    const stream = streamMessage({ role: "assistant", id: asst.id });
    $("#messages").append(stream.el);
    $("#messages").scrollTop = $("#messages").scrollHeight;

    state.streaming = true;
    state.aborter = new AbortController();
    setStreamingUI(true);

    try {
      const result = await gateway.chat({
        messages: c.msgs.filter(m => m.content && !m.error).map(m => ({ role: m.role, content: m.content })),
        model: state.model || undefined,
        signal: state.aborter.signal,
        onDelta: ({ content }) => { stream.setText(content); $("#messages").scrollTop = $("#messages").scrollHeight; },
      });
      asst.content = result.content;
      asst.reasoning = result.reasoning || "";
      asst.channel = result.channel || "";
      asst.model = result.model || "";
      asst.streaming = false;
      stream.finish();
      const meta = [];
      if (result.channel) meta.push(escapeHTML(result.channel));
      if (result.model) meta.push(escapeHTML(result.model));
      if (result.ttfbMs) meta.push("首字 " + result.ttfbMs + "ms");
      stream.setMeta(meta.join(" · "));
      if (!asst.content && !asst.reasoning) {
        stream.setError("（空响应）");
        asst.error = "（空响应）";
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
      }
    } finally {
      state.streaming = false;
      state.aborter = null;
      setStreamingUI(false);
      c.updated = Date.now();
      persistMsg(c, asstIdx, asst);
      persistConvo(c);
      renderConversations();
    }
  }

  function setStreamingUI(on) {
    $("#chatSend").classList.toggle("hidden", on);
    $("#chatStop").hidden = !on;
  }
  function stopStreaming() {
    if (state.aborter) state.aborter.abort();
  }

  /* ================= 导出 ================= */
  function exportChat() {
    const c = current();
    if (!c || !c.msgs.length) { toast("当前没有可导出的对话"); return; }
    const body = "# " + (c.title || "新对话") + "\n\n" + c.msgs.map(m => "## " + (m.role === "user" ? "你" : "QuakeAI") + "\n\n" + (m.content || m.error || "")).join("\n\n---\n\n");
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([body], { type: "text/markdown;charset=utf-8" }));
    a.download = (c.title || "chat").replace(/[\\/:*?"<>|]/g, "_") + ".md";
    a.click();
    URL.revokeObjectURL(a.href);
    toast("对话已导出为 Markdown", "ok");
  }

  /* ================= 弹层 ================= */
  function openModal(title, html, onSave, saveLabel) {
    $("#modalTitle").textContent = title;
    $("#modalBody").innerHTML = html;
    state.modalSave = onSave;
    $("#saveModal").textContent = saveLabel || "保存";
    $("#modalBackdrop").classList.remove("hidden");
  }
  function closeModal() { $("#modalBackdrop").classList.add("hidden"); state.modalSave = null; }

  /* ================= 侧栏 / 语音助手 ================= */
  function closeSidebar() { $("#sidebar").classList.remove("open"); $("#sidebarScrim").classList.remove("show"); }
  function openSidebar() { $("#sidebar").classList.add("open"); $("#sidebarScrim").classList.add("show"); }
  function openJarvis() { $("#jarvisOverlay").classList.remove("hidden"); $("#jarvisText").focus(); }
  function closeJarvis() { stopRecognition(); $("#jarvisOverlay").classList.add("hidden"); }
  function stopRecognition() {
    if (state.recognition) { try { state.recognition.stop(); } catch (e) {} state.recognition = null; }
    $("#jarvisMic").classList.remove("listening");
  }
  function startRecognition() {
    const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SR) {
      $("#jarvisHint").textContent = "当前浏览器不支持 Web Speech API，请使用文字输入。";
      toast("当前浏览器不支持语音识别，可直接输入文字。");
      return;
    }
    if (state.recognition) { stopRecognition(); $("#jarvisState").textContent = "我在听"; return; }
    const rec = new SR();
    state.recognition = rec;
    rec.lang = "zh-CN"; rec.interimResults = true; rec.continuous = false;
    $("#jarvisMic").classList.add("listening");
    $("#jarvisState").textContent = "正在聆听…";
    $("#jarvisHint").textContent = "说完后会自动填入识别结果。";
    rec.onresult = e => {
      let transcript = "";
      for (let i = e.resultIndex; i < e.results.length; i++) transcript += e.results[i][0].transcript;
      $("#jarvisTranscript").textContent = transcript;
      $("#jarvisText").value = transcript;
    };
    rec.onerror = e => toast("语音识别暂不可用：" + e.error);
    rec.onend = () => { state.recognition = null; $("#jarvisMic").classList.remove("listening"); $("#jarvisState").textContent = "我在听"; };
    try { rec.start(); } catch (e) { toast("无法启动麦克风，请检查浏览器权限。"); }
  }
  function sendJarvis() {
    const t = $("#jarvisText").value.trim();
    if (!t) return;
    $("#jarvisTranscript").textContent = "已提交：" + t;
    $("#jarvisText").value = "";
    closeJarvis();
    send(t);
  }

  /* ================= 主题 ================= */
  function applyTheme(v) { document.documentElement.dataset.theme = v === "dark" ? "dark" : "light"; }

  /* ================= 事件装配 ================= */
  function init() {
    applyTheme(Store.theme.get());
    $("#dateChip").textContent = new Intl.DateTimeFormat("zh-CN", { month: "long", day: "numeric", weekday: "short" }).format(new Date());
    $("#buildVersion").textContent = "QuakeAI " + (window.__BUILD_VERSION__ || "dev");

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

    $$("[data-view]").forEach(b => b.addEventListener("click", () => setView(b.dataset.view)));
    $("#newChat").onclick = () => { state.currentId = null; setView("chat"); renderMessages(); $("#chatPrompt").focus(); };
    $("#homeSend").onclick = () => { const v = $("#homePrompt").value; $("#homePrompt").value = ""; send(v); };
    $("#homePrompt").addEventListener("keydown", e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("#homeSend").click(); } });
    $("#chatSend").onclick = () => { const v = $("#chatPrompt").value; $("#chatPrompt").value = ""; $("#chatPrompt").style.height = "auto"; send(v); };
    $("#chatStop").onclick = stopStreaming;
    $("#chatPrompt").addEventListener("keydown", e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("#chatSend").click(); } });
    $("#chatPrompt").addEventListener("input", e => { e.target.style.height = "auto"; e.target.style.height = Math.min(e.target.scrollHeight, 180) + "px"; });
    $$(".quick-card").forEach(b => b.onclick = () => send(b.dataset.prompt));
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
    $("#chatMore").onclick = () => openModal("删除当前对话？",
      '<p class="muted-text">删除后不可恢复，可先「导出」留存记录。</p>',
      () => {
        const c = current();
        if (!c) return;
        Store.IDB.deleteConvo(c.id);
        state.conversations = state.conversations.filter(x => x.id !== c.id);
        state.currentId = null;
        renderConversations(); renderMessages();
        toast("当前对话已删除", "ok");
      }, "删除");
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
      if (btn.dataset.action === "copy") {
        (navigator.clipboard ? navigator.clipboard.writeText(m.content || "") : Promise.reject())
          .then(() => toast("已复制", "ok")).catch(() => toast("复制失败，请手动选择文本", "err"));
      } else if (btn.dataset.action === "retry") {
        if (state.streaming) { toast("正在生成中，请先停止"); return; }
        const idx = c.msgs.indexOf(m);
        if (m.role !== "assistant" || idx < 1 || c.msgs[idx - 1].role !== "user") {
          toast("重生成只对问答对生效"); return;
        }
        const prev = c.msgs[idx - 1];
        // 截断到该回答之前（IDB 位置键：删掉旧尾，同位置重写即覆盖，不残留）
        const removed = c.msgs.splice(idx);
        removed.forEach((mm, i) => Store.IDB.deleteMsg(c.id, idx + i));
        persistConvo(c);
        renderMessages();
        send(prev.content);
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
