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
  /* 附件气泡：图片缩略图 / 音视频播放器 / 文件 pill；desc = 发送流水线的理解产出
     （与请求里发给模型的注记同源——用户看到的就是模型看到的）。
     音频/视频的 Blob 经 slimAtt 持久化，刷新后 createObjectURL 仍可播放。 */
  function attMediaUrl(a) {
    try { if (a.file) return URL.createObjectURL(a.file); } catch (e) {}
    return "";
  }
  function attachChips(atts) {
    return (atts || []).map(a => {
      const descLine = a.desc
        ? '<div class="att-desc">' + (a.kind === "image" ? "🖼" : a.kind === "video" ? "🎬" : "📎") + " 理解：" + escapeHTML(a.desc) + "</div>" : "";
      const size = a.size ? "（" + fmtAttSize(a.size) + "）" : "";
      if (a.kind === "image" && a.dataUrl) return '<div class="att-img-wrap"><img class="att-thumb" src="' + a.dataUrl + '" alt="' + escapeHTML(a.name) + '">' + descLine + "</div>";
      const src = attMediaUrl(a);
      if (a.kind === "audio" && src) return '<div class="att-media"><div class="att-media-ic">🎵</div><audio class="att-audio" controls preload="metadata" src="' + src + '"></audio><div class="att-media-nm">' + escapeHTML(a.name) + size + "</div>" + descLine + "</div>";
      if (a.kind === "video" && src) return '<div class="att-media"><video class="att-video" controls preload="metadata" playsinline' + (a.poster ? ' poster="' + a.poster + '"' : "") + ' src="' + src + '"></video><div class="att-media-nm">' + escapeHTML(a.name) + size + "</div>" + descLine + "</div>";
      return '<div class="att-file-wrap"><span class="att-file">📎 ' + escapeHTML(a.name) + size + "</span>" + descLine + "</div>";
    }).join("");
  }
  function fmtAttSize(b) {
    if (b == null) return "";
    if (b < 1024) return b + "B";
    if (b < 1048576) return (b / 1024).toFixed(0) + "KB";
    return (b / 1048576).toFixed(1) + "MB";
  }
  function message({ role, text, id, meta, atts, retry }) {
    const el = document.createElement("article");
    el.dataset.messageId = id || "";
    if (role === "divider") {
      el.className = "ctx-div";
      el.innerHTML = "<span>" + escapeHTML(text || "上下文已重置") + "</span>";
      return el;
    }
    el.className = "message " + role;
    const avatar = role === "assistant" ? BRAND_AVATAR : "Q", label = role === "assistant" ? "QuakeAI" : "你";
    const attRow = (atts && atts.length) ? '<div class="att-row">' + attachChips(atts) + "</div>" : "";
    const actions = role === "assistant"
      ? '<button data-action="copy">复制</button><button data-action="share">分享</button>' + (retry ? '<button data-action="retry">重新生成</button>' : "")
      : '<button data-action="copy">复制</button><button data-action="edit">修改</button><button data-action="retry">重试</button>';
    el.innerHTML =
      '<div class="message-avatar">' + avatar + '</div>' +
      '<div class="message-content">' +
      '<div class="message-role">' + label + '</div>' +
      attRow +
      '<div class="message-text"></div>' +
      (meta ? '<div class="msg-meta"></div>' : "") +
      '<div class="message-actions">' + actions + '</div></div>';
    const textEl = el.querySelector(".message-text");
    if (textEl) textEl.innerHTML = md(text || "");
    if (meta) el.querySelector(".msg-meta").innerHTML = meta;
    return el;
  }

  /* 流式消息：返回 { el, setText(text), setError(msg), setMeta(html), finish() }。
     只改自己这条的 .message-text，不重建整列——历史消息的 DOM/选区/滚动不被打断。
     retry：终态是否需要「重新生成」按钮（流式中由 CSS 隐藏，finish 后现身）。 */
  function streamMessage({ role, id, retry }) {
    const el = message({ role, text: "", id, meta: true, retry: !!retry });
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

  global.QuakeComponents = { escapeHTML, esc, md, message, streamMessage, toast, healthBadge, attachChips, attMediaUrl, fmtAttSize };
})(window);
