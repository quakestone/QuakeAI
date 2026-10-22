/* ============================================================
   QuakeAI store.js —— 持久化单一数据源（localStorage + IndexedDB）
   硬约束（UI 重建铁律）：键名 / 库名 / 表结构 / 迁移逻辑与旧 UI 完全一致，
   旧版本写入的 gw_* 与 IndexedDB 数据必须无损读取、可续用。
   本模块是 UI 层唯一允许触碰持久化的地方。
   ============================================================ */
(function (global) {
  "use strict";

  /* ---------- localStorage：gw_* 配置项（键名不变） ---------- */
  const config = {
    get(key, dflt) {
      try { const v = localStorage.getItem(key); return v === null ? dflt : v; }
      catch (e) { return dflt; }
    },
    set(key, val) {
      try { localStorage.setItem(key, String(val)); return true; }
      catch (e) { return false; }
    },
    remove(key) {
      try { localStorage.removeItem(key); } catch (e) {}
    },
    getJSON(key, dflt) {
      try { const v = JSON.parse(localStorage.getItem(key) || "null"); return v === null ? dflt : v; }
      catch (e) { return dflt; }
    },
    setJSON(key, val) {
      try { localStorage.setItem(key, JSON.stringify(val)); return true; }
      catch (e) { return false; }
    },
  };

  /* ---------- 主题：gw_theme（与旧 UI 同键） ---------- */
  const theme = {
    get() { return config.get("gw_theme", "light"); },
    set(v) { config.set("gw_theme", v === "dark" ? "dark" : "light"); },
  };

  /* ---------- IndexedDB：gw_convos（库名/版本/表/索引不变） ---------- */
  const IDB = {
    db: null, NAME: "gw_convos", VER: 1,
    open() {
      return new Promise((res, rej) => {
        let r;
        try { r = indexedDB.open(this.NAME, this.VER); } catch (e) { return rej(e); }
        r.onupgradeneeded = (e) => {
          const db = e.target.result;
          if (!db.objectStoreNames.contains("convos")) {
            const cs = db.createObjectStore("convos", { keyPath: "id" });
            cs.createIndex("updated", "updated");
          }
          if (!db.objectStoreNames.contains("msgs")) {
            // 复合主键（convoId, idx）：重发截断后同位置重新入列 = 覆盖，不会重复
            const ms = db.createObjectStore("msgs", { keyPath: ["convoId", "idx"] });
            ms.createIndex("by_convo", "convoId");
          }
        };
        r.onsuccess = () => { this.db = r.result; res(this.db); };
        r.onerror = () => rej(r.error);
      });
    },
    _req(req) { return new Promise((res, rej) => { req.onsuccess = () => res(req.result); req.onerror = () => rej(req.error); }); },
    putConvo(c) {
      if (!this.db) return Promise.resolve();
      // tokens/answers/memory/title_auto 由工作流引擎维护；老会话这些字段为
      // undefined，结构化克隆原样跳过，读取不受影响。
      return this._req(this.db.transaction("convos", "readwrite").objectStore("convos")
        .put({ id: c.id, title: c.title, model: c.model, updated: c.updated,
          tokens: c.tokens, answers: c.answers, memory: c.memory, memory_at: c.memory_at, title_auto: c.title_auto, memfsm: c.memfsm }));
    },
    putMsg(convoId, idx, m) {
      if (!this.db) return Promise.resolve();
      return this._req(this.db.transaction("msgs", "readwrite").objectStore("msgs")
        .put({ convoId, idx, role: m.role, content: m.content, reasoning: m.reasoning, ts: m.ts,
          streaming: !!m.streaming, error: m.error, channel: m.channel, attempts: m.attempts,
          emphasis: !!m.emphasis,
          attachments: (m.attachments || []).map(slimAtt) }));
    },
    deleteMsg(convoId, idx) {
      if (!this.db) return Promise.resolve();
      return this._req(this.db.transaction("msgs", "readwrite").objectStore("msgs").delete([convoId, idx]));
    },
    deleteConvo(id) {
      if (!this.db) return Promise.resolve();
      const tx = this.db.transaction(["convos", "msgs"], "readwrite");
      tx.objectStore("convos").delete(id);
      tx.objectStore("msgs").index("by_convo").openCursor(IDBKeyRange.only(id)).onsuccess = (e) => {
        const cur = e.target.result;
        if (cur) { cur.delete(); cur.continue(); }
      };
      return new Promise((res) => { tx.oncomplete = res; tx.onerror = res; });
    },
    clearAll() {
      if (!this.db) return Promise.resolve();
      const tx = this.db.transaction(["convos", "msgs"], "readwrite");
      tx.objectStore("convos").clear(); tx.objectStore("msgs").clear();
      return new Promise((res) => { tx.oncomplete = res; tx.onerror = res; });
    },
    allConvos() {
      if (!this.db) return Promise.resolve([]);
      return this._req(this.db.transaction("convos", "readonly").objectStore("convos").getAll());
    },
    msgsOf(convoId) {
      if (!this.db) return Promise.resolve([]);
      return this._req(this.db.transaction("msgs", "readonly").objectStore("msgs").index("by_convo").getAll(convoId))
        .then(ms => ms.sort((a, b) => a.idx - b.idx));
    },
    allMsgs() {
      if (!this.db) return Promise.resolve([]);
      return this._req(this.db.transaction("msgs", "readonly").objectStore("msgs").getAll());
    },
  };

  /* ---------- 附件持久化形态（与旧 UI 同规则） ---------- */
  function slimAtt(a) {
    const base = { name: a.name, size: a.size, type: a.type, kind: a.kind };
    if (a.kind === "image" && a.dataUrl) base.dataUrl = a.dataUrl;
    if ((a.kind === "audio" || a.kind === "video") && a.file) base.file = a.file;
    if (a.kind === "video" && a.poster) base.poster = a.poster;
    if (a.desc) base.desc = a.desc;
    return base;
  }

  /* ---------- 会话 hydrate：IDB 为主，legacy gw_convos_v1 一次性迁移 ---------- */
  async function loadConversations() {
    await IDB.open();
    let stored = await IDB.allConvos();
    if (!stored.length) {
      const old = config.get("gw_convos_v1", "");
      if (old) {
        try {
          const list = JSON.parse(old) || [];
          for (const c of list) {
            await IDB.putConvo({ id: c.id, title: c.title, model: c.model, updated: c.updated });
            for (let i = 0; i < (c.msgs || []).length; i++) await IDB.putMsg(c.id, i, c.msgs[i]);
          }
          stored = await IDB.allConvos();
        } catch (e) {}
        config.set("gw_convos_v1_migrated", "1");
        config.remove("gw_convos_v1");
      }
    }
    stored.sort((a, b) => (b.updated || 0) - (a.updated || 0));
    const convos = await Promise.all(stored.map(async c => ({ ...c, msgs: await IDB.msgsOf(c.id) })));
    // 上次异常退出的流式消息：标记中断，避免下次打开还带光标
    for (const c of convos) for (const m of c.msgs) {
      if (m.streaming) { m.streaming = false; m.error = m.content ? "（生成中断）" : "（未完成）"; }
    }
    return convos;
  }

  global.QuakeStore = { config, theme, IDB, slimAtt, loadConversations };
})(window);
