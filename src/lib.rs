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
const DEFAULT_AUDIO_MODEL: &str = "glm-asr-2512";
const DEFAULT_TTS_MODEL: &str = "glm-tts";
const DEFAULT_TTS_VOICE: &str = "tongtong";

/// 智谱系判定（ provider 或 base_url 命中即算 ）
fn is_zhipu_like(provider: &str, base_url: &str) -> bool {
    provider.contains("zhipu")
        || provider.contains("bigmodel")
        || base_url.contains("bigmodel.cn")
        || base_url.contains("z.ai")
}

/// STT 模型代次迁移（智谱）：`/audio/transcriptions` 只认 ASR 模型。
/// glm-4-voice 是端到端语音对话模型（走 chat.completions + input_audio），
/// 拿到转写端点会被上游按 chat 参数校验拒绝（400 "message should not be empty"）。
/// 老账本里登记的 glm-4-voice / glm-asr 自动改写为 glm-asr-2512，用户零感知。
/// 智谱转写端点（/audio/transcriptions）当前唯一模型就是 glm-asr-2512，
/// 因此智谱系渠道一律收敛到它——任何误配（含 glm-4-voice 端到端语音模型）
/// 都会在请求前被改写，从根上消除「1214 messages 参数非法」类错误。
fn resolve_stt_model(provider: &str, base_url: &str, model: &str) -> String {
    if !is_zhipu_like(provider, base_url) {
        return model.to_string();
    }
    match model {
        "glm-asr-2512" => model.to_string(),
        _ => "glm-asr-2512".to_string(),
    }
}

/// TTS 模型迁移（智谱）：`/audio/speech` 的合成模型是 glm-tts；
/// glm-4-voice 是端到端语音模型（走 chat + input_audio），不是合成端点模型，
/// 放到 /audio/speech 会被上游按 chat 参数校验 → 1214「messages 参数非法」。
/// 智谱合成端点当前唯一模型就是 glm-tts，智谱系渠道一律收敛到它。
fn resolve_tts_model(provider: &str, base_url: &str, model: &str) -> String {
    if !is_zhipu_like(provider, base_url) {
        return model.to_string();
    }
    match model {
        "glm-tts" => model.to_string(),
        _ => "glm-tts".to_string(),
    }
}
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
    // 按「主能力优先 → 渠道 priority desc → 模型 priority desc」找该能力的首个模型
    // 能力放宽与 pick_model_attempts 保持一致：模型主能力不中但 alt_kind 命中时也可选
    // （多模态模型常常既能看图又能聊天），排序上仍让主能力命中的模型优先。
    let mut best: Option<(u8, i64, i64, String)> = None;
    for c in channels.iter().filter(|c| c.kind == "chat" || want_kind != "text") {
        for m in c.effective_models() {
            if !m.serves(want_kind) || !m.enabled {
                continue;
            }
            let key = (m.primary_rank(want_kind), c.priority, m.priority, m.name.clone());
            if best.as_ref().map(|b| key > *b).unwrap_or(true) {
                best = Some(key);
            }
        }
    }
    best.map(|b| b.3).unwrap_or_else(|| {
        if want_kind == "stt" {
            DEFAULT_AUDIO_MODEL.to_string()
        } else if want_kind == "tts" {
            DEFAULT_TTS_MODEL.to_string()
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
            "version": env!("CARGO_PKG_VERSION"),
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
    degraded: &[String],
    content_type: &str,
    stream: bool,
) -> Result<web_sys::Headers> {
    let h = web_sys::Headers::new()
        .map_err(|e| Error::RustError(format!("headers: {e:?}")))?;
    let attempt_str = attempts.join(";");
    let ch_name = header_safe(&channel.name);
    let ch_attempts = header_safe(&attempt_str);
    // 被降权（首包前失败并已记入账本）的模型名列表，逗号分隔。
    // attempts 里只有渠道名，而降权是**模型级**的（同一渠道换个模型可能就好），
    // 客户端要按模型给负分就必须知道具体哪些模型被降权了。
    let degraded_str = header_safe(&degraded.join(","));
    for (k, v) in [
        ("x-gateway-version", env!("CARGO_PKG_VERSION").to_string()),
        ("x-gateway-channel", ch_name.clone()),
        ("x-gateway-provider", channel.provider.clone()),
        ("x-gateway-base-url", channel.base_url.clone()),
        ("x-gateway-model", model.to_string()),
        ("x-gateway-attempts", ch_attempts.clone()),
        ("x-gateway-degraded", degraded_str),
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
fn final_error_response(
    status: u16,
    err_body: String,
    attempts: &[String],
    degraded: &[String],
) -> Result<Response> {
    let hdrs = worker::Headers::new();
    hdrs.set("Content-Type", "application/json")?;
    hdrs.set("x-gateway-version", env!("CARGO_PKG_VERSION"))?;
    hdrs.set("x-zhipu-gateway", "wasm")?;
    let attempts_safe = header_safe(&attempts.join(";"));
    hdrs.set("x-gateway-attempts", &attempts_safe)?;
    hdrs.set("x-zhipu-attempts", &attempts_safe)?;
    hdrs.set("x-gateway-degraded", &header_safe(&degraded.join(",")))?;
    Ok(Response::from_bytes(err_body.into_bytes())?
        .with_status(status)
        .with_headers(hdrs))
}

fn all_channels_failed(attempts: &[String], degraded: &[String]) -> Result<Response> {
    let mut resp = json_response(
        502,
        serde_json::json!({
            "error": {
                "code": "all_channels_failed",
                "message": "all channels failed before first upstream packet",
                "gateway": "zhipu-wasm-gateway",
                "attempts": attempts,
            }
        }),
    )?;
    resp.headers_mut()
        .set("x-gateway-degraded", &header_safe(&degraded.join(",")))?;
    Ok(resp)
}

// ---------------------------------------------------------------------------
// 渠道循环（chat / audio 共用）：首包前失败 → 换渠道；已吐字 → 不切换
// ---------------------------------------------------------------------------

/// 逐渠道尝试。返回：Ok(响应) | Err(最后一个上游错误) | Err(全部连接失败标记 None)
enum ChannelOutcome {
    /// 成功锁定（已吐字），直接回客户端
    Locked(Response),
    /// 全部失败：最后一个上游 HTTP 错误（透传）
    UpstreamError(u16, String, Vec<String>, Vec<String>),
    /// 全部连接级失败（拿不到 HTTP 响应）
    AllConnFailed(Vec<String>, Vec<String>),
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
    // 能力匹配放宽两条：
    // ① **alt_kind 兜底**——多模态模型常常「既懂图又能聊天」（DeepSeek deepseek-flash
    //    原生支持 image 输入同时是主力文本模型，单个 kind 字段表达不了）。这类模型
    //    通过 `alt_kind` 也进 text 候选池，但排序时 `primary_rank` 靠后（见下），
    //    所以纯文本请求不会被高优先级的视觉模型抢走流量。
    // ② tts 允许回落到**音频渠道的 stt 模型**——智谱 glm-4-voice 等语音模型兼具
    //    转写与合成，用户只登记了 stt 时播音也能用。
    let kind_ok = |c: &Channel, m: &ModelMeta| {
        m.serves(want_kind) || (want_kind == "tts" && m.kind == "stt" && c.kind == "audio")
    };
    let mut pairs: Vec<(ModelMeta, &Channel)> = Vec::new();
    for c in &usable {
        for m in c.effective_models() {
            if m.enabled && !m.health.cooling(now) && kind_ok(c, &m) {
                pairs.push((m, c));
            }
        }
    }
    // 排序：主能力命中优先 → 渠道 priority → 模型 priority → 名称（保证确定性）。
    // `primary_rank` 是最高优先级键：纯文本请求时，一个 kind=text 的模型永远排在
    // 靠 alt_kind 兜底的视觉模型前面，即使后者 priority 更高。
    let by_priority = |a: &(ModelMeta, &Channel), b: &(ModelMeta, &Channel)| {
        b.0.primary_rank(want_kind)
            .cmp(&a.0.primary_rank(want_kind))
            .then(b.1.priority.cmp(&a.1.priority))
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
    // 被降权的模型名（记入账本的那些）。客户端据此按**模型**给负分——
    // attempts 里只有渠道名，而降权是模型级的。
    let mut degraded: Vec<String> = Vec::new();
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
                degraded.push(att.model.clone());
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
            degraded.push(att.model.clone());
            last_error = Some((status, err_text));
            continue;
        }

        let content_type = content_type_of(&web_resp);
        let out_headers = diag_headers(ch, &att.model, &attempts, &degraded, &content_type, stream)?;

        // ---- 用量成本采集（不干扰透传）----
        // web_sys::Response::clone() 是 tee：克隆体独立可读，原体照常做流重放。
        // 我们只在克隆体上解析 usage（流式时 usage 在最后一个 data 块，非流式在顶层），
        // 解析失败（上游不回 usage）则只记请求数与延迟、不计成本。
        let usage_probe = web_sys::Response::clone(&web_resp).ok();

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
                // 用量成本落账（旁路读 usage，不阻塞客户端响应）
                if let Some(probe) = usage_probe {
                    let ch2 = ch.clone();
                    let model2 = att.model.clone();
                    let env2 = env.clone();
                    let t0b = t0;
                    let scope2 = scope.to_string();
                    // 单价档案在闭包外解析好（避免借用跨 'static 边界）
                    let meta = ch2
                        .effective_models()
                        .into_iter()
                        .find(|m| m.name == model2)
                        .unwrap_or_else(|| ModelMeta {
                            alt_kind: String::new(),
                            name: model2.clone(),
                            kind: "text".into(),
                            enabled: true,
                            priority: 0,
                            health: Default::default(),
                            price_in_hit: 0.0,
                            price_in_miss: 0.0,
                            price_out: 0.0,
                            currency: String::new(),
                        });
                    wasm_bindgen_futures::spawn_local(async move {
                        let tokens = read_usage_tokens(probe).await;
                        let latency = (admin::now_ms() - t0b) as u64;
                        let rec = build_usage_record(&scope2, &ch2, &model2, &meta, tokens, latency, "ok");
                        store::push_usage(&env2, &rec).await;
                    });
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
        Ok(ChannelOutcome::UpstreamError(status, err_body, attempts, degraded))
    } else {
        Ok(ChannelOutcome::AllConnFailed(attempts, degraded))
    }
}

// ---------------------------------------------------------------------------
// 用量成本：token 采集 + 成本折算
// ---------------------------------------------------------------------------

/// 从上游响应体（JSON 或 SSE 文本）里提取 token 用量。
///
/// 上游 usage 字段形态（OpenAI 兼容 + DeepSeek 扩展）：
/// ```json
/// "usage": {
///   "prompt_tokens": 100,
///   "prompt_cache_hit_tokens": 30,   // DeepSeek 专有：缓存命中的输入
///   "completion_tokens": 50,
///   "total_tokens": 150
/// }
/// ```
/// 流式时 usage 出现在最后一个 `data:` 块里；非流式在响应体顶层。
/// 解析不出来（上游不回 usage）时返回 None —— 此时只记请求数与延迟，不计成本。
fn parse_usage_tokens(text: &str) -> Option<UsageTokens> {
    let key = "\"usage\"";
    let idx = text.rfind(key)?;
    let rest = &text[idx + key.len()..];
    let open = rest.find('{')?;
    // 手工截取配平的 JSON 对象（usage 内部不含嵌套花括号，但保险起见做深度计数）
    let bytes = rest.as_bytes();
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let obj: serde_json::Value = serde_json::from_str(&rest[open..=end]).ok()?;
    let g = |k: &str| obj.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let prompt = g("prompt_tokens");
    let completion = g("completion_tokens");
    let total = g("total_tokens");
    if prompt == 0 && completion == 0 && total == 0 {
        return None;
    }
    Some(UsageTokens {
        prompt_tokens: prompt,
        prompt_cache_hit_tokens: g("prompt_cache_hit_tokens"),
        completion_tokens: completion,
        total_tokens: if total > 0 { total } else { prompt + completion },
    })
}

/// token 用量三元组（+ DeepSeek 的缓存命中输入）。
#[derive(Clone, Copy, Debug, Default)]
pub struct UsageTokens {
    pub prompt_tokens: u64,
    pub prompt_cache_hit_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// 按模型的单价档案折算成本。
///
/// DeepSeek 式两段输入计费：`prompt_cache_hit_tokens` 走命中价，其余输入走未命中价。
/// 单价单位为「每百万 token」。未配单价档案（免费模型）→ 成本 0、币种空。
fn compute_cost(meta: &ModelMeta, u: &UsageTokens) -> (f64, String) {
    if !meta.priced() {
        return (0.0, String::new());
    }
    let hit = u.prompt_cache_hit_tokens.min(u.prompt_tokens);
    let miss = u.prompt_tokens.saturating_sub(hit);
    let cost_in = (hit as f64 * meta.price_in_hit + miss as f64 * meta.price_in_miss) / 1_000_000.0;
    let cost_out = (u.completion_tokens as f64 * meta.price_out) / 1_000_000.0;
    ((cost_in + cost_out), meta.currency().to_string())
}

/// 从 tee 克隆体读完整 body 并解析 usage。
///
/// 上游不回 usage 时返回 None —— 此时只记请求数/延迟，不计成本（成本记 0）。
/// 读取失败（连接中断等）同样返回 None，不影响主链路。
async fn read_usage_tokens(probe: web_sys::Response) -> Option<UsageTokens> {
    let resp = probe;
    let promise = web_sys::Response::text(&resp).ok()?;
    let text = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    parse_usage_tokens(&text)
}

/// 组装一条用量记录（成本已折算）。
#[allow(clippy::too_many_arguments)]
fn build_usage_record(
    scope: &str,
    ch: &Channel,
    model: &str,
    meta: &ModelMeta,
    tokens: Option<UsageTokens>,
    latency_ms: u64,
    status: &str,
) -> crate::channels::UsageRecord {
    let ts = crate::admin::now_iso();
    let (cost, currency) = match tokens {
        Some(u) => compute_cost(meta, &u),
        None => (0.0, String::new()),
    };
    let t = tokens.unwrap_or_default();
    crate::channels::UsageRecord {
        ts: ts.clone(),
        day: crate::channels::UsageRecord::day_of(&ts),
        scope: scope.to_string(),
        provider: ch.provider.clone(),
        channel_id: ch.id.clone(),
        channel_name: ch.name.clone(),
        model: model.to_string(),
        prompt_tokens: t.prompt_tokens,
        prompt_cache_hit_tokens: t.prompt_cache_hit_tokens,
        completion_tokens: t.completion_tokens,
        total_tokens: t.total_tokens,
        latency_ms,
        status: status.to_string(),
        cost,
        currency,
        estimated: cost > 0.0,
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

    // ---- 能力判定（统一视觉甄别）：消息里含图片/视频（image_url / video_url
    // parts）→ vision，否则 text。want_kind 只决定"要哪种能力"，真正的
    // （渠道×模型）候选挑选全部交给下方同一个 pick_model_attempts —— 视觉与
    // 文本共用同一套排序/兜底/冷却逻辑，不存在两套分支。 ----
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
        ChannelOutcome::UpstreamError(status, err_body, attempts, degraded) => {
            final_error_response(status, err_body, &attempts, &degraded)
        }
        ChannelOutcome::AllConnFailed(attempts, degraded) => {
            all_channels_failed(&attempts, &degraded)
        }
    }
}

/// 视觉甄别：请求体里是否含图片/视频（OpenAI vision parts：content 数组中的
/// `image_url` / `video_url` 元素）。任一命中即视为需要视觉能力（want_kind=vision），
/// 由统一的 pick_model_attempts 路由到带 vision 能力的（渠道×模型）。
/// 多模态 omni 模型（nemotron-nano-omni / inkling）也能消费 video_url。
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
                            matches!(
                                p.get("type").and_then(|t| t.as_str()),
                                Some("image_url") | Some("video_url")
                            )
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
        // 每个候选用自己的模型名重建 multipart（model 文本字段随候选变）。
        // 智谱转写端点只认 ASR 模型：glm-4-voice 等旧名自动改写为 glm-asr-2512；
        // stream=false 为官方基础调用必带（否则上游按流式处理，非流式解析会拿不到 text）。
        |ch, m| {
            let model = resolve_stt_model(&ch.provider, &ch.base_url, m);
            let mut tf: Vec<(String, String)> = vec![
                ("model".into(), model),
                ("stream".into(), "false".into()),
            ];
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
        ChannelOutcome::UpstreamError(status, err_body, attempts, degraded) => {
            final_error_response(status, err_body, &attempts, &degraded)
        }
        ChannelOutcome::AllConnFailed(attempts, degraded) => {
            all_channels_failed(&attempts, &degraded)
        }
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
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TTS_VOICE)
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
        // 智谱合成模型是 glm-tts（glm-4-voice 是端到端语音模型，不是合成端点模型）；
        // response_format=wav 为官方默认，显式带上避免不同 Provider 默认值不一致。
        |ch, m| {
            let model = resolve_tts_model(&ch.provider, &ch.base_url, m);
            let p = serde_json::json!({
                "model": model,
                "input": input,
                "voice": voice,
                "response_format": "wav",
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
                .unwrap_or_else(|| "audio/wav".to_string())
        },
        "tts",
    )
    .await?;

    match outcome {
        ChannelOutcome::Locked(resp) => Ok(resp),
        ChannelOutcome::UpstreamError(status, err_body, attempts, degraded) => {
            final_error_response(status, err_body, &attempts, &degraded)
        }
        ChannelOutcome::AllConnFailed(attempts, degraded) => {
            all_channels_failed(&attempts, &degraded)
        }
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
    // 分享二级子页（/s）：内容压缩后放在 URL hash 里，服务端零存储。
    // 生产环境可把 s.<域名> CNAME 到本 Worker，即得独立的二级子域分享页。
    if method == Method::Get && (path == "/s" || path == "/s/") {
        return admin::html_response(include_str!("share_ui.html"));
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
            "routes: GET / (admin UI), GET /s (share page), GET /health, POST /v1/chat/completions, POST /v1/audio/transcriptions, POST /v1/audio/speech, /admin/api/*",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_cost, header_safe, parse_usage_tokens, payload_has_image, resolve_stt_model,
        resolve_tts_model, UsageTokens,
    };
    use crate::channels::{default_deepseek_channels, default_overseas_channels, ModelMeta};

    /// 版本号单一事实源：admin.rs 的 VERSION 与 /health 的字面量都必须来自
    /// Cargo.toml（编译期注入）。曾经 admin.rs 常量停在 0.7.2 没跟着升级，
    /// 侧栏版本号（取 /admin/api/channels 的 version）一直显示旧版本——
    /// 用户看到「前端版本变成 6.0/7.2」。此断言防止再次分裂。
    #[test]
    fn version_single_source_of_truth() {
        assert_eq!(crate::admin::VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(crate::admin::VERSION, "0.14.0");
    }

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

    /// 智谱系渠道：转写端点唯一模型 glm-asr-2512——旧名与误配一律收敛。
    /// 这是 v0.7.1 遗留「messages 参数非法」事故的根因防线：
    /// glm-4-voice 是端到端语音模型，放到转写端点会被上游按 chat 校验。
    #[test]
    fn resolve_stt_converges_on_zhipu() {
        for m in ["glm-4-voice", "glm-4-voice-flash", "glm-asr", "随便配的名字"] {
            assert_eq!(resolve_stt_model("zhipu-audio", "https://open.bigmodel.cn/api/paas/v4", m), "glm-asr-2512");
        }
        assert_eq!(resolve_stt_model("zhipu", "https://open.bigmodel.cn/api/paas/v4", "glm-asr-2512"), "glm-asr-2512");
        // 非智谱渠道不碰用户配置
        assert_eq!(resolve_stt_model("siliconflow", "https://api.siliconflow.cn/v1", "FunAudioLLM/SenseVoiceSmall"), "FunAudioLLM/SenseVoiceSmall");
    }

    /// 智谱系渠道：合成端点唯一模型 glm-tts——旧名 glm-4-voice 等一律收敛。
    #[test]
    fn resolve_tts_converges_on_zhipu() {
        for m in ["glm-4-voice", "glm-4-voice-flash", "tongyi-qwen", "随便配的名字"] {
            assert_eq!(resolve_tts_model("zhipu-audio", "https://open.bigmodel.cn/api/paas/v4", m), "glm-tts");
        }
        assert_eq!(resolve_tts_model("zhipu", "https://open.bigmodel.cn/api/paas/v4", "glm-tts"), "glm-tts");
        // 非智谱渠道不碰用户配置
        assert_eq!(resolve_tts_model("siliconflow", "https://api.siliconflow.cn/v1", "FunAudioLLM/CosyVoice2-0.5B"), "FunAudioLLM/CosyVoice2-0.5B");
    }

    /// z.ai（智谱海外站）同族收敛。
    #[test]
    fn resolve_converges_on_z_ai() {
        assert_eq!(resolve_stt_model("zhipu", "https://api.z.ai/api/paas/v4", "glm-4-voice"), "glm-asr-2512");
        assert_eq!(resolve_tts_model("z.ai", "https://api.z.ai/api/paas/v4", "glm-4-voice"), "glm-tts");
    }

    /// 海外兜底种子：3 条、全部 disabled、优先级低于国内、各含 vision 能力
    ///（image_url 才能在国内全挂时落到海外视觉模型）。名字幂等（id 不重复）。
    #[test]
    fn overseas_seed_channels_well_formed() {
        let now = "2026-10-20T00:00:00Z";
        let os = default_overseas_channels(now);
        assert_eq!(os.len(), 3, "海外种子应为 openrouter/groq/nvidia 共 3 条");

        // id 唯一、全部 disabled
        let mut ids = std::collections::HashSet::new();
        for c in &os {
            assert!(ids.insert(c.id.clone()), "id 重复：{}", c.id);
            assert!(!c.enabled, "海外种子必须默认 disabled（需登记 Key）：{}", c.id);
        }

        // 优先级均低于国内主力（国内 zhipu=100 / siliconflow=90 / zhipu-audio=80）
        for c in &os {
            assert!(c.priority < 80, "海外兜底优先级应低于国内（<80），实际 {} = {}", c.id, c.priority);
        }
        // 三条之间按 openrouter(40) > groq(35) > nvidia(30) 排序
        assert_eq!(os[0].id, "seed-openrouter-chat");
        assert_eq!(os[1].id, "seed-groq-chat");
        assert_eq!(os[2].id, "seed-nvidia-chat");
        assert!(os[0].priority > os[1].priority && os[1].priority > os[2].priority);

        // 每条都自带 vision 能力（含一个视觉模型），否则 image_url 无法兜底路由
        for c in &os {
            let caps: Vec<&str> = c.caps.iter().map(|s| s.as_str()).collect();
            assert!(caps.contains(&"vision"), "{} 必须声明 vision 能力", c.id);
            assert!(c.models_meta.iter().any(|m| m.kind == "vision"), "{} 必须至少登记一个视觉模型", c.id);
        }

        // OpenRouter 种子应补齐全部免费视觉/多模态模型（此前只列了 gemma-4-31b）
        let or = os.iter().find(|c| c.id == "seed-openrouter-chat").unwrap();
        let or_names: Vec<&str> = or.models_meta.iter().map(|m| m.name.as_str()).collect();
        for m in [
            "google/gemma-4-26b-a4b-it:free",
            "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
            "thinkingmachines/inkling:free",
            "thinkingmachines/inkling-small:free",
        ] {
            assert!(or_names.contains(&m), "OpenRouter 种子漏了免费视觉模型：{m}");
        }
        // 此前误标为 text 的两个视觉模型应修正为 vision
        for m in ["qwen/qwen3.8-27b:free", "openrouter/free"] {
            let meta = or.models_meta.iter().find(|x| x.name == m).unwrap();
            assert_eq!(meta.kind, "vision", "{m} 应为视觉能力");
        }

        // 时间戳字段填充
        for c in &os {
            assert_eq!(c.created_at, now);
            assert_eq!(c.updated_at, now);
        }
    }

    /// 视觉甄别：image_url / video_url 命中→vision；纯文本/畸形→text。
    #[test]
    fn payload_vision_detection() {
        let img = serde_json::json!({
            "messages": [{"role":"user","content":[
                {"type":"text","text":"图里是?"},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,AAA"}}
            ]}]
        });
        assert!(payload_has_image(&img), "image_url 应触发 vision");

        let vid = serde_json::json!({
            "messages": [{"role":"user","content":[
                {"type":"video_url","video_url":{"url":"data:video/mp4;base64,BBB"}}
            ]}]
        });
        assert!(payload_has_image(&vid), "video_url 应触发 vision（多模态 omni 模型）");

        let txt = serde_json::json!({
            "messages": [{"role":"user","content":"你好"}]
        });
        assert!(!payload_has_image(&txt), "纯文本不应触发 vision");

        let broken = serde_json::json!({});
        assert!(!payload_has_image(&broken), "畸形体不应触发 vision");
    }

    /// usage 解析：OpenAI 兼容顶层 usage + DeepSeek 缓存命中扩展字段。
    #[test]
    fn usage_parsing() {
        // 非流式：usage 在响应体顶层
        let body = r#"{"id":"x","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#;
        let u = parse_usage_tokens(body).expect("应解析出 usage");
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
        assert_eq!(u.prompt_cache_hit_tokens, 0);

        // DeepSeek 扩展：缓存命中 token 单列
        let ds = r#"{"usage":{"prompt_tokens":100,"prompt_cache_hit_tokens":30,"completion_tokens":20,"total_tokens":120}}"#;
        let u2 = parse_usage_tokens(ds).expect("应解析出 DeepSeek usage");
        assert_eq!(u2.prompt_cache_hit_tokens, 30);
        assert_eq!(u2.prompt_tokens, 100);

        // 流式：usage 在最后一个 data 块里
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\ndata: [DONE]\n\n";
        let u3 = parse_usage_tokens(sse).expect("应从 SSE 解析出 usage");
        assert_eq!(u3.prompt_tokens, 7);
        assert_eq!(u3.completion_tokens, 3);

        // 上游不回 usage → None（只记请求数，不计成本）
        assert!(parse_usage_tokens(r#"{"choices":[]}"#).is_none());
        assert!(parse_usage_tokens("").is_none());
    }

    /// 成本折算：DeepSeek 式两段输入计费（命中价 / 未命中价）+ 输出价。
    #[test]
    fn cost_computation() {
        let meta = ModelMeta {
            alt_kind: String::new(),
            name: "deepseek-flash".into(),
            kind: "text".into(),
            enabled: true,
            priority: 100,
            health: Default::default(),
            price_in_hit: 0.02,
            price_in_miss: 1.0,
            price_out: 2.0,
            currency: "CNY".into(),
        };
        // 100 输入（其中 30 命中）+ 50 输出
        let u = UsageTokens {
            prompt_tokens: 100,
            prompt_cache_hit_tokens: 30,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let (cost, cur) = compute_cost(&meta, &u);
        // 30×0.02 + 70×1.0 + 50×2.0 = 0.6 + 70 + 100 = 170.6 元/百万 → 0.0001706
        assert_eq!(cur, "CNY");
        assert!((cost - 0.0001706).abs() < 1e-9, "成本折算不对：{cost}");

        // 免费模型（无单价档案）→ 成本 0、币种空
        let free = ModelMeta {
            alt_kind: String::new(),
            name: "free".into(),
            kind: "text".into(),
            enabled: true,
            priority: 50,
            health: Default::default(),
            price_in_hit: 0.0,
            price_in_miss: 0.0,
            price_out: 0.0,
            currency: String::new(),
        };
        let (c2, cur2) = compute_cost(&free, &u);
        assert_eq!(c2, 0.0);
        assert_eq!(cur2, "");
    }

    /// DeepSeek 种子渠道：现行模型名 + 单价档案 + 默认 disabled。
    #[test]
    fn deepseek_seed_channel_well_formed() {
        let now = "2026-10-20T00:00:00Z";
        let ds = default_deepseek_channels(now);
        assert_eq!(ds.len(), 1, "DeepSeek 种子应为 1 条");
        let c = &ds[0];
        assert_eq!(c.id, "seed-deepseek-chat");
        assert!(!c.enabled, "种子必须默认 disabled（需登记 Key）");
        assert!(c.priority < 80, "DeepSeek 优先级应低于国内主力，实际 {}", c.priority);
        assert!(c.caps.iter().any(|x| x == "vision"), "应声明 vision 能力");
        // 现行模型名（旧的 deepseek-chat/deepseek-reasoner 已弃用）
        let names: Vec<&str> = c.models_meta.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"deepseek-flash"), "应含 deepseek-flash");
        assert!(names.contains(&"deepseek-v4-pro"), "应含 deepseek-v4-pro");
        assert!(
            !names.contains(&"deepseek-chat"),
            "deepseek-chat 已弃用，不应再作默认模型"
        );
        // deepseek-flash 是主力低价模型（主能力 text），实测原生支持 image 输入 →
        // 次要能力标 vision。纯文本与看图都该命中它。
        let flash = c.models_meta.iter().find(|m| m.name == "deepseek-flash").unwrap();
        assert_eq!(flash.kind, "text", "deepseek-flash 主能力应为 text（主力低价模型）");
        assert_eq!(flash.alt_kind, "vision", "deepseek-flash 次要能力应为 vision（支持 image 输入）");
        // 渠道 caps 仍需声明 vision，图片请求才可能路由进来
        assert!(c.caps.iter().any(|x| x == "vision"), "渠道 caps 应含 vision");
        // 单价档案齐全（CNY）
        for m in &c.models_meta {
            assert!(m.priced(), "{} 缺单价档案", m.name);
            assert_eq!(m.currency(), "CNY", "{} 币种应为 CNY", m.name);
        }
        assert_eq!(c.created_at, now);
        assert_eq!(c.updated_at, now);
    }

    /// DeepSeek 主力必须真的能被路由选中——不只是「种子存在」。
    /// 回归背景：deepseek-flash 起初被标成 kind=vision，纯文本请求把它整条过滤掉，
    /// DeepSeek 流量全落到更贵的 deepseek-v4-pro。修法是引入 `alt_kind` 表达
    /// 「既懂图又能聊天」，这里锁死该行为。
    #[test]
    fn deepseek_flash_reachable_for_both_text_and_vision() {
        let now = 1_800_000_000_000f64;
        let mut ds = default_deepseek_channels("2026-10-20T00:00:00Z");
        ds[0].enabled = true;
        ds[0].api_key = "sk-test".into();

        // 纯文本：应命中 deepseek-flash（模型优先级 100 > pro 的 90），
        // 而不是绕过它去用更贵的 deepseek-v4-pro。
        let (attempts, notes) = super::pick_model_attempts(&ds, "text", None, now);
        assert!(!attempts.is_empty(), "DeepSeek 应有 text 候选：{notes:?}");
        assert_eq!(
            attempts[0].model, "deepseek-flash",
            "纯文本应命中 deepseek-flash（vision 是 text 的超集）"
        );

        // 视觉：仍命中 deepseek-flash
        let (vis, _) = super::pick_model_attempts(&ds, "vision", None, now);
        assert!(!vis.is_empty(), "DeepSeek 应有 vision 候选");
        assert_eq!(vis[0].model, "deepseek-flash", "视觉应命中 deepseek-flash");
    }

    /// 视觉候选里不能混入纯文本模型（text 接不了图，没有 alt_kind 就不能进 vision 池）。
    #[test]
    fn text_only_model_never_serves_vision() {
        let now = 1_800_000_000_000f64;
        let mut ds = default_deepseek_channels("2026-10-20T00:00:00Z");
        ds[0].enabled = true;
        ds[0].api_key = "sk-test".into();

        // 显式请求 pro + 纯文本 → 正常
        let (t, _) = super::pick_model_attempts(&ds, "text", Some("deepseek-v4-pro"), now);
        assert_eq!(t[0].model, "deepseek-v4-pro");

        // 视觉候选里不能只有 pro —— 必须有 flash 兜底
        let (v, _) = super::pick_model_attempts(&ds, "vision", None, now);
        assert!(
            v.iter().any(|a| a.model == "deepseek-flash"),
            "视觉候选应含 deepseek-flash"
        );
        assert!(
            !v.iter().any(|a| a.model == "deepseek-v4-pro"),
            "纯文本模型 deepseek-v4-pro 不应进视觉候选"
        );
    }
}
