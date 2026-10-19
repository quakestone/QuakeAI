//! 管理面：资源（渠道）登记/更新/删除/探测 + 前端页面 + 事件账本查询。
//!
//! 路由（主线侧）：
//!   GET    /admin                     管理页 UI（静态 HTML）
//!   GET    /admin/api/channels        渠道列表（api_key 只回掩码）
//!   POST   /admin/api/channels        登记新资源
//!   PUT    /admin/api/channels/{id}   更新（含 API Key 轮换：空值=不改变）
//!   DELETE /admin/api/channels/{id}   删除
//!   POST   /admin/api/channels/{id}/test  连通性探测 + 回写健康状态
//!   POST   /admin/api/seed            一键载入国内默认（智谱/硅基流动/智谱音频）
//!   GET    /admin/api/events          降权/切换事件账本（最近 50 条）
//!   GET    /admin/api/diag            部署自检（DO 绑定/env 状态，排障用）
//!
//! 安全：配置 ADMIN_TOKEN（var 或 secret）后，所有 /admin/api/* 都要求
//! header `x-admin-token` 匹配；未配置则只面向本地/内网使用（页面有提示）。

use worker::*;
use wasm_bindgen::prelude::*;
use crate::channels::*;
use crate::json_error;
use crate::json_response;
use crate::store::*;

const VERSION: &str = "0.7.0";

/// 资源账本不可用（Durable Object 绑定缺失/类未注册/storage 异常）。
/// 以前这里直接 `?` 抛出，用户只看到裸的 500，完全不知道要绑 DO。
fn store_unavailable(e: &worker::Error) -> Result<Response> {
    json_error(
        503,
        "store_unavailable",
        &format!(
            "资源账本不可用：{e:?}。请确认 Worker 已绑定 Durable Object —— 变量名 RESOURCE_STORE、类名 ResourceStore（Dashboard: Settings → Bindings → Add → Durable Object；wrangler: 见 wrangler.toml）。"
        ),
    )
}

/// 判断一个 api_key 值是不是「掩码占位」而非真实 Key。
fn looks_masked(s: &str) -> bool {
    s.contains("***") || s.contains("...") || s.contains('…')
}

pub fn now_iso() -> String {
    js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_default()
}

pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

// ---------------------------------------------------------------------------
// 入口分发
// ---------------------------------------------------------------------------

/// `sub` 是去掉 "/admin" 前缀后的路径（可能为空）。
///
/// 兜底：内部任何未预期错误都不再裸奔成 500 纯文本——统一转成带错误详情的
/// JSON（排查 500 的第一现场：DO 绑定缺失、存储异常都会在这里现形）。
pub async fn handle_admin(req: Request, env: &Env, sub: &str) -> Result<Response> {
    match handle_admin_inner(req, env, sub).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            worker::console_error!("admin internal error: {e:?}");
            json_error(
                500,
                "internal_error",
                &format!(
                    "管理面内部错误：{e:?}。若与资源账本有关，请确认 Worker 已绑定 Durable Object（变量名 RESOURCE_STORE / 类名 ResourceStore），或打开 /admin/api/diag 自检。"
                ),
            )
        }
    }
}

async fn handle_admin_inner(req: Request, env: &Env, sub: &str) -> Result<Response> {
    // 管理页本体：不强制 token（token 只保护 API），方便本地直接打开
    if req.method() == Method::Get && (sub.is_empty() || sub == "/") {
        return html_response(include_str!("admin_ui.html"));
    }
    if sub == "/health" {
        return json_response(
            200,
            serde_json::json!({ "status": "ok", "admin": true, "gateway": "zhipu-wasm-gateway", "version": VERSION }),
        );
    }
    if !sub.starts_with("/api/") {
        return json_error(404, "not_found", "unknown admin route");
    }

    // ---- API token 校验 ----
    if let Ok(expected) = env.var("ADMIN_TOKEN") {
        let expected = expected.to_string();
        let supplied = req
            .headers()
            .get("x-admin-token")
            .ok()
            .flatten()
            .unwrap_or_default();
        if supplied != expected {
            return json_error(401, "unauthorized", "ADMIN_TOKEN is set; send x-admin-token header");
        }
    }

    let api = &sub["/api".len()..];
    let method = req.method();
    match (&method, api) {
        (Method::Get, "/channels") => api_list(env).await,
        (Method::Post, "/channels") => api_create(req, env).await,
        (Method::Post, "/seed") => api_seed(env).await,
        (Method::Get, "/events") => api_events(env).await,
        (Method::Get, "/diag") => api_diag(env).await,
        _ => {
            // /channels/{id} 与 /channels/{id}/test
            if let Some(rest) = api.strip_prefix("/channels/") {
                if let Some(id) = rest.strip_suffix("/test") {
                    if method == Method::Post {
                        return api_test(env, id).await;
                    }
                } else if method == Method::Put {
                    return api_update(req, env, rest).await;
                } else if method == Method::Delete {
                    return api_delete(env, rest).await;
                }
            }
            json_error(404, "not_found", "unknown admin api route")
        }
    }
}

// ---------------------------------------------------------------------------
// API 实现
// ---------------------------------------------------------------------------

fn channel_summary(list: &[Channel], events: &[RouteEvent]) -> serde_json::Value {
    let now = now_ms();
    let enabled = list.iter().filter(|c| c.enabled).count();
    let healthy = list
        .iter()
        .filter(|c| c.enabled && c.health.status == "healthy")
        .count();
    let cooling = list
        .iter()
        .filter(|c| c.enabled && c.cooling(now))
        .count();
    let pending_key = list
        .iter()
        .filter(|c| c.enabled && c.api_key.is_empty())
        .count();
    serde_json::json!({
        "total": list.len(),
        "enabled": enabled,
        "healthy": healthy,
        "cooling": cooling,
        "pending_key": pending_key,
        "events_24h": events.len(),
    })
}

async fn api_list(env: &Env) -> Result<Response> {
    // 关键：不要用 unwrap_or_default 吞掉账本错误，否则前端看到的是
    // 「一切正常但列表为空」，而保存时才炸成 500（真实踩过的坑）。
    let (list, store_state, store_error) = match load_all_channels(env).await {
        Ok(l) => (l, "durable-object", String::new()),
        Err(e) => (Vec::new(), "unavailable", format!("{e:?}")),
    };
    let events = load_events(env).await;
    json_response(
        200,
        serde_json::json!({
            "channels": masked_channels(&list),
            "summary": channel_summary(&list, &events),
            "presets": PRESETS.iter().map(|p| serde_json::json!({
                "provider": p.provider,
                "name": p.name,
                "base_url": p.base_url,
                "models": p.models,
                "tier": p.tier,
                "kind": p.kind,
                "region": p.region,
                "free_tier": p.free_tier,
                "doc": p.doc,
            })).collect::<Vec<_>>(),
            "store": store_state,
            "store_error": store_error,
            "store_binding": { "variable": "RESOURCE_STORE", "class": "ResourceStore" },
            "version": VERSION,
        }),
    )
}

/// 登记表单校验 + 落库。允许同 provider 多 key（多个渠道指向同一 base_url）。
async fn api_create(mut req: Request, env: &Env) -> Result<Response> {
    // 空 body 视为 {}（走缺字段校验），不让 read 失败把整请求打成裸 500
    let body = req.text().await.unwrap_or_default();
    let v: serde_json::Value = if body.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return json_error(400, "bad_json", &format!("请求体不是合法 JSON：{e}")),
        }
    };

    let provider = v.get("provider").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let base_url = v.get("base_url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("chat").trim().to_string();
    if provider.is_empty() {
        return json_error(400, "missing_provider", "field 'provider' is required");
    }
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return json_error(400, "bad_base_url", "base_url must start with http:// or https://");
    }
    if kind != "chat" && kind != "audio" {
        return json_error(400, "bad_kind", "kind must be chat or audio");
    }

    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(e) => return store_unavailable(&e),
    };
    let now = now_iso();
    // 新登记的 Key 必须是真实值；掩码（xxx***xxxx / xxx...yyyy / aaa…bbb）一律拦下，
    // 否则会把列表里复制来的掩码当真 Key 存进账本，上游 401 都不知道为什么。
    let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if looks_masked(&api_key) {
        return json_error(
            400,
            "masked_key",
            "api_key 看起来是掩码（如 5bc8...4bRY / 5bc***RY），请粘贴完整 Key；暂不登记请留空。",
        );
    }

    // id：用户指定或 provider-kind-序号
    let want_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let id = if want_id.is_empty() {
        let mut n = 1;
        loop {
            let cand = format!("{provider}-{kind}-{n}");
            if !list.iter().any(|c| c.id == cand) {
                break cand;
            }
            n += 1;
        }
    } else {
        if list.iter().any(|c| c.id == want_id) {
            return json_error(409, "duplicate_id", "channel id already exists");
        }
        want_id
    };

    let models: Vec<String> = v
        .get("models")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // 结构化模型清单（可选）：缺省时由 models + kind 推导（见 effective_models）
    let models_meta: Vec<crate::channels::ModelMeta> = v
        .get("models_meta")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let ch = Channel {
        id: id.clone(),
        provider,
        name: v.get("name").and_then(|x| x.as_str()).unwrap_or(&id).to_string(),
        kind,
        base_url,
        api_key: v.get("api_key").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
        models,
        tier: v.get("tier").and_then(|x| x.as_str()).unwrap_or("T1").to_string(),
        priority: v.get("priority").and_then(|x| x.as_i64()).unwrap_or(50),
        enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
        note: v.get("note").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        caps: v
            .get("caps")
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        models_meta,
        health: Health::default(),
        created_at: now.clone(),
        updated_at: now,
    };
    list.push(ch.clone());
    if let Err(e) = save_all_channels(env, &list).await {
        return store_unavailable(&e);
    }

    json_response(
        201,
        serde_json::json!({ "channel": masked_channels(&[ch]).pop().unwrap_or(serde_json::Value::Null) }),
    )
}

/// 更新：按 JSON 字段覆盖；`api_key` 缺省/空/掩码占位 → 保持原 Key 不变（轮换语义）。
async fn api_update(mut req: Request, env: &Env, id: &str) -> Result<Response> {
    let body = req.text().await.unwrap_or_default();
    let v: serde_json::Value = if body.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return json_error(400, "bad_json", &format!("请求体不是合法 JSON：{e}")),
        }
    };

    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(e) => return store_unavailable(&e),
    };
    let pos = match list.iter().position(|c| c.id == id) {
        Some(p) => p,
        None => return json_error(404, "channel_not_found", "no such channel"),
    };
    let mut ch = list[pos].clone();

    if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
        ch.name = s.to_string();
    }
    if let Some(s) = v.get("base_url").and_then(|x| x.as_str()) {
        let s = s.trim();
        if !s.starts_with("http://") && !s.starts_with("https://") {
            return json_error(400, "bad_base_url", "base_url must start with http(s)://");
        }
        ch.base_url = s.to_string();
    }
    if let Some(k) = v.get("kind").and_then(|x| x.as_str()) {
        if k != "chat" && k != "audio" {
            return json_error(400, "bad_kind", "kind must be chat or audio");
        }
        ch.kind = k.to_string();
    }
    if let Some(s) = v.get("api_key").and_then(|x| x.as_str()) {
        let s = s.trim();
        // 前端回传的掩码/空值都视为“不修改 Key”；只有真正的新明文才轮换
        if !s.is_empty() && !looks_masked(s) {
            ch.api_key = s.to_string();
        }
    }
    if let Some(arr) = v.get("models").and_then(|x| x.as_array()) {
        ch.models = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    // 结构化模型清单：显式传入则整体覆盖（管理页模型行的保存入口）
    if let Some(mm) = v.get("models_meta") {
        if let Ok(parsed) = serde_json::from_value::<Vec<crate::channels::ModelMeta>>(mm.clone()) {
            ch.models_meta = parsed;
        }
    }
    if let Some(s) = v.get("tier").and_then(|x| x.as_str()) {
        ch.tier = s.to_string();
    }
    if let Some(i) = v.get("priority").and_then(|x| x.as_i64()) {
        ch.priority = i;
    }
    if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
        ch.enabled = b;
        if b {
            // 重新启用时清除冷却，给一次立即恢复的机会
            ch.health.cooldown_until_ms = None;
            if ch.health.status == "down" {
                ch.health.status = "unknown".into();
            }
        }
    }
    if let Some(s) = v.get("note").and_then(|x| x.as_str()) {
        ch.note = s.to_string();
    }
    ch.updated_at = now_iso();
    list[pos] = ch.clone();
    // 以前这里是裸 `?`：DO 不可用时用户只看到一个没有解释的 500。
    // 与登记/删除/seed 保持一致：账本问题一律 503 + 绑定指引。
    if let Err(e) = save_all_channels(env, &list).await {
        return store_unavailable(&e);
    }

    json_response(
        200,
        serde_json::json!({ "channel": masked_channels(&[ch]).pop().unwrap_or(serde_json::Value::Null) }),
    )
}

async fn api_delete(env: &Env, id: &str) -> Result<Response> {
    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(e) => return store_unavailable(&e),
    };
    let before = list.len();
    list.retain(|c| c.id != id);
    if list.len() == before {
        return json_error(404, "channel_not_found", "no such channel");
    }
    if let Err(e) = save_all_channels(env, &list).await {
        return store_unavailable(&e);
    }
    json_response(200, serde_json::json!({ "ok": true, "deleted": id }))
}

async fn api_seed(env: &Env) -> Result<Response> {
    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(e) => return store_unavailable(&e),
    };
    let now = now_iso();
    let mut added = 0;
    for seed in default_cn_channels(&now) {
        if !list.iter().any(|c| c.id == seed.id) {
            list.push(seed);
            added += 1;
        }
    }
    if let Err(e) = save_all_channels(env, &list).await {
        return store_unavailable(&e);
    }
    json_response(
        200,
        serde_json::json!({
            "ok": true,
            "added": added,
            "channels": masked_channels(&list),
        }),
    )
}

async fn api_events(env: &Env) -> Result<Response> {
    let events = load_events(env).await;
    json_response(200, serde_json::json!({ "events": events }))
}

/// 部署自检：前端排障页与运维脚本都能用。一次请求说清
///「DO 绑没绑上、env 里有什么、绑定该是什么名字」。
async fn api_diag(env: &Env) -> Result<Response> {
    let store = match load_all_channels(env).await {
        Ok(list) => serde_json::json!({ "ok": true, "channels": list.len() }),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:?}") }),
    };
    json_response(
        200,
        serde_json::json!({
            "version": VERSION,
            "store": store,
            "expected_binding": { "variable": "RESOURCE_STORE", "class": "ResourceStore" },
            "env": {
                "has_zhipu_api_key": env.var("ZHIPU_API_KEY").map(|k| !k.to_string().is_empty()).unwrap_or(false),
                "has_admin_token": env.var("ADMIN_TOKEN").is_ok(),
                "zhipu_base_url": env.var("ZHIPU_BASE_URL").map(|v| v.to_string()).unwrap_or_default(),
            }
        }),
    )
}

// ---------------------------------------------------------------------------
// 连通性探测
// ---------------------------------------------------------------------------

/// 读取上游错误体前 200 字符（探测/排障用）。
async fn body_snippet(resp: &web_sys::Response) -> String {
    match resp.text() {
        Ok(p) => wasm_bindgen_futures::JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        Err(_) => String::new(),
    }
}

/// 探测一个渠道：
///   - chat 渠道：POST {base}/chat/completions {model, max_tokens:1, stream:false}
///   - audio 渠道：GET {base}/models（上游支持列表时）
/// 判定：2xx=healthy；401/403=链路通但 Key 无效；429=限流（60s 冷却）；其余=失败。
/// 结果回写到渠道 health，并返回探测报告。
async fn api_test(env: &Env, id: &str) -> Result<Response> {
    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(e) => return store_unavailable(&e),
    };
    let pos = match list.iter().position(|c| c.id == id) {
        Some(p) => p,
        None => return json_error(404, "channel_not_found", "no such channel"),
    };
    let ch = list[pos].clone();

    let (status, latency, note) = probe_channel(&ch).await;

    // ---- 回写健康状态 ----
    let mut health = ch.health.clone();
    health.latency_ms = Some(latency);
    health.last_check = Some(now_iso());
    health.last_error = if note.is_empty() { None } else { Some(note.clone()) };
    if (200..300).contains(&status) {
        health.status = "healthy".into();
        health.cooldown_until_ms = None;
    } else if status == 401 || status == 403 {
        health.status = "degraded".into();
    } else if status == 429 {
        health.status = "degraded".into();
        health.cooldown_until_ms = Some(now_ms() + crate::COOLDOWN_MS);
    } else {
        health.status = "down".into();
    }
    list[pos].health = health;
    if let Err(e) = save_all_channels(env, &list).await {
        return store_unavailable(&e);
    }

    let verdict = if (200..300).contains(&status) {
        "healthy"
    } else if status == 401 || status == 403 {
        "key-invalid-or-unauthorized (channel reachable)"
    } else if status == 429 {
        "rate-limited (60s cooldown applied)"
    } else {
        "failed"
    };

    json_response(
        200,
        serde_json::json!({
            "id": id,
            "upstream_status": status,
            "latency_ms": latency,
            "verdict": verdict,
            "note": note,
        }),
    )
}

/// 实际探测请求（最小代价）：chat 用 1-token 对话；audio 用 GET /models。
async fn probe_channel(ch: &Channel) -> (u16, u64, String) {
    let t0 = now_ms();
    if ch.kind == "audio" {
        // GET {base}/models
        let url = ch.url("/models");
        let init = web_sys::RequestInit::new();
        init.set_method("GET");
        let web_req = match web_sys::Request::new_with_str_and_init(&url, &init) {
            Ok(r) => r,
            Err(e) => return (0, (now_ms() - t0) as u64, format!("build request: {e:?}")),
        };
        if !ch.api_key.is_empty() {
            let _ = web_req.headers().set("Authorization", &format!("Bearer {}", crate::bearer_token(ch)));
        }
        let global = js_sys::global().unchecked_into::<web_sys::WorkerGlobalScope>();
        let resp = match wasm_bindgen_futures::JsFuture::from(global.fetch_with_request(&web_req)).await {
            Ok(r) => match r.dyn_into::<web_sys::Response>() {
                Ok(r) => r,
                Err(_) => return (0, (now_ms() - t0) as u64, "cast failed".into()),
            },
            Err(e) => return (0, (now_ms() - t0) as u64, format!("fetch failed: {e:?}")),
        };
        let status = resp.status();
        let note = if (200..300).contains(&status) {
            String::new()
        } else {
            body_snippet(&resp).await
        };
        return (status, (now_ms() - t0) as u64, note);
    }

    // chat：最小请求
    let model = ch.models.first().cloned().unwrap_or_else(|| "default".to_string());
    let payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "stream": false,
    });
    let url = ch.url("/chat/completions");
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&payload.to_string()));
    let web_req = match web_sys::Request::new_with_str_and_init(&url, &init) {
        Ok(r) => r,
        Err(e) => return (0, (now_ms() - t0) as u64, format!("build request: {e:?}")),
    };
    let _ = web_req.headers().set("Content-Type", "application/json");
    if !ch.api_key.is_empty() {
        let _ = web_req.headers().set("Authorization", &format!("Bearer {}", crate::bearer_token(ch)));
    }
    let global = js_sys::global().unchecked_into::<web_sys::WorkerGlobalScope>();
    let resp = match wasm_bindgen_futures::JsFuture::from(global.fetch_with_request(&web_req)).await {
        Ok(r) => match r.dyn_into::<web_sys::Response>() {
            Ok(r) => r,
            Err(_) => return (0, (now_ms() - t0) as u64, "cast failed".into()),
        },
        Err(e) => return (0, (now_ms() - t0) as u64, format!("fetch failed: {e:?}")),
    };
    let status = resp.status();
    let note = if (200..300).contains(&status) {
        String::new()
    } else {
        body_snippet(&resp).await
    };
    (status, (now_ms() - t0) as u64, note)
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn html_response(body: &str) -> Result<Response> {
    let headers = web_sys::Headers::new()
        .map_err(|e| Error::RustError(format!("headers: {e:?}")))?;
    headers
        .set("Content-Type", "text/html; charset=utf-8")
        .map_err(|e| Error::RustError(format!("set header: {e:?}")))?;
    let init = web_sys::ResponseInit::new();
    init.set_status(200);
    init.set_headers(&headers);
    web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
        .map(Response::from)
        .map_err(|e| Error::RustError(format!("build response: {e:?}")))
}

/// 供 lib.rs 在 chat/audio 路由里更新渠道健康状态 + 记录事件。
pub async fn report_success(env: &Env, id: &str, provider: &str, scope: &str, model: &str, attempts: &[String], latency_ms: u64) {
    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Some(pos) = list.iter().position(|c| c.id == id) {
        let ch = &mut list[pos];
        ch.health.status = "healthy".into();
        ch.health.latency_ms = Some(latency_ms);
        ch.health.last_check = Some(now_iso());
        ch.health.last_error = None;
        if ch.health.cooldown_until_ms.is_some() {
            ch.health.cooldown_until_ms = None;
        }
        // 模型级：命中的模型恢复健康（失败计数清零、冷却解除）
        ensure_models_meta(ch);
        if let Some(m) = ch.models_meta.iter_mut().find(|m| m.name == model) {
            m.health.status = "healthy".into();
            m.health.latency_ms = Some(latency_ms);
            m.health.last_check = Some(now_iso());
            m.health.last_error = None;
            m.health.fail_count = 0;
            m.health.cooldown_until_ms = None;
        }
        let _ = save_all_channels(env, &list).await;
    }
    push_event(
        env,
        &RouteEvent {
            ts: now_iso(),
            scope: scope.into(),
            model: model.into(),
            attempts: attempts.to_vec(),
            channel: id.into(),
            provider: provider.into(),
            result: if attempts.len() > 1 { "degraded" } else { "ok" }.into(),
            message: format!("latency {latency_ms}ms via {provider}"),
        },
    )
    .await;
}

/// 429/限流降权：写入冷却窗口（60s），后续请求跳过该渠道。
/// 同时写模型级健康：该模型连续失败计数 +1，429 时按失败次数加长冷却
/// （60s × min(fail_count,5)）——模型级自动更换的感知基础。
pub async fn report_degraded(env: &Env, id: &str, provider: &str, scope: &str, model: &str, attempts: &[String], status: u16) {
    let mut list = match load_all_channels(env).await {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Some(pos) = list.iter().position(|c| c.id == id) {
        let ch = &mut list[pos];
        ch.health.status = "degraded".into();
        ch.health.last_check = Some(now_iso());
        let msg = format!("HTTP {status}");
        ch.health.last_error = Some(msg.clone());
        ch.health.fail_count += 1;
        if status == 429 {
            let times = ch.health.fail_count.min(5).max(1);
            ch.health.cooldown_until_ms = Some(now_ms() + crate::COOLDOWN_MS * times as f64);
        }
        // 模型级：把推导结果固化为 models_meta（旧账本首次失败时落地），
        // 之后只冷却这个模型，同渠道其他模型不受影响。
        ensure_models_meta(ch);
        if let Some(m) = ch.models_meta.iter_mut().find(|m| m.name == model) {
            m.health.status = "degraded".into();
            m.health.last_check = Some(now_iso());
            m.health.last_error = Some(msg);
            m.health.fail_count += 1;
            if status == 429 {
                let times = m.health.fail_count.min(5).max(1);
                m.health.cooldown_until_ms = Some(now_ms() + crate::COOLDOWN_MS * times as f64);
            }
        }
        let _ = save_all_channels(env, &list).await;
    }
    push_event(
        env,
        &RouteEvent {
            ts: now_iso(),
            scope: scope.into(),
            model: model.into(),
            attempts: attempts.to_vec(),
            channel: id.into(),
            provider: provider.into(),
            result: "failed".into(),
            message: format!("HTTP {status} on {provider}"),
        },
    )
    .await;
}

/// 旧账本（models_meta 为空）首次写健康前，把 effective_models 的推导结果
/// 固化成 models_meta，让模型级状态有落点。
fn ensure_models_meta(ch: &mut Channel) {
    if ch.models_meta.is_empty() {
        ch.models_meta = ch.effective_models();
    }
}
