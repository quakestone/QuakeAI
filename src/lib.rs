//! zhipu-wasm-gateway v0.4 —— WASM AI 聚合网关（资源管理 + 智能路由）
//!
//! 演进：
//!   v0.1  Rust→wasm32 跑 Cloudflare Workers(本地 workerd)，打通智谱 OpenAI 兼容 API
//!   v0.2  验证两个"WASM 做不到"：流式首包前重试、multipart 音视频上传
//!   v0.3  资源管理：
//!         - 前端页面 /admin：资源登记/更新/删除/探测/轮换 Key（国内预设目录）
//!         - Durable Object 单例账本：渠道 + API Key + 健康状态 + 降权事件
//!         - 路由：priority 大者优先；429/冷却中的渠道自动跳过（"先选能力强的，
//!           被降权后换一个"）；Key 只回掩码，明文不出 DO
//!         - 流式语义不变：首包前失败可重试、已吐字不切换
//!   v0.4  管理面硬化 + ChatGPT 式前端：
//!         - 登记 500 修复：api_update 裸 ? → 503 store_unavailable（含 DO 绑定指引）；
//!           handle_admin 外层兜底把未预期 Err 转 JSON 500；空 body 视为 {}
//!         - 前端重写：类 ChatGPT 布局（对话首页 + 模型管理二级子页，hash 路由），
//!           流式聊天（SSE 逐帧 + 停止按钮 + 渠道徽章），移动端抽屉/底部弹层适配
//!         - 掩码 Key 前端预检（含 ***/.../… 直接拦截，不再打后端）
//!   v0.5  智谱 JWT 自动鉴权（本版）：
//!         - provider=zhipu 且 Key 为 {id}.{secret} → 现场生成 HS256 短期 JWT
//!           （sign_type=SIGN，TTL 1h）；v4 端点对原始 Key 支持不稳定（401）
//!         - 零依赖手写 SHA-256/HMAC/base64url（src/zhipu_jwt.rs，标准向量单测）
//!         - chat / audio / 连通性探测三条路径全部走 bearer_token() 统一出口
//!
//! 路由：
//!   GET    /                         对话首页（管理 UI，hash 路由：/ 与 #/models）
//!   GET    /health                   健康检查
//!   POST   /v1/chat/completions      OpenAI 兼容对话（流式/非流式，多通道+首包前重试）
//!   POST   /v1/audio/transcriptions  multipart 音视频上传（原生解析→裸 bytes→手工重建）
//!   /admin/*                         管理面 API（见 admin.rs）

use worker::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::channels::Channel;

mod admin;
mod channels;
mod store;
mod zhipu_jwt;

const DEFAULT_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
const DEFAULT_MODEL: &str = "glm-4-flash";
const DEFAULT_AUDIO_MODEL: &str = "glm-4-voice";
/// 429 降权冷却窗口（毫秒）：与 DO 账本里的 cooldown_until_ms 对齐
pub(crate) const COOLDOWN_MS: f64 = 60_000.0;

// ---------------------------------------------------------------------------
// 绑定与通道
// ---------------------------------------------------------------------------

/// 读取绑定值：先 var（非敏感配置），再 secret（密钥），最后默认值。
fn binding(env: &Env, name: &str, default: Option<&str>) -> Option<String> {
    if let Ok(v) = env.var(name) {
        return Some(v.to_string());
    }
    if let Ok(s) = env.secret(name) {
        return Some(s.to_string());
    }
    default.map(|s| s.to_string())
}

/// 静态通道 URL 列表（ZHIPU_CHANNELS JSON 绑定），缺省单通道。
fn static_channel_urls(env: &Env, default_base: &str) -> Vec<(String, String)> {
    if let Some(raw) = binding(env, "ZHIPU_CHANNELS", None) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(arr) = v.as_array() {
                let list: Vec<(String, String)> = arr
                    .iter()
                    .filter_map(|item| {
                        Some((
                            item.get("name")?.as_str()?.to_string(),
                            item.get("url")?.as_str()?.to_string(),
                        ))
                    })
                    .collect();
                if !list.is_empty() {
                    return list;
                }
            }
        }
    }
    vec![("default".to_string(), default_base.to_string())]
}

/// 加载某类型（chat/audio）的可用渠道，按 priority 降序：
///   1. DO 账本里 enabled + 已登记 Key + 未冷却的动态渠道；
///   2. 动态不可用（DO 空/全部停用或冷却）→ 回退静态通道（ZHIPU_CHANNELS + ZHIPU_API_KEY）。
async fn load_channels(
    env: &Env,
    kind: &str,
    fallback_key: &str,
    default_base: &str,
) -> Vec<Channel> {
    let now = admin::now_ms();
    let dynamic = match store::load_all_channels(env).await {
        Ok(all) if !all.is_empty() => {
            let mut list: Vec<Channel> = all
                .into_iter()
                .filter(|c| {
                    c.kind == kind && c.enabled && !c.api_key.is_empty() && !c.cooling(now)
                })
                .collect();
            list.sort_by(|a, b| b.priority.cmp(&a.priority));
            list
        }
        _ => Vec::new(),
    };
    if dynamic.is_empty() {
        channels::static_channels(
            &static_channel_urls(env, default_base),
            kind,
            fallback_key.to_string(),
        )
    } else {
        dynamic
    }
}

/// 默认模型：优先绑定 → 账本中最高优先级 chat 渠道的首个模型 → 内置默认。
async fn resolve_default_model(env: &Env, channels: &[Channel]) -> String {
    if let Some(m) = binding(env, "ZHIPU_MODEL", None) {
        if !m.is_empty() {
            return m;
        }
    }
    channels
        .iter()
        .filter(|c| c.kind == "chat")
        .find_map(|c| c.models.first().cloned())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

// ---------------------------------------------------------------------------
// 响应工具
// ---------------------------------------------------------------------------

pub(crate) fn json_response(status: u16, value: serde_json::Value) -> Result<Response> {
    let headers = web_sys::Headers::new()
        .map_err(|e| Error::RustError(format!("headers: {e:?}")))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| Error::RustError(format!("set header: {e:?}")))?;
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    init.set_headers(&headers);
    web_sys::Response::new_with_opt_str_and_init(Some(&value.to_string()), &init)
        .map(Response::from)
        .map_err(|e| Error::RustError(format!("build response: {e:?}")))
}

pub(crate) fn json_error(status: u16, code: &str, message: &str) -> Result<Response> {
    json_response(
        status,
        serde_json::json!({
            "error": {
                "code": code,
                "message": message,
                "gateway": "zhipu-wasm-gateway",
            }
        }),
    )
}

/// 上游 Authorization 的 Bearer 值：
/// - 智谱（provider=zhipu）且 Key 为 `{id}.{secret}` 两段式 → 现场生成
///   短期 JWT（v4 端点对原始 Key 支持不稳定，会 401「令牌已过期或验证不正确」，
///   详见 zhipu_jwt.rs 模块文档）；
/// - 其余（硅基流动 / OpenRouter 等 OpenAI 兼容源）原样用 Key。
/// JWT 有效期 1 小时（与 report_degraded 的 60s 冷却无关，独立常量）。
pub(crate) fn bearer_token(ch: &Channel) -> String {
    const JWT_TTL_MS: f64 = 3_600_000.0;
    if ch.provider == "zhipu" && zhipu_jwt::looks_like_zhipu_key(&ch.api_key) {
        if let Some(jwt) = zhipu_jwt::sign_zhipu_jwt(&ch.api_key, admin::now_ms(), JWT_TTL_MS) {
            return jwt;
        }
    }
    ch.api_key.clone()
}

/// GET /health：证明 WASM Worker 本体在运行
fn handle_health() -> Result<Response> {
    json_response(
        200,
        serde_json::json!({
            "status": "ok",
            "gateway": "zhipu-wasm-gateway",
            "version": "0.5.0",
            "runtime": "cloudflare-workers-wasm",
            "features": [
                "resource-management-admin-ui",
                "durable-object-channel-ledger",
                "stream-retry-first-packet",
                "multipart-audio-video-upload",
                "cooldown-skip-after-degrade"
            ]
        }),
    )
}

// ---------------------------------------------------------------------------
// 上游请求工具
// ---------------------------------------------------------------------------

/// 向单个渠道发一次 POST，返回 (status, web_sys::Response)。
/// 连接级错误（DNS/TCP/TLS 失败）返回 Err，由调用方计入失败并继续下一渠道。
async fn fetch_channel(
    channel: &Channel,
    path: &str,
    body: &JsValue,
    extra_headers: &[(&str, String)],
) -> Result<(u16, web_sys::Response)> {
    let upstream_url = channel.url(path);
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(body);

    let web_req = web_sys::Request::new_with_str_and_init(&upstream_url, &init)
        .map_err(|e| Error::RustError(format!("build upstream request: {e:?}")))?;
    for (k, v) in extra_headers {
        web_req
            .headers()
            .set(k, v)
            .map_err(|e| Error::RustError(format!("set upstream header {k}: {e:?}")))?;
    }

    let global = js_sys::global().unchecked_into::<web_sys::WorkerGlobalScope>();
    let promise = global.fetch_with_request(&web_req);
    let resp = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| Error::RustError(format!("fetch failed: {e:?}")))?;
    let web_resp: web_sys::Response = resp
        .dyn_into()
        .map_err(|e| Error::RustError(format!("response cast: {e:?}")))?;
    let status = web_resp.status();
    Ok((status, web_resp))
}

// ---------------------------------------------------------------------------
// A. 流式重试：首帧前失败 → 换渠道；已吐字 → 不切换
// ---------------------------------------------------------------------------

/// 读上游流的下一帧：Err = Promise reject（连接中断）；(done, value)
async fn read_chunk(reader: &web_sys::ReadableStreamDefaultReader) -> Result<(bool, JsValue)> {
    let p = reader.read();
    let result = wasm_bindgen_futures::JsFuture::from(p)
        .await
        .map_err(|e| Error::RustError(format!("read failed: {e:?}")))?;
    let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let value = js_sys::Reflect::get(&result, &JsValue::from_str("value")).unwrap_or(JsValue::UNDEFINED);
    Ok((done, value))
}

/// 把「已读到的首帧 + 剩余的上游流」重新拼成一个 ReadableStream 返回给客户端。
/// 首帧之后任何失败只 close() 收尾 —— 已吐字不切换。
fn build_restream_response(
    reader: web_sys::ReadableStreamDefaultReader,
    first_value: JsValue,
    headers: &web_sys::Headers,
) -> Result<Response> {
    let underlying = web_sys::UnderlyingSource::new();

    let reader2 = reader;
    let fv = first_value;
    let start = wasm_bindgen::closure::Closure::once(move |controller: JsValue| {
        // 控制器即 ReadableStreamDefaultController，直接把首帧推给客户端
        let Ok(ctrl) = controller.dyn_into::<web_sys::ReadableStreamDefaultController>() else {
            return;
        };
        if ctrl.enqueue_with_chunk(&fv).is_err() {
            let _ = ctrl.close();
            return;
        }
        // 续读剩余帧：post-launch 异步循环
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                match read_chunk(&reader2).await {
                    Ok((done, value)) => {
                        if done {
                            let _ = ctrl.close();
                            break;
                        }
                        if !value.is_undefined() && ctrl.enqueue_with_chunk(&value).is_err() {
                            let _ = ctrl.close();
                            break;
                        }
                    }
                    Err(_) => {
                        // 已吐字：绝不切换渠道，干净收尾当前流
                        let _ = ctrl.close();
                        break;
                    }
                }
            }
        });
    });
    underlying.set_start(start.as_ref().unchecked_ref::<js_sys::Function>());
    start.forget(); // JS 回调持有期间不能 drop 闭包

    let stream = web_sys::ReadableStream::new_with_underlying_source(&underlying)
        .map_err(|e| Error::RustError(format!("new ReadableStream: {e:?}")))?;
    let init = web_sys::ResponseInit::new();
    init.set_status(200);
    init.set_headers(headers);
    web_sys::Response::new_with_opt_readable_stream_and_init(Some(&stream), &init)
        .map(Response::from)
        .map_err(|e| Error::RustError(format!("build stream response: {e:?}")))
}

/// 给客户端响应统一注入诊断头（兼容旧 x-zhipu-* + 新增 x-gateway-*）。
fn diag_headers(
    channel: &Channel,
    model: &str,
    attempts: &[String],
    content_type: &str,
    stream: bool,
) -> Result<web_sys::Headers> {
    let h = web_sys::Headers::new()
        .map_err(|e| Error::RustError(format!("headers: {e:?}")))?;
    let attempt_str = attempts.join(";");
    for (k, v) in [
        ("x-gateway-version", "0.5.0"),
        ("x-gateway-channel", channel.name.as_str()),
        ("x-gateway-provider", channel.provider.as_str()),
        ("x-gateway-base-url", channel.base_url.as_str()),
        ("x-gateway-model", model),
        ("x-gateway-attempts", attempt_str.as_str()),
        ("x-zhipu-gateway", "wasm"),
        ("x-zhipu-channel", channel.name.as_str()),
        ("x-zhipu-base-url", channel.base_url.as_str()),
        ("x-zhipu-model", model),
        ("x-zhipu-attempts", attempt_str.as_str()),
    ] {
        h.set(k, v)
            .map_err(|e| Error::RustError(format!("set header {k}: {e:?}")))?;
    }
    h.set("Content-Type", content_type)
        .map_err(|e| Error::RustError(format!("set content-type: {e:?}")))?;
    if stream {
        h.set("Cache-Control", "no-cache")
            .map_err(|e| Error::RustError(format!("set cache-control: {e:?}")))?;
    }
    Ok(h)
}

/// 把一个非 2xx / 连接失败记入 DO 账本（降权信号 + 事件）。
async fn note_failure(env: &Env, ch: &Channel, status: u16, scope: &str, model: &str, attempts: &[String]) {
    // 静态回退通道没有账本条目，只记连接日志
    if ch.id.starts_with("static-") {
        return;
    }
    admin::report_degraded(env, &ch.id, &ch.provider, scope, model, attempts, status).await;
}

/// 最终失败：透传最后一个上游错误（状态码+错误体），降权信号不灭。
fn final_error_response(status: u16, err_body: String, attempts: &[String]) -> Result<Response> {
    let hdrs = worker::Headers::new();
    hdrs.set("Content-Type", "application/json")?;
    hdrs.set("x-gateway-version", "0.5.0")?;
    hdrs.set("x-zhipu-gateway", "wasm")?;
    hdrs.set("x-gateway-attempts", &attempts.join(";"))?;
    hdrs.set("x-zhipu-attempts", &attempts.join(";"))?;
    Ok(Response::from_bytes(err_body.into_bytes())?
        .with_status(status)
        .with_headers(hdrs))
}

fn all_channels_failed(attempts: &[String]) -> Result<Response> {
    json_response(
        502,
        serde_json::json!({
            "error": {
                "code": "all_channels_failed",
                "message": "all channels failed before first upstream packet",
                "gateway": "zhipu-wasm-gateway",
                "attempts": attempts,
            }
        }),
    )
}

// ---------------------------------------------------------------------------
// 渠道循环（chat / audio 共用）：首包前失败 → 换渠道；已吐字 → 不切换
// ---------------------------------------------------------------------------

/// 逐渠道尝试。返回：Ok(响应) | Err(最后一个上游错误) | Err(全部连接失败标记 None)
enum ChannelOutcome {
    /// 成功锁定（已吐字），直接回客户端
    Locked(Response),
    /// 全部失败：最后一个上游 HTTP 错误（透传）
    UpstreamError(u16, String, Vec<String>),
    /// 全部连接级失败（拿不到 HTTP 响应）
    AllConnFailed(Vec<String>),
}

async fn try_channels(
    env: &Env,
    channels: &[Channel],
    path: &str,
    body: &JsValue,
    make_headers: impl Fn(&Channel) -> Result<Vec<(String, String)>>,
    stream: bool,
    content_type_of: impl Fn(&web_sys::Response) -> String,
    scope: &str,
    model: &str,
) -> Result<ChannelOutcome> {
    let mut attempts: Vec<String> = Vec::new();
    let mut last_error: Option<(u16, String)> = None;
    let t0 = admin::now_ms();

    for ch in channels {
        let headers = make_headers(ch)?;
        let (status, web_resp) = match fetch_channel(
            ch,
            path,
            body,
            &headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect::<Vec<_>>(),
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                attempts.push(format!("{}=conn-failed", ch.name));
                worker::console_error!("channel {} conn failed: {e:?}", ch.name);
                note_failure(env, ch, 0, scope, model, &attempts).await;
                continue;
            }
        };

        // 非 2xx（429/5xx/4xx）：信号透传给上层做降权判断，这里立即换下一渠道
        if !(200..300).contains(&status) {
            attempts.push(format!("{}=HTTP{}", ch.name, status));
            let err_text = match web_resp.text() {
                Ok(p) => wasm_bindgen_futures::JsFuture::from(p)
                    .await
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };
            note_failure(env, ch, status, scope, model, &attempts).await;
            last_error = Some((status, err_text));
            continue;
        }

        let content_type = content_type_of(&web_resp);
        let out_headers = diag_headers(ch, model, &attempts, &content_type, stream)?;

        // 读首帧，失败（连接中断/空流）才算"首包前失败"，换渠道
        let body_stream = match web_resp.body() {
            Some(s) => s,
            None => {
                attempts.push(format!("{}=empty-body", ch.name));
                continue;
            }
        };
        let reader: web_sys::ReadableStreamDefaultReader = body_stream
            .get_reader()
            .dyn_into()
            .map_err(|e| Error::RustError(format!("get_reader: {e:?}")))?;

        match read_chunk(&reader).await {
            Ok((done, value)) if done || value.is_undefined() => {
                attempts.push(format!("{}=empty-stream", ch.name));
                continue;
            }
            Ok((_, value)) => {
                attempts.push(format!("{}=ok", ch.name));
                let resp = build_restream_response(reader, value, &out_headers)?;
                // 账本回写：命中 + 延迟（动态渠道）
                if !ch.id.starts_with("static-") {
                    let latency = (admin::now_ms() - t0) as u64;
                    admin::report_success(env, &ch.id, &ch.provider, scope, model, &attempts, latency)
                        .await;
                }
                return Ok(ChannelOutcome::Locked(resp));
            }
            Err(_) => {
                attempts.push(format!("{}=conn-reset-before-first", ch.name));
                continue;
            }
        }
    }

    if let Some((status, err_body)) = last_error {
        Ok(ChannelOutcome::UpstreamError(status, err_body, attempts))
    } else {
        Ok(ChannelOutcome::AllConnFailed(attempts))
    }
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions
// ---------------------------------------------------------------------------

async fn handle_chat_completions(mut req: Request, env: &Env) -> Result<Response> {
    let fallback_key = binding(env, "ZHIPU_API_KEY", None).unwrap_or_default();
    let default_base = binding(env, "ZHIPU_BASE_URL", Some(DEFAULT_BASE_URL))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    // ---- 解析客户端请求体 ----
    let body_text = req.text().await?;
    let mut payload: serde_json::Value = if body_text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&body_text)
            .map_err(|e| Error::RustError(format!("invalid JSON body: {e}")))?
    };

    let stream = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // ---- 渠道（账本动态优先，回退静态）----
    let channels = load_channels(env, "chat", &fallback_key, &default_base).await;
    let default_model = resolve_default_model(env, &channels).await;

    let model_missing = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    let model = if model_missing {
        payload["model"] = serde_json::Value::String(default_model.clone());
        default_model
    } else {
        payload["model"].as_str().unwrap().to_string()
    };

    let upstream_body = serde_json::to_string(&payload)
        .map_err(|e| Error::RustError(format!("failed to serialize body: {e}")))?;
    let body_js = JsValue::from_str(&upstream_body);

    let outcome = try_channels(
        env,
        &channels,
        "/chat/completions",
        &body_js,
        |ch| {
            let mut headers = vec![
                ("Authorization".to_string(), format!("Bearer {}", bearer_token(ch))),
                ("Content-Type".to_string(), "application/json".to_string()),
            ];
            if stream {
                headers.push(("Accept".to_string(), "text/event-stream".to_string()));
            }
            Ok(headers)
        },
        stream,
        |resp| {
            if stream {
                "text/event-stream".to_string()
            } else {
                resp.headers()
                    .get("content-type")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "application/json".to_string())
            }
        },
        "chat",
        &model,
    )
    .await?;

    match outcome {
        ChannelOutcome::Locked(resp) => Ok(resp),
        ChannelOutcome::UpstreamError(status, err_body, attempts) => {
            final_error_response(status, err_body, &attempts)
        }
        ChannelOutcome::AllConnFailed(attempts) => all_channels_failed(&attempts),
    }
}

// ---------------------------------------------------------------------------
// B. multipart 音视频上传：解 multipart → 裸 bytes → 手工重建 multipart 转发
// ---------------------------------------------------------------------------

const MULTIPART_BOUNDARY: &str = "ZhipuWasmGatewayBoundary7f3A9c";

/// 手工拼装 multipart/form-data 字节体（不依赖 FormData 序列化，任何平台稳成）
fn build_multipart_body(file_field: (&str, &str), text_fields: &[(&str, String)], bytes: &[u8]) -> Vec<u8> {
    let (field, filename) = file_field;
    let mut body: Vec<u8> = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    for (k, v) in text_fields {
        body.extend_from_slice(
            format!(
                "\r\n--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    body
}

/// POST /v1/audio/transcriptions（同路径亦适用于视频等任意二进制上传）
async fn handle_audio_transcriptions(mut req: Request, env: &Env) -> Result<Response> {
    let fallback_key = binding(env, "ZHIPU_API_KEY", None).unwrap_or_default();
    let default_base = binding(env, "ZHIPU_BASE_URL", Some(DEFAULT_BASE_URL))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    // ---- 1. 解析 inbound multipart ----
    let fd = req
        .form_data()
        .await
        .map_err(|e| Error::RustError(format!("form_data parse failed: {e:?}")))?;
    let file = match fd.get("file") {
        Some(worker::FormEntry::File(f)) => f,
        Some(worker::FormEntry::Field(_)) => {
            return json_error(400, "bad_file_field", "field 'file' is a text field, expected a file part")
        }
        None => return json_error(400, "missing_file", "multipart field 'file' is required"),
    };
    let filename = file.name();
    let model = fd
        .get_field("model")
        .or_else(|| binding(env, "ZHIPU_AUDIO_MODEL", None))
        .unwrap_or_else(|| DEFAULT_AUDIO_MODEL.to_string());

    // ---- 2. 裸字节读取（File::bytes：Blob.arrayBuffer -> Vec<u8>） ----
    let bytes = file.bytes().await?;

    // ---- 3. 重建 multipart 字节体 ----
    let mut text_fields: Vec<(String, String)> = vec![("model".into(), model.clone())];
    if let Some(lang) = fd.get_field("language") {
        text_fields.push(("language".into(), lang));
    }
    let body_bytes = build_multipart_body(
        ("file", &filename),
        &text_fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect::<Vec<_>>(),
        &bytes,
    );
    let u8 = js_sys::Uint8Array::from(&body_bytes[..]);
    let body_js = JsValue::from(u8);
    let content_type = format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}");

    // ---- 4. 渠道（audio 账本 → 回退静态）+ 首帧前重试 ----
    let channels = load_channels(env, "audio", &fallback_key, &default_base).await;

    let outcome = try_channels(
        env,
        &channels,
        "/audio/transcriptions",
        &body_js,
        |ch| {
            Ok(vec![
                ("Authorization".to_string(), format!("Bearer {}", bearer_token(ch))),
                ("Content-Type".to_string(), content_type.clone()),
            ])
        },
        false,
        |resp| {
            resp.headers()
                .get("content-type")
                .ok()
                .flatten()
                .unwrap_or_else(|| "application/json".to_string())
        },
        "audio",
        &model,
    )
    .await?;

    match outcome {
        ChannelOutcome::Locked(resp) => Ok(resp),
        ChannelOutcome::UpstreamError(status, err_body, attempts) => {
            final_error_response(status, err_body, &attempts)
        }
        ChannelOutcome::AllConnFailed(attempts) => all_channels_failed(&attempts),
    }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let method = req.method();

    // 管理页 / 管理面
    if path == "/" {
        return admin::handle_admin(req, &env, "").await;
    }
    if path.starts_with("/admin") {
        return admin::handle_admin(req, &env, &path["/admin".len()..]).await;
    }
    if method == Method::Get && path == "/health" {
        return handle_health();
    }

    match (method, path.as_str()) {
        (Method::Post, "/v1/chat/completions") => handle_chat_completions(req, &env).await,
        (Method::Post, "/v1/audio/transcriptions") => {
            handle_audio_transcriptions(req, &env).await
        }
        _ => json_error(
            404,
            "not_found",
            "routes: GET / (admin UI), GET /health, POST /v1/chat/completions, POST /v1/audio/transcriptions, /admin/api/*",
        ),
    }
}
