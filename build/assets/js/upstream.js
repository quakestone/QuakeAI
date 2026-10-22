/* ============================================================
   QuakeAI upstream.js —— 上游与接入（B4：F25/F26/F27/F19/F28）
   纯静态部署（脱钩 Cloudflare）后的一站式「怎么连上游」：
   - F25 上游转发代理：localStorage gw_upstream_proxy。浏览器直连智谱等上游会被
     CORS 拦截，用户自建转发服务后，网关把真实上游地址放 x-gw-target 头转交
     （lib.rs fetch_channel 直读该键，UI 只负责填写与状态展示）。
   - F26 接入方式：OpenAI 兼容端点 + curl 示例 + 观测响应头（只读文档）。
   - F27 管理面鉴权：localStorage gw_admin_token → x-admin-token 本地软锁
     （网关侧 admin.rs 强制校验：配了就必须带头，否则 401）。
   - F19 本设备 Key：localStorage gw_device_keys（JSON，按 provider）→
     x-device-keys 头；仅当渠道未登记共享 Key 时生效，不写共享账本。
   - F28 部署自检：GET /diag JSON 展示（build.sh 同源，纯静态下即本地 WASM 态）。
   持久化全部走 Store.config（gw_* 键名与旧 UI 一致，旧数据无缝读取）。
   ============================================================ */
(function (global) {
  "use strict";

  const { config, toast } = { config: global.QuakeStore.config, toast: global.QuakeComponents.toast };
  const $ = (s) => document.querySelector(s);
  const escapeHTML = global.QuakeComponents.escapeHTML;

  /* 掩码识别（与 providers.js 同口径：含 *** / ... 一律视为掩码） */
  const MASK_RE = /(\*{2,}|\.{3}|…)/;
  function looksMaskedKey(k) { return MASK_RE.test(k || ""); }
  function maskKey(k) {
    k = String(k || "");
    return k.length > 8 ? k.slice(0, 3) + "***" + k.slice(-4) : (k ? "***" : "");
  }

  /* ---------- 本设备 Key：JSON {provider: key} ---------- */
  function deviceKeys() { return config.getJSON("gw_device_keys", {}) || {}; }
  function setDeviceKey(provider, value) {
    const m = deviceKeys();
    const p = String(provider || "").trim();
    if (!p) return;
    const v = String(value || "").trim();
    if (v) m[p] = v; else delete m[p];
    config.setJSON("gw_device_keys", m);
  }

  /* ---------- F25 代理 ---------- */
  function proxyUrl() { return config.get("gw_upstream_proxy", ""); }
  function saveProxy() {
    const v = ($("#upProxy").value || "").trim();
    if (v && !/^https?:\/\//i.test(v)) {
      $("#upProxyHint").textContent = "需要以 http:// 或 https:// 开头";
      return;
    }
    if (v) config.set("gw_upstream_proxy", v); else config.remove("gw_upstream_proxy");
    $("#upProxyHint").textContent = v ? "已保存 · 网关转发时将带 x-gw-target 头" : "已清空 · 恢复直连（可能被 CORS 拦截）";
    toast(v ? "上游转发地址已保存" : "已清空转发地址（直连模式）", "ok");
    renderProxyState();
  }
  function renderProxyState() {
    const v = proxyUrl();
    const el = $("#upProxyState");
    if (el) {
      el.innerHTML = v
        ? '<i class="mini-dot green-dot"></i> 已配置：<code>' + escapeHTML(v) + "</code>"
        : '<i class="mini-dot gray-dot"></i> 未配置 —— 浏览器直连上游将被 CORS 拦截，对话不可用';
    }
    const inp = $("#upProxy");
    if (inp && inp.value !== v) inp.value = v;
  }

  /* ---------- F27 管理面鉴权 ---------- */
  function adminToken() { return config.get("gw_admin_token", ""); }
  function saveToken() {
    const v = ($("#upToken").value || "").trim();
    if (v) config.set("gw_admin_token", v); else config.remove("gw_admin_token");
    $("#upTokenHint").textContent = v ? "已设置 · /admin/api/* 请求将带 x-admin-token" : "已清空 = 不鉴权";
    toast(v ? "管理口令已保存" : "已清空管理口令", "ok");
    renderTokenState();
  }
  function renderTokenState() {
    const v = adminToken();
    const el = $("#upTokenState");
    if (el) {
      el.innerHTML = v
        ? '<i class="mini-dot green-dot"></i> 已设置（' + escapeHTML(maskKey(v)) + "）"
        : '<i class="mini-dot gray-dot"></i> 未设置 = 不鉴权（仅建议本机/内网使用）';
    }
    const inp = $("#upToken");
    if (inp && document.activeElement !== inp) inp.value = v;
  }

  /* ---------- F19 本设备 Key 行 ---------- */
  function renderDeviceKeys() {
    const el = $("#upDevKeyRows");
    if (!el) return;
    const chs = (global.QuakeProviders && global.QuakeProviders.state.channels) || [];
    const provs = new Set([...chs.map(c => c.provider).filter(Boolean), ...Object.keys(deviceKeys())]);
    if (!provs.size) {
      el.innerHTML = '<div class="empty-state"><div class="empty-icon">✦</div><h2>暂无渠道</h2><p>去「服务商」登记渠道后，可按 provider 录入本设备自己的 Key。</p></div>';
      return;
    }
    const dk = deviceKeys();
    el.innerHTML = [...provs].sort().map(p => {
      const k = dk[p] || "";
      const ep = encodeURIComponent(p);
      return '<div class="dk-row" data-prov="' + ep + '">' +
        '<span class="dk-prov">' + escapeHTML(p) + "</span>" +
        '<span class="dk-st ' + (k ? "on" : "") + '">' + (k ? "已登记 " + escapeHTML(maskKey(k)) : "未登记") + "</span>" +
        '<input class="dk-in" id="dk-' + ep + '" type="password" placeholder="' + (k ? "输入新值保存 · 留空保存 = 清除" : "sk-…（只存本浏览器）") + '" autocomplete="off">' +
        '<button class="soft-btn" data-act="save">保存</button>' +
        '<button class="soft-btn danger" data-act="clear">清除</button>' +
        "</div>";
    }).join("");
  }
  function saveDeviceKey(prov, value) {
    const p = decodeURIComponent(prov || "");
    if (!p) return;
    const v = String(value || "").trim();
    if (v && looksMaskedKey(v)) { toast("这看起来是掩码，请粘贴完整 Key", "err"); return; }
    setDeviceKey(p, v);
    renderDeviceKeys();
    toast(v ? "本设备 Key 已保存（" + p + "）" : "已清除本设备 Key（" + p + "）", "ok");
  }

  /* ---------- F28 部署自检 ---------- */
  async function runDiag() {
    const el = $("#upDiagBody");
    if (!el) return;
    el.textContent = "自检中…";
    try {
      const j = await global.QuakeAdapters.gateway.diag();
      el.textContent = JSON.stringify(j, null, 2);
      toast("自检完成", "ok");
    } catch (e) {
      el.textContent = "自检失败：" + (e && e.message ? e.message : String(e));
      toast("自检失败：" + (e && e.message ? e.message : String(e)), "err");
    }
  }

  /* ---------- 装载（进入视图 / 渠道变更后） ---------- */
  function load() {
    renderProxyState();
    renderTokenState();
    renderDeviceKeys();
  }

  function init() {
    $("#upProxySave").onclick = saveProxy;
    $("#upTokenSave").onclick = saveToken;
    $("#upDiagRun").onclick = runDiag;
    $("#upDevKeyRows").addEventListener("click", (e) => {
      const b = e.target.closest("button[data-act]");
      if (!b) return;
      const row = b.closest(".dk-row");
      const prov = row && row.dataset.prov;
      if (b.dataset.act === "clear") { saveDeviceKey(prov, ""); return; }
      const inp = row ? row.querySelector(".dk-in") : null;
      saveDeviceKey(prov, inp ? inp.value : "");
    });
    load();
  }

  global.QuakeUpstream = {
    init, load,
    proxyUrl, adminToken, deviceKeys,
    saveProxy, saveToken, runDiag,
    /// 发送失败时的引导：代理未配 + 渠道需要转发 → 提示去「上游与接入」配代理
    hintIfProxyMissing() {
      if (!proxyUrl()) {
        toast("对话失败：未配置上游转发代理（浏览器直连会被 CORS 拦截）——去「上游与接入」填写", "err");
        return true;
      }
      return false;
    },
  };
})(window);
