/* ============================================================
   QuakeAI points.js —— 模型积分账本（个人使用偏好）
   口径与旧 UI 完全一致（localStorage 键 gw_model_points_v1）：
     每次模型选择 +1 · 每次使用 +1 · 每次分享 +10 · 每次降权 −20
   积分决定模型选择器排序（倒序），「自动」作为独立条目（__auto__）参与计分。
   key 用模型名（与 x-gateway-model 响应头同源）——选择/命中/降权三处
  拿到的都是模型名，统一成一种 key 才能记在同一条目上。
   同时承载「换模型重置上下文」开关（gw_ctx_reset）。
   ============================================================ */
(function (global) {
  "use strict";

  const { config } = global.QuakeStore;
  const { toast } = global.QuakeComponents;

  const POINTS_KEY = "gw_model_points_v1";
  const AUTO_KEY = "__auto__";
  const POINTS_RULE = { sel: 1, use: 1, share: 10, degrade: -20 };

  const Points = {
    data: { v: 1, models: {} },
    load() {
      try {
        const raw = localStorage.getItem(POINTS_KEY);
        if (raw) {
          const j = JSON.parse(raw);
          if (j && j.models) this.data = { v: 1, models: j.models };
        }
      } catch { /* 损坏则重置为空账本 */ }
      return this.data;
    },
    save() {
      try { localStorage.setItem(POINTS_KEY, JSON.stringify(this.data)); } catch { /* 配额满忽略 */ }
    },
    /// 取（并初始化）某条目的计数。key 传 null/空 = 自动条目。
    entry(key) {
      const k = key || AUTO_KEY;
      if (!this.data.models[k]) this.data.models[k] = { sel: 0, use: 0, share: 0, degrade: 0, score: 0 };
      return this.data.models[k];
    },
    /// 记一次事件并重算积分。kind: sel 选择 | use 使用 | share 分享 | degrade 降权
    add(kind, key, n = 1) {
      if (!(kind in POINTS_RULE)) return;
      const e = this.entry(key);
      e[kind] = (e[kind] || 0) + n;
      this.recalc(key);
      this.save();
    },
    /// 积分 = 选择×1 + 使用×1 + 分享×10 + 降权×(−20)
    recalc(key) {
      const k = key || AUTO_KEY;
      const e = this.entry(k);
      e.score =
        (e.sel || 0) * POINTS_RULE.sel +
        (e.use || 0) * POINTS_RULE.use +
        (e.share || 0) * POINTS_RULE.share +
        (e.degrade || 0) * POINTS_RULE.degrade;
      return e.score;
    },
    score(key) { return this.entry(key).score || 0; },
    /// 全部条目（按积分倒序）。并列时按名称排，保证顺序稳定可预期。
    list() {
      return Object.keys(this.data.models)
        .map(k => ({ key: k, ...this.data.models[k] }))
        .sort((a, b) => (b.score - a.score) || String(a.key).localeCompare(String(b.key)));
    },
    reset() { this.data = { v: 1, models: {} }; this.save(); },
  };
  Points.load();

  /// 从适配层 meta 结算积分（meta = {model, degraded}，头已由 adapters 层解码）：
  ///   · 使用 +1 —— 命中模型（x-gateway-model）；「自动」模式下自动条目也 +1
  ///   · 降权 −20 —— 本次被降权的模型（x-gateway-degraded，逗号分隔）
  /// 前端只在这里结算「使用/降权」，「选择/分享」由 pickModel / shareMsg 结算。
  function applyRoutePoints(meta, requestedModel) {
    try {
      const m = meta || {};
      if (m.model) {
        Points.add("use", m.model);
        if (!requestedModel) Points.add("use", AUTO_KEY);
      }
      if (m.degraded) {
        for (const x of m.degraded.split(",").map(s => s.trim()).filter(Boolean)) {
          Points.add("degrade", x);
        }
      }
    } catch (e) { /* 积分记账失败不能影响主流程 */ }
  }

  /* ---------- 换模型重置上下文（gw_ctx_reset，默认开） ---------- */
  const CTX_RESET_KEY = "gw_ctx_reset";
  function ctxResetOn() { return config.get(CTX_RESET_KEY, "1") !== "0"; }
  function setCtxReset(on) {
    config.set(CTX_RESET_KEY, on ? "1" : "0");
    toast(on ? "换模型将重置上下文" : "换模型保留上下文", on ? "ok" : "");
  }

  global.QuakePoints = { Points, AUTO_KEY, POINTS_RULE, POINTS_KEY, applyRoutePoints, ctxResetOn, setCtxReset };
})(window);
