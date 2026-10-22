/* ============================================================================
   QuakePrompts —— 提示词（SKILL）管理页（B3 / F20）
   ----------------------------------------------------------------------------
   本模块是「提示词」视图的唯一业务层：列表（二级分类 + 每类独立分页 5 条）、
   新建/编辑/删除、启停、设为默认对话（≤3）、使用计数。
   数据层复用 QuakeSkills（skills.js：load/api/默认流/系统级口径不变）；
   持久化键不变（gw_default_skills / gw_skip_default / skills DO 账本）。
   与旧 UI（admin_ui.html SKILL 管理标签）行为对齐，表现层换 Clarity Kit。
   依赖：adapters（WASM 边界）/ components（toast/escapeHTML）/ skills（数据层）。
   app.js 只做视图装配与 onChange 回调（提示词变更 → 刷新对话页三流条）。
   脚本加载序：… → skills → providers → prompts → voice → workflow → app。
   ==========================================================================*/
(function (global) {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const { toast, escapeHTML } = global.QuakeComponents;
  const Skills = global.QuakeSkills;

  const PAGE_SIZE = 5;             // 每个二级分类每页 5 条（与旧 UI 同口径）
  const SUMMARY_MAX = 30;          // 简介手填上限（超出截断）

  const state = {
    cat: Skills.SKILL_CATS[0],    // 当前二级分类
    pages: {},                     // 各分类独立页码 { 分类: n }
    loading: false,
    editId: null,                  // 对话框：null = 新建，否则编辑该 id
  };
  let onChange = () => {};         // app.js 注入：提示词变更后的联动刷新

  /* ================= 数据 ================= */
  async function load() {
    state.loading = true;
    try { await Skills.load(); }
    catch (e) { toast("提示词加载失败：" + (e.message || e), "err"); }
    state.loading = false;
    render();
  }

  /* ================= 渲染 ================= */
  function catOf(s) { return Skills.skillCat(s); }
  function listOf(cat) {
    return Skills.state.list
      .filter(s => Skills.skillListable(s) && catOf(s) === cat)
      .sort((a, b) => (Number(b.used_count || 0) - Number(a.used_count || 0)) || String(a.title).localeCompare(String(b.title)));
  }

  function render() {
    renderSummary();
    renderCatBar();
    renderRows();
  }

  function renderSummary() {
    const el = $("#prSummary");
    if (!el) return;
    const all = Skills.state.list.filter(Skills.skillListable);
    const enabled = all.filter(s => s.enabled).length;
    const defIds = Skills.defaultSkillIds().filter(id => all.some(s => s.id === id));
    const uses = all.reduce((n, s) => n + (Number(s.used_count) || 0), 0);
    const sysOn = (Skills.sysSkillOn("think") ? 1 : 0) + (Skills.sysSkillOn("search") ? 1 : 0);
    el.innerHTML =
      stat(all.length, "条提示词") +
      stat(enabled, "已启用", "ok") +
      stat(defIds.length + "/" + Skills.MAX_DEFAULT_SKILLS, "默认对话", defIds.length ? "ok" : "") +
      stat(uses, "累计使用") +
      stat(sysOn + "/2", "系统级开启", sysOn ? "ok" : "");
  }
  function stat(v, k, tone) {
    return '<div class="prov-stat"><span class="prov-stat-k">' + escapeHTML(k) + '</span><b class="' + (tone || "") + '">' + escapeHTML(String(v)) + "</b></div>";
  }

  function renderCatBar() {
    const el = $("#prCatBar");
    if (!el) return;
    el.innerHTML = Skills.SKILL_CATS.map(c => {
      const n = listOf(c).length;
      return '<button class="sk-cat' + (c === state.cat ? " sel" : "") + '" data-cat="' + escapeHTML(c) + '">' + escapeHTML(c) + " <b>" + n + "</b></button>";
    }).join("");
  }

  function renderRows() {
    const el = $("#prRows");
    if (!el) return;
    const cat = Skills.SKILL_CATS.includes(state.cat) ? state.cat : Skills.SKILL_CATS[0];
    const all = listOf(cat);
    const pages = Math.max(1, Math.ceil(all.length / PAGE_SIZE));
    state.pages[cat] = Math.min(Math.max(1, state.pages[cat] || 1), pages);
    const cur = state.pages[cat];
    const list = all.slice((cur - 1) * PAGE_SIZE, cur * PAGE_SIZE);
    const defIds = Skills.defaultSkillIds().filter(id => Skills.state.list.some(s => s.id === id));

    let html = "";
    // 默认对话徽标：一眼看到每次发送必带的是哪几个
    html += '<div class="sk-def-badge">默认对话 <b>' + defIds.length + "/" + Skills.MAX_DEFAULT_SKILLS + "</b>" +
      (defIds.length ? "：" + defIds.map(id => { const s = Skills.state.list.find(x => x.id === id); return s ? escapeHTML(s.title) : id; }).join("、") : "（未设置——在条目上点「设为默认」）") + "</div>";
    if (!all.length) {
      html += '<div class="empty-state"><div class="empty-icon">✦</div><h2>「' + escapeHTML(cat) + '」下还没有提示词</h2><p>点右上角「＋ 新建提示词」——只需标题与提示词正文，发送时作为 system 消息注入。</p></div>';
    } else {
      for (const s of list) {
        const isDef = defIds.includes(s.id);
        const full = defIds.length >= Skills.MAX_DEFAULT_SKILLS;
        html += '<article class="sk-row card" data-id="' + escapeHTML(s.id) + '">' +
          '<div class="sk-row-top"><strong class="sk-row-ttl">' + escapeHTML(s.title) + "</strong>" +
          '<span class="sk-row-cat">' + escapeHTML(catOf(s)) + "</span>" +
          (s.enabled ? "" : '<span class="cap-badge off">已停用</span>') +          '<span class="sk-row-use" title="该提示词被发送带上的次数">已用 ' + (Number(s.used_count) || 0) + " 次</span></div>" +
          '<p class="sk-row-desc">' + escapeHTML(s.summary || Skills.summarizePrompt(s.prompt)) + "</p>" +
          '<pre class="sk-row-body">' + escapeHTML(s.prompt) + "</pre>" +
          '<div class="sk-row-foot">' +
          '<button class="soft-btn" data-act="toggle">' + (s.enabled ? "停用" : "启用") + "</button>" +
          (isDef
            ? '<button class="soft-btn def-on" data-act="def">★ 默认对话中 · 取消</button>'
            : '<button class="soft-btn" data-act="def"' + (full ? ' disabled title="默认对话最多 ' + Skills.MAX_DEFAULT_SKILLS + ' 个，先取消一个"' : "") + ">设为默认对话</button>") +
          '<button class="soft-btn" data-act="edit">编辑</button>' +
          '<button class="soft-btn danger" data-act="del">删除</button>' +
          "</div></article>";
      }
    }
    el.innerHTML = html;
    const pg = $("#prPager");
    if (pg) {
      if (pages > 1) {
        pg.hidden = false;
        $("#prPageN").textContent = cur + " / " + pages + " · 共 " + all.length + " 个";
        $("#prPrev").disabled = cur <= 1;
        $("#prNext").disabled = cur >= pages;
      } else pg.hidden = true;
    }
  }

  /* ================= 操作 ================= */
  async function toggleEnabled(id) {
    const s = Skills.state.list.find(x => x.id === id);
    if (!s) return;
    const next = !s.enabled;
    try {
      await Skills.api.update(id, { enabled: next });
      await Skills.load();
      render();
      onChange();
      toast(next ? "已启用（对话页可选）" : "已停用（对话页不可选）", "ok");
    } catch (e) { toast("操作失败：" + (e.message || e), "err"); }
  }

  function toggleDefault(id) {
    if (Skills.toggleDefaultSkill(id)) { render(); onChange(); }
  }

  async function del(id) {
    const s = Skills.state.list.find(x => x.id === id);
    if (!s) return;
    if (!window.confirm("确定删除提示词「" + s.title + "」？删除后正在默认对话中的也会被移除。")) return;
    try {
      await Skills.api.remove(id);
      // 联动：默认列表与手选态清理（否则持有已不存在的 id，静默失效）
      const dIds = Skills.defaultSkillIds();
      if (dIds.includes(id)) Skills.saveDefaultSkillIds(dIds.filter(x => x !== id));
      if (Skills.state.active && Skills.state.active.id === id) Skills.state.active = null;
      await Skills.load();
      render();
      onChange();
      toast("已删除", "ok");
    } catch (e) { toast("删除失败：" + (e.message || e), "err"); }
  }

  /* ================= 对话框 ================= */
  function openDlg(editId) {
    const dlg = $("#skill-dlg");
    if (!dlg) return;
    const s = editId ? Skills.state.list.find(x => x.id === editId) : null;
    state.editId = s ? s.id : null;
    $("#skDlgTitle").textContent = s ? "编辑提示词：" + s.title : "新建提示词";
    $("#sk-title").value = s ? s.title : "";
    $("#sk-prompt").value = s ? s.prompt : "";
    $("#sk-summary").value = s ? (s.summary || "") : "";
    $("#sk-enabled").value = s ? (s.enabled ? "1" : "0") : "1";
    $("#sk-category").value = s ? Skills.skillCat(s) : Skills.SKILL_CATS[0];
    $("#sk-tier").value = s ? Skills.skillTier(s) : "normal";
    $("#sk-dlg-err").textContent = "";
    try { dlg.showModal(); } catch (e) {}
    $("#sk-title").focus();
  }

  async function save() {
    const dlg = $("#skill-dlg");
    const editId = state.editId;
    const title = $("#sk-title").value.trim();
    const prompt = $("#sk-prompt").value;
    let summary = $("#sk-summary").value.trim();
    if (summary.length > SUMMARY_MAX) summary = summary.slice(0, SUMMARY_MAX);
    if (!summary) summary = Skills.summarizePrompt(prompt);
    const enabled = $("#sk-enabled").value === "1";
    const category = Skills.SKILL_CATS.includes($("#sk-category").value) ? $("#sk-category").value : Skills.SKILL_CATS[0];
    const tier = $("#sk-tier").value === "system" ? "system" : "normal";
    const err = $("#sk-dlg-err");
    if (!title) { err.textContent = "标题不能为空"; return; }
    if (!prompt.trim()) { err.textContent = "提示词正文不能为空"; return; }
    const body = { title, prompt, summary, enabled, category, tier };
    try {
      if (editId) await Skills.api.update(editId, body);
      else await Skills.api.create(body);
      try { dlg.close(); } catch (e) {}
      await Skills.load();
      render();
      onChange();
      toast(editId ? "已保存" : "已创建", "ok");
    } catch (e) { err.textContent = "保存失败：" + (e.message || e); }
  }

  /* ================= 装配 ================= */
  function init(onChangeCb) {
    if (typeof onChangeCb === "function") onChange = onChangeCb;
    $("#prAdd").onclick = () => openDlg(null);
    $("#prRefresh").onclick = () => load();
    $("#prPrev").onclick = () => { const c = state.cat; state.pages[c] = Math.max(1, (state.pages[c] || 1) - 1); renderRows(); };
    $("#prNext").onclick = () => { const c = state.cat; state.pages[c] = (state.pages[c] || 1) + 1; renderRows(); };
    $("#prCatBar").addEventListener("click", e => {
      const b = e.target.closest("button[data-cat]");
      if (!b) return;
      state.cat = b.dataset.cat;
      render();
    });
    $("#prRows").addEventListener("click", e => {
      const b = e.target.closest("button[data-act]");
      if (!b) return;
      const id = b.closest("[data-id]").dataset.id;
      const act = b.dataset.act;
      if (act === "toggle") toggleEnabled(id);
      else if (act === "def") toggleDefault(id);
      else if (act === "edit") openDlg(id);
      else if (act === "del") del(id);
    });
    $("#sk-save").onclick = save;
    $("#sk-cancel").onclick = () => { try { $("#skill-dlg").close(); } catch (e) {} };
    $("#sk-close").onclick = () => { try { $("#skill-dlg").close(); } catch (e) {} };
    // 首次装载（数据层 load 有幂等；这里兜底视图初始渲染）
    load();
  }

  global.QuakePrompts = {
    state, init, load,
    // 供测试/调试直接调用
    openDlg, save, toggleEnabled, toggleDefault, del, listOf,
  };
})(window);
