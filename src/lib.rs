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
use crate::channels::ModelMeta;

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
                    // 音频渠道（stt/tts）允许暂缺 Key：进入候选池后由 borrow_key_if_absent
                    // 从同 provider 聊天渠道补位；聊天渠道（text）仍须自带 Key。
                    c.kind == kind
                        && c.enabled
                        && !c.cooling(now)
                        && (kind == "audio" || !c.api_key.is_empty())
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

/// 默认模型：优先绑定 → 账本中该能力的最高优先级模型 → 内置默认。
/// `want_kind`：text / vision / stt / tts——按请求能力选默认模型。
async fn resolve_default_model(env: &Env, channels: &[Channel], want_kind: &str) -> String {
    if let Some(m) = binding(env, "ZHIPU_MODEL", None) {
        if !m.is_empty() {
            return m;
        }
    }
    // 按「渠道 priority desc → 模型 priority desc」找该能力的首个模型
    let mut best: Option<(i64, i64, String)> = None;
    for c in channels.iter().filter(|c| c.kind == "chat" || want_kind != "text") {
        for m in c.effective_models() {
            if m.kind != want_kind || !m.enabled {
                continue;
            }
            let key = (c.priority, m.priority, m.name.clone());
            if best.as_ref().map(|b| (key.0, key.1) > (b.0, b.1)).unwrap_or(true) {
                best = Some(key);
            }
        }
    }
    best.map(|b| b.2).unwrap_or_else(|| {
        if want_kind == "stt" || want_kind == "tts" {
            DEFAULT_AUDIO_MODEL.to_string()
        } else {
            DEFAULT_MODEL.to_string()
        }
    })
}

/// 没有可用候选：明确告诉客户端缺什么能力（模型级 503）。
fn no_candidates_response(want_kind: &str, notes: &[String]) -> Result<Response> {
    let kind_cn = match want_kind {
        "vision" => "视觉（图片/视频理解）",
        "stt" => "语音转写（STT）",
        "tts" => "语音合成（TTS 播音）",
        _ => "文本对话",
    };
    json_response(
        503,
        serde_json::json!({
            "error": {
                "code": "no_candidate_model",
                "message": format!("没有可用的{kind_cn}模型——请到模型管理登记/启用对应能力的渠道与模型"),
                "want_kind": want_kind,
                "notes": notes,
                "gateway": "zhipu-wasm-gateway",
            }
        }),
    )
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
            "version": "0.7.1",
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

/// HTTP 响应头值只允许 ASCII：中文渠道名/ID 直接塞进头会被浏览器按 Latin-1
/// 解成乱码（`智谱` → `æ™ºè°±`）。这里对非 ASCII 字节做 percent-encoding
/// （RFC 3986 非保留字符原样保留），前端用 decodeURIComponent 还原。
/// ASCII 值（版本号/URL/模型名等）不受影响，编码后仍是原值。
fn header_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(
            b,
            b'-' | b'.' | b'_' | b'~' | b':' | b'/' | b'?' | b'#' | b'[' | b']' | b'@'
                | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
        );
        if keep {
            out.push(b as char);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// 给客户端响应统一注入诊断头（兼容旧 x-zhipu-* + 新增 x-gateway-*）。
/// 所有可能含中文的值（渠道名、attempts 里的渠道 ID）都经 header_safe 编码，
/// 保证线上响应头是纯 ASCII，前端 decodeURIComponent 还原。
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
    let ch_name = header_safe(&channel.name);
    let ch_attempts = header_safe(&attempt_str);
    for (k, v) in [
        ("x-gateway-version", "0.7.1".to_string()),
        ("x-gateway-channel", ch_name.clone()),
        ("x-gateway-provider", channel.provider.clone()),
        ("x-gateway-base-url", channel.base_url.clone()),
        ("x-gateway-model", model.to_string()),
        ("x-gateway-attempts", ch_attempts.clone()),
        ("x-zhipu-gateway", "wasm".to_string()),
        ("x-zhipu-channel", ch_name),
        ("x-zhipu-base-url", channel.base_url.clone()),
        ("x-zhipu-model", model.to_string()),
        ("x-zhipu-attempts", ch_attempts),
    ] {
        h.set(k, &v)
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
    hdrs.set("x-gateway-version", "0.7.1")?;
    hdrs.set("x-zhipu-gateway", "wasm")?;
    let attempts_safe = header_safe(&attempts.join(";"));
    hdrs.set("x-gateway-attempts", &attempts_safe)?;
    hdrs.set("x-zhipu-attempts", &attempts_safe)?;
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

/// 一个待尝试的（渠道 × 模型）候选。模型是路由一等公民：
/// 某个模型出错只冷却它自己，同渠道其他模型/其他渠道的同能力模型继续顶上。
#[derive(Clone)]
struct ModelAttempt {
    ch: Channel,
    model: String,
}

impl ModelAttempt {
    /// 模型级优先级（找不到该模型时返回 0）
    #[allow(dead_code)]
    fn model_priority(&self) -> i64 {
        self.ch
            .effective_models()
            .iter()
            .find(|m| m.name == self.model)
            .map(|m| m.priority)
            .unwrap_or(0)
    }
}

/// 模型选择引擎：跨渠道选出可提供 `want_kind` 能力的（渠道×模型）候选，
/// 候选排序：渠道 priority desc → 模型 priority desc → 模型名。
///
/// 行为（实现"模型出错自动更换"）：
/// - 客户端指定模型 `rm`：优先把所有**提供该模型**的渠道排在最前（按优先级），
///   任一个上游失败即换下一个渠道（同模型跨渠道切换）；
/// - 若全部提供 `rm` 的渠道都失败，再回退到**同类其他模型**（模型级自动更换）；
/// - 未指定模型：直接用全部同能力模型（按优先级）。
/// - 过滤：渠道 enabled + 有 Key(音频可暂缺) + 未冷却；模型 enabled + 未冷却。
fn pick_model_attempts(
    channels: &[Channel],
    want_kind: &str,
    requested: Option<&str>,
    now: f64,
) -> (Vec<ModelAttempt>, Vec<String>) {
    let out: Vec<ModelAttempt>;
    let mut notes: Vec<String> = Vec::new();
    let usable: Vec<&Channel> = channels
        .iter()
        .filter(|c| {
            // 同 load_channels：音频渠道允许暂缺 Key（随后跨渠道借用），聊天渠道须自带 Key
            c.enabled
                && (c.kind == "audio" || !c.api_key.is_empty())
                && !c.cooling(now)
        })
        .collect();

    // 收集全部可用（渠道×模型）对：渠道提供该能力、模型启用且未冷却。
    // 能力匹配放宽：tts 允许回落到**音频渠道的 stt 模型**——智谱 glm-4-voice
    // 等语音模型兼具转写与合成，用户只登记了 stt 时播音也能用。
    let kind_ok = |c: &Channel, m: &ModelMeta| {
        m.kind == want_kind || (want_kind == "tts" && m.kind == "stt" && c.kind == "audio")
    };
    let mut pairs: Vec<(ModelMeta, &Channel)> = Vec::new();
    for c in &usable {
        for m in c.effective_models() {
            if m.enabled && !m.health.cooling(now) && kind_ok(c, &m) {
                pairs.push((m, c));
            }
        }
    }
    let by_priority = |a: &(ModelMeta, &Channel), b: &(ModelMeta, &Channel)| {
        b.1.priority
            .cmp(&a.1.priority)
            .then(b.0.priority.cmp(&a.0.priority))
            .then(a.0.name.cmp(&b.0.name))
    };

    if let Some(rm) = requested {
        // 主选：所有提供该模型的渠道（跨渠道），按优先级
        let mut primary: Vec<(ModelMeta, &Channel)> =
            pairs.iter().filter(|(m, _)| m.name == rm).cloned().collect();
        if !primary.is_empty() {
            primary.sort_by(by_priority);
            let mut list: Vec<ModelAttempt> = primary
                .iter()
                .map(|(m, c)| ModelAttempt { ch: (*c).clone(), model: m.name.clone() })
                .collect();
            // 回退：同类其他模型（主选全部失败后再尝试）——实现模型级自动更换
            let mut fallback: Vec<(ModelMeta, &Channel)> =
                pairs.iter().filter(|(m, _)| m.name != rm).cloned().collect();
            fallback.sort_by(by_priority);
            list.extend(
                fallback
                    .iter()
                    .map(|(m, c)| ModelAttempt { ch: (*c).clone(), model: m.name.clone() }),
            );
            return (list, notes);
        }
        notes.push(format!("requested-model-unavailable:{rm}"));
    }

    let mut all = pairs;
    all.sort_by(by_priority);
    out = all
        .into_iter()
        .map(|(m, c)| ModelAttempt { ch: (*c).clone(), model: m.name })
        .collect();
    (out, notes)
}

/// Key 复用：智谱主线与音频渠道同 Base URL 同 Key。音频/视觉渠道没登记 Key 时，
/// 用同 provider（或同 base_url）的聊天渠道 Key 补位——用户只登记一处即可。
fn borrow_key_if_absent(ch: &mut Channel, all: &[Channel]) {
    if !ch.api_key.is_empty() {
        return;
    }
    if let Some(donor) = all
        .iter()
        .find(|c| !c.api_key.is_empty() && (c.provider == ch.provider || c.base_url == ch.base_url))
    {
        ch.api_key = donor.api_key.clone();
    }
}

/// 客户端请求头 `x-device-keys`：{ "id": "设备序列号", "keys": { "provider": "sk-…" } }
/// 语义：A 设备不想用 B 登记的共享 Key —— 浏览器按 provider 缓存一份自己的 Key，
/// 只在本机请求生效、不写入共享账本（内存态）。渠道自身没登记 Key 时按 provider
/// 匹配补上；provider 名大小写不敏感。
#[derive(serde::Deserialize, Default)]
struct DeviceKeys {
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    keys: std::collections::BTreeMap<String, String>,
}

fn apply_device_keys(channels: &mut [Channel], req: &worker::Request) {
    let raw = req
        .headers()
        .get("x-device-keys")
        .ok()
        .flatten()
        .unwrap_or_default();
    if raw.trim().is_empty() {
        return;
    }
    let dk: DeviceKeys = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return, // 头格式不对：忽略（不影响共享账本链路）
    };
    if dk.keys.is_empty() {
        return;
    }
    for ch in channels.iter_mut() {
        if !ch.api_key.is_empty() {
            continue; // 账本已登记 Key 的渠道优先用账本的
        }
        let prov = ch.provider.to_ascii_lowercase();
        if let Some(k) = dk
            .keys
            .iter()
            .find(|(p, _)| p.to_ascii_lowercase() == prov)
            .map(|(_, v)| v)
        {
            if !k.trim().is_empty() {
                ch.api_key = k.trim().to_string();
            }
        }
    }
}

/// 逐（渠道×模型）尝试。每个候选用自己的模型名重建请求体（make_body 回调）。
/// 首包前失败 → 换下一个候选；已吐字 → 锁定不切换。
async fn try_model_attempts(
    env: &Env,
    attempts_list: &[ModelAttempt],
    path: &str,
    make_body: impl Fn(&Channel, &str) -> JsValue,
    make_headers: impl Fn(&Channel) -> Result<Vec<(String, String)>>,
    stream: bool,
    content_type_of: impl Fn(&web_sys::Response) -> String,
    scope: &str,
) -> Result<ChannelOutcome> {
    let mut attempts: Vec<String> = Vec::new();
    let mut last_error: Option<(u16, String)> = None;
    let t0 = admin::now_ms();

    for att in attempts_list {
        let ch = &att.ch;
        let headers = make_headers(ch)?;
        let body = make_body(ch, &att.model);
        let (status, web_resp) = match fetch_channel(
            ch,
            path,
            &body,
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
                note_failure(env, ch, 0, scope, &att.model, &attempts).await;
                continue;
            }
        };

        // 非 2xx（429/5xx/4xx）：模型级记健康，立即换下一个候选
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
            note_failure(env, ch, status, scope, &att.model, &attempts).await;
            last_error = Some((status, err_text));
            continue;
        }

        let content_type = content_type_of(&web_resp);
        let out_headers = diag_headers(ch, &att.model, &attempts, &content_type, stream)?;

        // 读首帧，失败（连接中断/空流）才算"首包前失败"，换候选
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
                    admin::report_success(env, &ch.id, &ch.provider, scope, &att.model, &attempts, latency)
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
    let payload: serde_json::Value = if body_text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&body_text)
            .map_err(|e| Error::RustError(format!("invalid JSON body: {e}")))?
    };

    let stream = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // ---- 能力判定：消息里含图片（image_url parts）→ vision，否则 text ----
    let want_kind = if payload_has_image(&payload) { "vision" } else { "text" };

    // ---- 渠道（账本动态优先，回退静态）----
    let mut channels = load_channels(env, "chat", &fallback_key, &default_base).await;
    apply_device_keys(&mut channels, &req);

    let model_missing = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    let requested = if model_missing {
        Some(resolve_default_model(env, &channels, want_kind).await)
    } else {
        Some(payload["model"].as_str().unwrap().to_string())
    };

    // ---- 模型选择引擎：跨渠道挑（渠道×模型）候选 ----
    let (candidates, notes) =
        pick_model_attempts(&channels, want_kind, requested.as_deref(), admin::now_ms());
    if candidates.is_empty() {
        return no_candidates_response(want_kind, &notes);
    }
    // Key 复用：候选渠道没登记 Key 时，向同 provider/base_url 的渠道借
    let mut candidates = candidates;
    for c in candidates.iter_mut() {
        borrow_key_if_absent(&mut c.ch, &channels);
    }

    let outcome = try_model_attempts(
        env,
        &candidates,
        "/chat/completions",
        // 每个候选用自己的模型名重建请求体
        |_ch, model| {
            let mut p = payload.clone();
            p["model"] = serde_json::Value::String(model.to_string());
            JsValue::from_str(&serde_json::to_string(&p).unwrap_or_default())
        },
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

/// 请求体里是否含图片（OpenAI vision parts：content 数组中的 image_url 元素）。
fn payload_has_image(payload: &serde_json::Value) -> bool {
    payload
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter().any(|m| {
                m.get("content")
                    .and_then(|c| c.as_array())
                    .map(|parts| {
                        parts.iter().any(|p| {
                            p.get("type").and_then(|t| t.as_str()) == Some("image_url")
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// B. multipart 音视频上传：解 multipart → 裸 bytes → 手工重建 multipart 转发
// ---------------------------------------------------------------------------

const MULTIPART_BOUNDARY: &str = "ZhipuWasmGatewayBoundary7f3A9c";

/// 手工拼装 multipart/form-data 字节体（不依赖 FormData 序列化，任何平台稳成）。
/// `content_type` 透传客户端上传的原始 MIME（空则回退 octet-stream）——
/// 上游按文件名/Content-Type 识别音视频格式（如 audio/webm），不能一律 octet-stream。
fn build_multipart_body(
    file_field: (&str, &str),
    content_type: &str,
    text_fields: &[(&str, String)],
    bytes: &[u8],
) -> Vec<u8> {
    let (field, filename) = file_field;
    let ct = if content_type.is_empty() {
        "application/octet-stream"
    } else {
        content_type
    };
    let mut body: Vec<u8> = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {ct}\r\n\r\n"
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
    // 客户端显式指定的 stt 模型（可空——空则交给引擎按能力挑选）
    let model = fd
        .get_field("model")
        .or_else(|| binding(env, "ZHIPU_AUDIO_MODEL", None))
        .unwrap_or_default();

    // ---- 2. 裸字节读取（File::bytes：Blob.arrayBuffer -> Vec<u8>） ----
    let bytes = file.bytes().await?;
    let file_mime = file.type_();
    let content_type = format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}");

    // ---- 4. 模型选择引擎（stt 能力）+ Key 复用 + 首帧前重试 ----
    let mut channels = load_channels(env, "audio", &fallback_key, &default_base).await;
    apply_device_keys(&mut channels, &req);
    // Key 借用候选池：除音频渠道自身外，还纳入同 provider 的聊天渠道
    // —— 用户只在智谱主线登记一次 Key，音频渠道自动复用（修复"语音识别报错"）。
    let mut donors = load_channels(env, "chat", &fallback_key, &default_base).await;
    apply_device_keys(&mut donors, &req);
    let donor_pool: Vec<Channel> = channels.iter().chain(donors.iter()).cloned().collect();
    let requested_model = if model.is_empty() { None } else { Some(model.clone()) };
    let (mut candidates, notes) =
        pick_model_attempts(&channels, "stt", requested_model.as_deref(), admin::now_ms());
    // 静态回退场景：candidates 可能来自静态渠道（无 models_meta）——把请求的
    // 模型名补进去，保证 ZHIPU_AUDIO_MODEL / DEFAULT_AUDIO_MODEL 仍然生效
    if candidates.is_empty() && !channels.is_empty() {
        let m = requested_model.unwrap_or_else(|| DEFAULT_AUDIO_MODEL.to_string());
        candidates = channels
            .iter()
            .map(|ch| ModelAttempt { ch: ch.clone(), model: m.clone() })
            .collect();
    }
    if candidates.is_empty() {
        return no_candidates_response("stt", &notes);
    }
    // Key 复用：音频渠道没登记 Key → 借同 provider（智谱主线）的 Key
    for c in candidates.iter_mut() {
        borrow_key_if_absent(&mut c.ch, &donor_pool);
    }
    // 借完仍无 Key 的候选剔除（空 Bearer 打上游只会吃 401，白烧一次尝试）
    candidates.retain(|c| !c.ch.api_key.is_empty());
    if candidates.is_empty() {
        return json_error(
            503,
            "no_usable_key",
            "没有带 Key 的渠道可用于语音识别——请先在模型管理给渠道登记 Key",
        );
    }

    let outcome = try_model_attempts(
        env,
        &candidates,
        "/audio/transcriptions",
        // 每个候选用自己的模型名重建 multipart（model 文本字段随候选变）
        |_ch, m| {
            let mut tf: Vec<(String, String)> = vec![("model".into(), m.to_string())];
            if let Some(lang) = fd.get_field("language") {
                tf.push(("language".into(), lang));
            }
            let bb = build_multipart_body(
                ("file", &filename),
                &file_mime,
                &tf.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>(),
                &bytes,
            );
            let u8 = js_sys::Uint8Array::from(&bb[..]);
            JsValue::from(u8)
        },
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
// C. TTS 语音合成（播音）：OpenAI 兼容 /v1/audio/speech
// ---------------------------------------------------------------------------

/// POST /v1/audio/speech：{ model, input, voice } → 上游音频字节流。
/// 供「播音按钮 / 双向通话」使用；tts 能力模型经引擎挑选，Key 复用同主线。
async fn handle_audio_speech(mut req: Request, env: &Env) -> Result<Response> {
    let fallback_key = binding(env, "ZHIPU_API_KEY", None).unwrap_or_default();
    let default_base = binding(env, "ZHIPU_BASE_URL", Some(DEFAULT_BASE_URL))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let body_text = req.text().await?;
    let payload: serde_json::Value = if body_text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&body_text)
            .map_err(|e| Error::RustError(format!("invalid JSON body: {e}")))?
    };
    let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if input.trim().is_empty() {
        return json_error(400, "missing_input", "field 'input' is required");
    }
    let voice = payload
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("tongyi-qwen")
        .to_string();
    let requested = payload
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    // 候选池 = 聊天渠道 + 音频渠道：tts 模型（glm-4-voice 等）登记在音频渠道上，
    // 只取 chat 会「播音无模型」；Key 借用池同理（音频渠道常缺 Key，借主线）。
    let mut chat_ch = load_channels(env, "chat", &fallback_key, &default_base).await;
    let mut audio_ch = load_channels(env, "audio", &fallback_key, &default_base).await;
    apply_device_keys(&mut chat_ch, &req);
    apply_device_keys(&mut audio_ch, &req);
    let mut channels: Vec<Channel> = chat_ch;
    channels.extend(audio_ch);
    let donor_pool: Vec<Channel> = channels.clone();
    let (mut candidates, notes) =
        pick_model_attempts(&channels, "tts", requested.as_deref(), admin::now_ms());
    if candidates.is_empty() {
        return no_candidates_response("tts", &notes);
    }
    for c in candidates.iter_mut() {
        borrow_key_if_absent(&mut c.ch, &donor_pool);
    }
    // 借完仍无 Key 的候选直接剔除（否则带着空 Bearer 打上游只会吃 401）
    candidates.retain(|c| !c.ch.api_key.is_empty());
    if candidates.is_empty() {
        return json_error(
            503,
            "no_usable_key",
            "没有带 Key 的渠道可用于播音——请先在模型管理给渠道登记 Key",
        );
    }

    let outcome = try_model_attempts(
        env,
        &candidates,
        "/audio/speech",
        |_ch, m| {
            let p = serde_json::json!({
                "model": m,
                "input": input,
                "voice": voice,
            });
            JsValue::from_str(&p.to_string())
        },
        |ch| {
            Ok(vec![
                ("Authorization".to_string(), format!("Bearer {}", bearer_token(ch))),
                ("Content-Type".to_string(), "application/json".to_string()),
            ])
        },
        false,
        |resp| {
            resp.headers()
                .get("content-type")
                .ok()
                .flatten()
                .unwrap_or_else(|| "audio/mpeg".to_string())
        },
        "tts",
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
        (Method::Post, "/v1/audio/speech") => handle_audio_speech(req, &env).await,
        _ => json_error(
            404,
            "not_found",
            "routes: GET / (admin UI), GET /health, POST /v1/chat/completions, POST /v1/audio/transcriptions, POST /v1/audio/speech, /admin/api/*",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::header_safe;

    /// 诊断头只允许 ASCII：中文渠道名必须 percent-encoding（浏览器按 Latin-1
    /// 解响应头，裸中文会变乱码，如 `智谱` → `æ™ºè°±`）。
    #[test]
    fn header_safe_encodes_non_ascii() {
        assert_eq!(header_safe("智谱-JWT"), "%E6%99%BA%E8%B0%B1-JWT");
        assert_eq!(header_safe("硅基流动-Qwen"), "%E7%A1%85%E5%9F%BA%E6%B5%81%E5%8A%A8-Qwen");
        // 空串与纯 ASCII 原样返回（URL / 模型名 / 版本号不受影响）
        assert_eq!(header_safe(""), "");
        assert_eq!(header_safe("dyn-b"), "dyn-b");
        assert_eq!(header_safe("https://open.bigmodel.cn/api/paas/v4"), "https://open.bigmodel.cn/api/paas/v4");
        assert_eq!(header_safe("ch-a=HTTP429;ch-b=ok"), "ch-a=HTTP429;ch-b=ok");
    }

    /// 编码值必须能被 JS decodeURIComponent 语义还原（标准 percent-encoding，
    /// 含 % 自身的转义，保证往返无损）。
    #[test]
    fn header_safe_roundtrip() {
        for s in ["智谱-JWT", "硅基流动-Qwen", "渠道 100%", "a&b=c", "emoji-🎙️"] {
            let enc = header_safe(s);
            assert!(enc.is_ascii(), "编码后必须纯 ASCII: {enc}");
            // 手写解码（std 无 percent 解码），与 JS decodeURIComponent 行为一致
            let mut dec = Vec::new();
            let bytes = enc.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                    dec.push(u8::from_str_radix(hex, 16).unwrap());
                    i += 3;
                } else {
                    dec.push(bytes[i]);
                    i += 1;
                }
            }
            assert_eq!(String::from_utf8(dec).unwrap(), s, "往返必须无损: {s}");
        }
    }
}
