/* ============================================================
   QuakeAI share.js —— 分享链接构建（纯函数层）
   口径与旧 UI 完全一致：回答内容压缩后放进 URL hash
  （deflate-raw → base64url，服务端零存储），落地页 /s 渲染
  （src/share_ui.html，随仓库部署）。
   二级子域优先（s.<根域>），平台域（github.io 等）回落同源 /s。
   ============================================================ */
(function (global) {
  "use strict";

  const SHARE_MAX = 6000; // 内容上限：再长二维码会密到扫不动

  function b64urlFromBytes(bytes) {
    let s = "";
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  function b64urlUnicode(text) { // 无 CompressionStream 时的降级：明文 base64url
    return btoa(unescape(encodeURIComponent(text))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  async function deflateB64url(text) {
    const cs = new CompressionStream("deflate-raw");
    const buf = new Uint8Array(await new Response(new Blob([text]).stream().pipeThrough(cs)).arrayBuffer());
    return b64urlFromBytes(buf);
  }
  /// 二级子域优先：www.x.com / app.x.com → s.x.com；localhost/IP 回落同源 /s。
  /// 平台域（github.io / workers.dev…）用户无权配 s.<子域>（打开必然 404），必须回落同源。
  const PLATFORM_SUFFIXES = ["workers.dev", "vercel.app", "github.io", "pages.dev", "netlify.app",
    "render.com", "onrender.com", "fly.dev", "railway.app", "up.railway.app", "herokuapp.com",
    "glitch.me", "repl.co", "surge.sh", "web.app", "firebaseapp.com", "appspot.com",
    "trycloudflare.com", "ngrok.io", "loca.lt", "csb.app", "gitpod.io", "azurewebsites.net"];
  function shareOrigin() {
    const h = location.hostname;
    const same = () => location.origin;
    if (h === "localhost" || !h.includes(".") || /^\d+(\.\d+){3}$/.test(h)) return same();
    if (PLATFORM_SUFFIXES.some(s => h === s || h.endsWith("." + s))) return same();
    const labels = h.split(".");
    const root = labels.length >= 3 ? labels.slice(1).join(".") : h;
    return location.protocol + "//s." + root;
  }
  async function buildShareUrl(text, model) {
    let c = text;
    if (c.length > SHARE_MAX) c = c.slice(0, SHARE_MAX) + "\n\n…（内容过长已截断，打开应用查看全文）";
    const payload = { v: 1, t: c.slice(0, 30), c, m: model || "", ts: Date.now() };
    const json = JSON.stringify(payload);
    const frag = typeof CompressionStream !== "undefined" ? "z." + await deflateB64url(json) : "p." + b64urlUnicode(json);
    return shareOrigin() + "/s#" + frag;
  }

  global.QuakeShare = { SHARE_MAX, b64urlFromBytes, b64urlUnicode, deflateB64url, shareOrigin, buildShareUrl };
})(window);
