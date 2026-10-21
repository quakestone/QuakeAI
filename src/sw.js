// v0.19.29 离线韧性 Service Worker（手写轻量版，不引 Workbox——本项目是
// 「单 wasm + 内嵌 HTML」架构，Workbox 运行时比这份 SW 大两个数量级）。
//
// 分层策略（严格按需求：正常缓存优先，兜底仅在「弹尽粮绝」时触发）：
//   导航请求  network-first → 成功即刷新缓存 → 失败回缓存的主 UI（**UI 不变**，
//             用户看到还是那个聊天界面，历史会话从 IndexedDB 照常渲染）
//             → 主 UI 都没缓存（首访且离线）才回 offline.html 极简兜底页
//   静态资源  stale-while-revalidate（先给缓存，后台刷新）
//   API 请求  一律直通网络（/v1、/admin/api、/health、/cdn-cgi 不缓存——
//             业务数据必须实时，缓存会产生「假成功」）
//
// 版本：__BUILD_VERSION__ 由网关返回时替换（与 admin_ui.html 同机制），
// 每次发版缓存名自动翻新，activate 清旧桶。

const CACHE = "gw-shell-" + "__BUILD_VERSION__";
const OFFLINE_URL = "/offline.html";
const SHELL = ["/", OFFLINE_URL];

// 需要实时性、绝不缓存的路径前缀
const BYPASS = ["/v1/", "/admin/api/", "/admin/", "/cdn-cgi/"];
function bypass(pathname) {
  if (pathname === "/health") return true;
  return BYPASS.some((p) => pathname.startsWith(p));
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      .catch(() => {})              // 预缓存失败不阻塞安装（首次可能本来就没网）
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (bypass(url.pathname)) return;              // API 直通网络

  // ① 导航请求：network-first，失败回缓存主 UI，再失败才 offline.html
  if (req.mode === "navigate") {
    event.respondWith((async () => {
      try {
        const fresh = await fetch(req);
        if (fresh && fresh.ok) {
          const c = await caches.open(CACHE);
          c.put("/", fresh.clone());             // 用最新主 UI 刷新壳缓存
        }
        return fresh;
      } catch {
        return (await caches.match("/")) || (await caches.match(OFFLINE_URL))
          || new Response("离线：网络与缓存都不可用", { status: 503, headers: { "Content-Type": "text/plain; charset=utf-8" } });
      }
    })());
    return;
  }

  // ② 静态资源：stale-while-revalidate（缓存先给，后台静默刷新）
  event.respondWith((async () => {
    const cached = await caches.match(req);
    const network = fetch(req).then((r) => {
      if (r && r.ok) caches.open(CACHE).then((c) => c.put(req, r.clone()));
      return r;
    }).catch(() => null);
    return cached || (await network) || Response.error();
  })());
});
