/* ============================================================
   QuakeAI attachments.js —— 附件管线（F4）
   口径与旧 UI 完全一致：上限常量 / kind 判定 / dataUrl / 视频抽帧 /
   API content 构建 / 发送前校验。持久化形态见 store.js slimAtt。
   ============================================================ */
(function (global) {
  "use strict";

  const LIMITS = { text: 1048576, skill: 102400, media: 3145728, file: 2097152 * 10 };
  const KIND_CAPS = { image: 5, audio: 3, video: 1, file: 5 };
  const KIND_LABEL = { image: "图片", audio: "音频", video: "视频", file: "文件" };

  function byteLen(s) {
    try { return new TextEncoder().encode(String(s || "")).length; }
    catch { return String(s || "").length; }
  }
  function attCount(list, kind) { return (list || []).filter(a => a.kind === kind).length; }
  /// 多媒体合计体积：图片+音频+视频（不含文件——文件不做任何读取与处理）
  function mediaBytes(list) {
    return (list || []).filter(a => a.kind !== "file").reduce((s, a) => s + (a.size || 0), 0);
  }
  function fmtSize(b) {
    if (b == null) return "";
    if (b < 1024) return b + "B";
    if (b < 1048576) return (b / 1024).toFixed(0) + "KB";
    return (b / 1048576).toFixed(1) + "MB";
  }
  function classify(file) {
    const t = file.type || "";
    if (t.startsWith("image/")) return "image";
    if (t.startsWith("audio/")) return "audio";
    if (t.startsWith("video/")) return "video";
    return "file";
  }
  function readAsDataURL(f) {
    return new Promise((res, rej) => {
      const r = new FileReader();
      r.onload = () => res(r.result);
      r.onerror = () => rej(r.error);
      r.readAsDataURL(f);
    });
  }

  /* ---------- 视频抽帧（旧实现原口径迁移） ---------- */
  function videoFrameAt(file, ratio, width) {
    return new Promise(res => {
      let v;
      try { v = document.createElement("video"); } catch { return res(null); }
      let url;
      try { url = URL.createObjectURL(file); } catch { return res(null); }
      const finish = frame => {
        try { URL.revokeObjectURL(url); } catch {}
        try { v.pause(); v.src = ""; } catch {}
        res(frame);
      };
      const timer = setTimeout(() => finish(null), 8000);
      v.muted = true; v.playsInline = true; v.preload = "metadata";
      v.onloadeddata = () => {
        try { v.currentTime = Math.max(0.05, Math.min((v.duration || 1) * ratio, (v.duration || 1) - 0.05)); }
        catch { clearTimeout(timer); finish(null); }
      };
      v.onseeked = () => {
        clearTimeout(timer);
        try {
          const vw = v.videoWidth || 0, vh = v.videoHeight || 0;
          if (!vw || !vh) return finish(null);
          const w = width || 200, h = Math.max(1, Math.round(w * vh / vw));
          const c = document.createElement("canvas");
          c.width = w; c.height = h;
          c.getContext("2d").drawImage(v, 0, 0, w, h);
          finish(c.toDataURL("image/jpeg", 0.72));
        } catch { finish(null); }
      };
      v.onerror = () => { clearTimeout(timer); finish(null); };
      v.src = url;
    });
  }
  function videoPoster(file) { return videoFrameAt(file, 0.02, 200); }

  /* ---------- API content 构建（与旧 UI 同口径） ----------
     无 desc 的图片走 image_url 直连；其余（含已理解的图/视频/音频/文件）
     以 [附件: …] / [图片 X 理解：…] 注记拼进文本。 */
  function buildApiContent(m) {
    const atts = m.attachments || [];
    if (!atts.length) return m.content || "";
    const directImgs = atts.filter(a => a.kind === "image" && a.dataUrl && !a.desc);
    const notes = atts.filter(a => !(a.kind === "image" && a.dataUrl && !a.desc))
      .map(a => a.desc
        ? `[${a.kind === "image" ? "图片" : a.kind === "video" ? "视频" : "附件"} ${a.name} 理解：${a.desc}]`
        : `[附件: ${a.name}${a.size ? `（${fmtSize(a.size)}）` : ""}]`);
    let text = m.content || "";
    if (notes.length) text = text ? notes.join("\n") + "\n" + text : notes.join("\n");
    if (!directImgs.length) return text;
    const parts = [];
    if (text) parts.push({ type: "text", text });
    for (const im of directImgs) parts.push({ type: "image_url", image_url: { url: im.dataUrl } });
    return parts;
  }
  function attHasMedia(atts) {
    return (atts || []).some(a => a.kind === "image" || a.kind === "video");
  }

  /* ---------- 发送前校验（防御 + 明确提示） ----------
     返回 null 表示通过，否则返回错误提示串。 */
  function validateSendPayload(text, atts) {
    const tb = byteLen(text);
    if (tb > LIMITS.text) return `文本内容 ${fmtSize(tb)} 超过 1MB 上限，请删减后再发送`;
    for (const k of Object.keys(KIND_CAPS)) {
      const n = attCount(atts, k);
      if (n > KIND_CAPS[k]) return `${KIND_LABEL[k]} ${n} 个超过上限 ${KIND_CAPS[k]} 个，请移除后再发送`;
    }
    const mb = mediaBytes(atts);
    if (mb > LIMITS.media) return `多媒体（图片+音频+视频）合计 ${fmtSize(mb)} 超过 3MB 上限，请移除一些再发送`;
    return null;
  }

  /* ---------- 入口硬拦：文件挑选 → 附件实例（旧 UI 同规则） ----------
     onAdd(att) 每个成功入列的附件回调（供 UI 渲染/抽帧）。 */
  async function pickFiles(files, current, onAdd) {
    let added = 0;
    for (const f of Array.from(files || [])) {
      if (f.size > LIMITS.file) { global.QuakeComponents.toast(`${f.name} 超过 20MB 单文件上限，已跳过`, "err"); continue; }
      const kind = classify(f);
      const cap = KIND_CAPS[kind], cur = attCount(current, kind);
      if (cur >= cap) {
        global.QuakeComponents.toast(`${KIND_LABEL[kind]}最多 ${cap} 个（已有 ${cur} 个），「${f.name}」未添加`, "err");
        continue;
      }
      if (kind !== "file") {
        const used = mediaBytes(current);
        if (used + f.size > LIMITS.media) {
          global.QuakeComponents.toast(`多媒体（图片+音频+视频）合计最多 3MB，当前 ${fmtSize(used)}，「${f.name}」有 ${fmtSize(f.size)}，未添加`, "err");
          continue;
        }
      }
      const att = { name: f.name, size: f.size, type: f.type || "application/octet-stream", kind, file: f };
      if (kind === "image") { try { att.dataUrl = await readAsDataURL(f); } catch { /* 无预览也能发 */ } }
      current.push(att);
      added++;
      if (onAdd) onAdd(att);
      // 视频首帧缩略图异步抽取：不阻塞后续文件与发送
      if (kind === "video") videoPoster(f).then(p => {
        if (p && current.includes(att)) { att.poster = p; if (global.QuakeUI) global.QuakeUI.onAttachmentsChanged(); }
      }).catch(() => {});
    }
    if (added && added < files.length) global.QuakeComponents.toast(`已添加 ${added} 个，${files.length - added} 个超限未添加`, "err");
    return added;
  }

  global.QuakeAttachments = {
    LIMITS, KIND_CAPS, KIND_LABEL,
    byteLen, attCount, mediaBytes, fmtSize, classify, readAsDataURL,
    videoFrameAt, videoPoster, buildApiContent, attHasMedia,
    validateSendPayload, pickFiles,
  };
})(window);
