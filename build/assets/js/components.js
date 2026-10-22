/* ============================================================
   QuakeAI components.js —— DOM 组件助手（零框架）
   - escapeHTML / md 渲染（口径与旧 UI 一致，历史消息渲染不缩水）
   - message：静态消息；streamMessage：带增量句柄的流式消息
   - toast：全局轻提示（ok/err 两态）
   - healthBadge：网关健康三态徽标
   ============================================================ */
(function (global) {
  "use strict";

  const escapeHTML = s => String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const esc = escapeHTML;

  /* ---------- Markdown（与旧 UI 同口径：代码块 + 块级 + 行内） ---------- */
  function mdInline(t) {
    return t
      .replace(/`([^`\n]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
      .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  }
  function mdBlock(t) {
    const lines = esc(t).split("\n");
    let out = "", i = 0;
    while (i < lines.length) {
      const ln = lines[i];
      if (/^\s*-\s+/.test(ln)) {
        let items = "";
        while (i < lines.length && /^\s*-\s+/.test(lines[i])) { items += "<li>" + mdInline(lines[i].replace(/^\s*-\s+/, "")) + "</li>"; i++; }
        out += "<ul>" + items + "</ul>";
      } else if (/^\s*\d+[.)]\s+/.test(ln)) {
        let items = "";
        while (i < lines.length && /^\s*\d+[.)]\s+/.test(lines[i])) { items += "<li>" + mdInline(lines[i].replace(/^\s*\d+[.)]\s+/, "")) + "</li>"; i++; }
        out += "<ol>" + items + "</ol>";
      } else if (/^#{1,4}\s+/.test(ln)) {
        out += "<h4>" + mdInline(ln.replace(/^#{1,4}\s+/, "")) + "</h4>"; i++;
      } else if (ln.trim() === "") { i++; }
      else {
        let para = ln;
        while (i + 1 < lines.length && lines[i + 1].trim() !== "" && !/^\s*(-|\d+[.)]|#{1,4}\s)/.test(lines[i + 1])) { para += "\n" + lines[++i]; }
        out += "<p>" + mdInline(para).replace(/\n/g, "<br>") + "</p>"; i++;
      }
    }
    return out;
  }
  function md(src) {
    const parts = String(src ?? "").split(/```([\w-]*)\n?([\s\S]*?)(?:```|$)/);
    let out = "";
    for (let i = 0; i < parts.length; i += 3) {
      out += mdBlock(parts[i] || "");
      if (i + 2 < parts.length) out += "<pre><code>" + esc(parts[i + 2]) + "</code></pre>";
    }
    return out;
  }

  /* ---------- 消息 ---------- */
  const BRAND_AVATAR = "Q";
  function message({ role, text, id, meta }) {
    const el = document.createElement("article");
    el.className = "message " + role;
    el.dataset.messageId = id || "";
    const avatar = role === "assistant" ? BRAND_AVATAR : "Q", label = role === "assistant" ? "QuakeAI" : "你";
    el.innerHTML =
      '<div class="message-avatar">' + avatar + '</div>' +
      '<div class="message-content">' +
      '<div class="message-role">' + label + '</div>' +
      '<div class="message-text"></div>' +
      (meta ? '<div class="msg-meta"></div>' : "") +
      '<div class="message-actions">' +
      (role === "assistant"
        ? '<button data-action="copy">复制</button><button data-action="retry">重新生成</button>'
        : '<button data-action="copy">复制</button>') +
      '</div></div>';
    el.querySelector(".message-text").innerHTML = md(text || "");
    if (meta) el.querySelector(".msg-meta").innerHTML = meta;
    return el;
  }

  /* 流式消息：返回 { el, setText(text), setError(msg), setMeta(html), finish() }。
     只改自己这条的 .message-text，不重建整列——历史消息的 DOM/选区/滚动不被打断。 */
  function streamMessage({ role, id }) {
    const el = message({ role, text: "", id, meta: true });
    el.classList.add("streaming");
    const textEl = el.querySelector(".message-text");
    const metaEl = el.querySelector(".msg-meta");
    textEl.classList.add("stream-plain");     // 流式中用纯文本，避免半截 markdown 抖动
    return {
      el,
      setText(t) { textEl.textContent = t; },
      setError(msg) { el.classList.add("error"); el.classList.remove("streaming"); textEl.classList.remove("stream-plain"); textEl.textContent = msg; },
      setMeta(html) { metaEl.innerHTML = html; },
      finish() {
        el.classList.remove("streaming");
        textEl.classList.remove("stream-plain");
        textEl.innerHTML = md(textEl.textContent || "");   // 终态一次渲染完整 markdown
      },
    };
  }

  /* ---------- 轻提示 ---------- */
  function toast(msg, kind) {
    const region = document.getElementById("toastRegion");
    if (!region) return;
    const el = document.createElement("div");
    el.className = "toast" + (kind === "err" ? " toast-err" : kind === "ok" ? " toast-ok" : "");
    el.textContent = msg;
    region.append(el);
    setTimeout(() => el.remove(), 3000);
  }

  /* ---------- 健康徽标（后端口径：unknown | healthy | degraded | down） ---------- */
  function healthBadge(state, label) {
    const s = state || "unknown";
    const cls = s === "down" ? "bad" : s;
    return '<span class="hb ' + cls + '">' + escapeHTML(label || ({ healthy: "健康", degraded: "降级", unknown: "未知", down: "故障", bad: "异常" }[s] || "未知")) + "</span>";
  }

  global.QuakeComponents = { escapeHTML, esc, md, message, streamMessage, toast, healthBadge };
})(window);
