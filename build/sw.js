/* QuakeAI 网关 · 纯静态 PWA Service Worker
 * 职责：预缓存应用壳（HTML / WASM / JS / 清单 / 图标），实现离线开壳。
 * 注意：网关数据请求（/v1 /admin /health）由页面内适配层直调浏览器 WASM，
 * 根本不会走到网络 / SW，这里无需也不应拦截它们。 */
const CACHE = "quakeai-gw-v2";
const PRECACHE = [
  "./",
  "./index.html",
  "./assets/css/tokens.css",
  "./assets/css/app.css",
  "./assets/css/gw.css",
  "./assets/js/store.js",
  "./assets/js/adapters.js",
  "./assets/js/components.js",
  "./assets/js/points.js",
  "./assets/js/magic.js",
  "./assets/js/skills.js",
  "./assets/js/attachments.js",
  "./assets/js/share.js",
  "./assets/js/qrcode.js",
  "./assets/js/app.js",
  "./assets/wasm/index.js",
  "./assets/wasm/index_bg.wasm",
  "./manifest.webmanifest",
  "./icon.svg",
];

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(PRECACHE)).then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    ).then(() => self.clients.claim())
  );
});

// 导航请求：网络优先，失败回落缓存壳；其余静态资源：缓存优先、回源更新。
self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;

  if (req.mode === "navigate") {
    e.respondWith(
      fetch(req).catch(() => caches.match("./index.html").then((r) => r || caches.match("./")))
    );
    return;
  }
  e.respondWith(
    caches.match(req).then((cached) => {
      if (cached) {
        // 后台更新
        fetch(req).then((net) => {
          if (net && net.ok) caches.open(CACHE).then((c) => c.put(req, net.clone()));
        }).catch(() => {});
        return cached;
      }
      return fetch(req).then((net) => {
        if (net && net.ok) {
          caches.open(CACHE).then((c) => c.put(req, net.clone()));
        }
        return net;
      }).catch(() => new Response("Offline", { status: 503, headers: { "Content-Type": "text/plain" } }));
    })
  );
});
