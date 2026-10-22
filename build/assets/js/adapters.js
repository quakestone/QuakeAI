/* ============================================================
   QuakeAI adapters.js —— UI 适配层（唯一 WASM 边界）
   铁律：
   - 本文件是全应用唯一 import WASM 胶水（build/assets/wasm/index.js）的地方；
   - 所有网关差异（Request 构建 / x-gw-sub / 自定义头 / percent 解码 / SSE 解析 /
     错误语义 / Abort）在此层消化，UI 只面对本适配器的语义化接口；
   - 不吞错、不伪造成功；不修改 gw.* 的签名、参数与返回结构。
   ============================================================ */
(function (global) {
  "use strict";

  const { config } = global.QuakeStore;

  /* HTTP 头不允许非 ASCII：网关把中文渠道名/ID 做了 percent-encoding，这里还原。
     解码失败（异常值）时原样返回，不抛错。 */
  function decodeHeader(v) {
    if (!v) return "";
    try { return decodeURIComponent(v); } catch (e) { return v; }
  }

  /* 网关自定义请求头（与旧 UI 口径一致，键名不变）：
     x-admin-token 管理面软锁 / x-device-keys 本设备 Key 覆盖 /
     x-gw-features 系统级能力注入（B1 起由 skills 三流填充）。 */
  function gwHeaders(extra) {
    const h = Object.assign({}, extra || {});
    const token = config.get("gw_admin_token", "");
    if (token) h["x-admin-token"] = token;
    const dk = config.get("gw_device_keys", "");
    if (dk) h["x-device-keys"] = dk;
    return h;
  }

  class GatewayAdapter {
    constructor() {
      this._gw = null;
      this._initPromise = null;   // init 幂等：并发调用共用同一个 Promise，防 WASM 初始化竞态
      this._failed = false;
    }

    /* 装载 WASM（幂等）。失败时 _failed=true，后续调用直接抛可读错误，不死循环重试。 */
    init() {
      if (this._gw) return Promise.resolve(this._gw);
      if (this._failed) return Promise.reject(new Error("网关 WASM 加载失败，请刷新页面重试"));
      if (!this._initPromise) {
        this._initPromise = (async () => {
          const mod = await import("../wasm/index.js");
          const gw = await mod.default();
          this._gw = gw;
          return gw;
        })().catch(e => {
          this._failed = true;
          this._initPromise = null;
          throw new Error("网关 WASM 加载失败：" + (e && e.message ? e.message : String(e)));
        });
      }
      return this._initPromise;
    }

    get ready() { return !!this._gw; }

    /* ---- /health ---- */
    async health() {
      const gw = await this.init();
      const resp = await gw.health();
      const j = await resp.json().catch(() => ({}));
      if (!resp.ok) throw new Error(j.error && j.error.message || ("health HTTP " + resp.status));
      return j;
    }

    /* ---- /admin/api/* ---- 统一管理面入口。
       注意 x-gw-sub 头：WASM admin() 从请求头读子路径（去掉 /admin 前缀），
       与旧 UI __gwDispatch 的约定一致——URL 与头必须同时正确。 */
    async admin(path, opts) {
      opts = opts || {};
      const method = (opts.method || "GET").toUpperCase();
      const gw = await this.init();
      const headers = gwHeaders(Object.assign({ "Content-Type": "application/json", "x-gw-sub": "/api" + path }, opts.headers || {}));
      const req = new Request("/admin/api" + path, {
        method, headers,
        body: opts.body === undefined || opts.body === null ? null : JSON.stringify(opts.body),
      });
      const resp = await gw.admin(req);
      const j = await resp.json().catch(() => ({}));
      if (!resp.ok) throw new Error(j.error && j.error.message || ("HTTP " + resp.status));
      return j;
    }

    /* ---- 渠道 ---- */
    async channels() {
      const j = await this.admin("/channels");
      return Array.isArray(j.channels) ? j.channels : (Array.isArray(j) ? j : []);
    }
    async createChannel(body) { return this.admin("/channels", { method: "POST", body }); }
    async updateChannel(id, body) { return this.admin("/channels/" + encodeURIComponent(id), { method: "PUT", body }); }
    async removeChannel(id) { return this.admin("/channels/" + encodeURIComponent(id), { method: "DELETE" }); }
    async testChannel(id) { return this.admin("/channels/" + encodeURIComponent(id) + "/test", { method: "POST", body: {} }); }

    /* ---- 对话（流式 SSE） ----
       chat({messages, model, features, signal, onDelta})
       onDelta({content, reasoning}) 每到达一个内容增量调用；
       返回 {content, reasoning, channel, attempts, model, usage, ttfbMs}。
       取消即 Abort（signal.abort()），上游读取循环随 Response 流中止。 */
    async chat(o) {
      o = o || {};
      const gw = await this.init();
      const body = { messages: o.messages || [], stream: true, stream_options: { include_usage: true } };
      if (o.model) body.model = o.model;
      const headers = gwHeaders({
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
      });
      if (o.features && o.features.length) headers["x-gw-features"] = o.features.join(",");
      const t0 = (global.performance && performance.now) ? performance.now() : 0;
      const resp = await gw.chat(new Request("/v1/chat/completions", {
        method: "POST", headers, body: JSON.stringify(body), signal: o.signal,
      }));
      const meta = {
        channel: decodeHeader(resp.headers.get("x-gateway-channel")),
        attempts: decodeHeader(resp.headers.get("x-gateway-attempts")),
        model: decodeHeader(resp.headers.get("x-gateway-model")),
      };
      if (!resp.ok) {
        const j = await resp.json().catch(() => ({}));
        throw new Error((j.error && j.error.message) || ("HTTP " + resp.status));
      }
      if (!resp.body) throw new Error("响应无内容流");
      const reader = resp.body.getReader();
      const dec = new TextDecoder();
      const sink = { content: "", reasoning: "", usage: null };
      let buf = "", ttfbMs = 0;
      const feed = (block) => {
        const data = block.split("\n")
          .filter(l => l.startsWith("data:"))
          .map(l => l.slice(5).replace(/^ /, ""))
          .join("\n");
        if (!data || data === "[DONE]") return;
        try {
          const j = JSON.parse(data);
          const delta = j.choices && j.choices[0] && j.choices[0].delta;
          if (delta) {
            const before = sink.content.length;
            if (typeof delta.content === "string") sink.content += delta.content;
            if (typeof delta.reasoning_content === "string") sink.reasoning += delta.reasoning_content;
            if (!ttfbMs && sink.content.length > before && global.performance && performance.now) {
              ttfbMs = Math.round(performance.now() - t0);
            }
          }
          if (j.usage) sink.usage = j.usage;
        } catch (e) {}
      };
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        // 上游换行风格不统一：兼容 \n\n 与 \r\n\r\n，只认一种会切不出块
        let m;
        while ((m = /\r?\n\r?\n/.exec(buf))) {
          const block = buf.slice(0, m.index);
          buf = buf.slice(m.index + m[0].length);
          const before = sink.content.length;
          feed(block);
          if (o.onDelta && sink.content.length > before) {
            o.onDelta({ content: sink.content, reasoning: sink.reasoning });
          }
        }
      }
      if (buf.trim()) {
        feed(buf);
        if (o.onDelta) o.onDelta({ content: sink.content, reasoning: sink.reasoning });
      }
      return Object.assign({}, meta, { content: sink.content, reasoning: sink.reasoning, usage: sink.usage, ttfbMs });
    }

    /* ---- 语音（B3 接入；壳层先定义边界） ---- */
    async speech(payload) {
      const gw = await this.init();
      const req = new Request("/v1/audio/speech", {
        method: "POST",
        headers: gwHeaders({ "Content-Type": "application/json" }),
        body: JSON.stringify(payload),
      });
      return gw.audio_speech(req);
    }
    async transcribe(formData) {
      const gw = await this.init();
      const req = new Request("/v1/audio/transcriptions", {
        method: "POST",
        headers: gwHeaders({}),
        body: formData,
      });
      return gw.audio_transcriptions(req);
    }

    /* ---- 运维 ---- */
    async diag() { return this.admin("/diag"); }
    async events() { return this.admin("/events"); }
    async usage() { return this.admin("/usage"); }
  }

  /* ---------- 可用模型口径（与旧 UI 一致：effModels + 可用渠道过滤） ---------- */
  function effModels(c) {
    if (c.models_meta && c.models_meta.length) return c.models_meta;
    return (c.models || []).map(name => ({
      name,
      kind: c.kind === "audio" ? "stt" : (/glm-4v|glm-4\.1v|-vl|vision/i.test(name) ? "vision" : "text"),
      alt_kind: "", enabled: true, priority: 50, health: null,
    }));
  }
  function usableChatChannels(channels) {
    const now = Date.now();
    return (channels || []).filter(c => c.enabled && c.kind === "chat" && c.api_key_masked && c.api_key_masked !== "未登记"
      && !(c.health && c.health.cooldown_until_ms && c.health.cooldown_until_ms > now));
  }
  /* 对话可用模型清单：[{name, channel, kind}]，同名多渠道只保留优先渠道 */
  function usableChatModels(channels) {
    const out = [];
    for (const c of usableChatChannels(channels)) {
      for (const m of effModels(c)) {
        if (m.enabled === false) continue;
        if (m.kind !== "text" && m.alt_kind !== "text") continue;
        if (out.some(x => x.name === m.name)) continue;
        out.push({ name: m.name, channel: c.name || c.id, kind: m.kind });
      }
    }
    return out;
  }

  global.QuakeAdapters = {
    GatewayAdapter,
    gateway: new GatewayAdapter(),   // 单例：UI 只与单例对话，防重复装载/多源不一致
    effModels, usableChatChannels, usableChatModels, decodeHeader, gwHeaders,
  };
})(window);
