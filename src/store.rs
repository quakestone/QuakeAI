//! 资源账本的持久层：Durable Object 单例（JS glue 中的 ResourceStore 类）
//! + 主线侧 RPC 封装。
//!
//! DO 只提供三个原语（channels 读/全量写、events 读/追加），所有业务合并
//! （登记、轮换、健康回写）都在主线读-改-写完成 —— DO 天然串行化，无锁安全。
//!
//! DO 类本体在 build 阶段追加进 index.js（见 scripts/build.sh 的 post-glue），
//! 这样 wasm-bindgen 的 re-export 不参与 DO 识别，workerd 静态解析
//! `export class ResourceStore extends DurableObject` 即可完成注册。

use worker::*;
use wasm_bindgen::prelude::*;
use crate::channels::*;

// ---------------------------------------------------------------------------
// 主线侧 RPC
// ---------------------------------------------------------------------------

/// 向资源账本 DO 发一次请求。调用方负责处理 Err（回退静态配置）。
pub async fn store_rpc(env: &Env, method: Method, path: &str, body: Option<&str>) -> Result<Response> {
    let namespace = env
        .durable_object(DO_BINDING)
        .map_err(|e| Error::RustError(format!("durable_object binding {DO_BINDING}: {e:?}")))?;
    let id = namespace.id_from_name(DO_NAME)?;
    let stub = id.get_stub()?;
    let mut init = worker::RequestInit::new();
    init.with_method(method);
    if let Some(b) = body {
        init.with_body(Some(JsValue::from_str(b)));
        let h = worker::Headers::new();
        h.set("Content-Type", "application/json")?;
        init.with_headers(h);
    }
    let req = Request::new_with_init(&format!("http://resource-store{path}"), &init)
        .map_err(|e| Error::RustError(format!("store request: {e:?}")))?;
    stub.fetch_with_request(req).await
}

/// 读取完整渠道账本（Err = DO 不可用，调用方回退静态通道）。
/// 出口处做一次「语音端点模型」零迁移：老账本音频渠道登记的是
/// glm-4-voice（端到端语音模型，转写/合成端点都不认），按能力改成
/// glm-asr-2512（stt）/ glm-tts（tts）——只改内存态，不回写 DO，
/// 用户在管理页看到的仍是登记值（显示层另行提示）。
pub async fn load_all_channels(env: &Env) -> Result<Vec<Channel>> {
    let mut resp = store_rpc(env, Method::Get, "/channels", None).await?;
    let text = resp.text().await?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let mut list: Vec<Channel> = v
        .get("channels")
        .and_then(|c| serde_json::from_value::<Vec<Channel>>(c.clone()).ok())
        .unwrap_or_default();
    for ch in list.iter_mut() {
        if ch.kind != "audio" {
            continue;
        }
        for m in ch.models_meta.iter_mut() {
            let migrated = match (m.name.as_str(), m.kind.as_str()) {
                ("glm-4-voice" | "glm-4-voice-flash", "stt") => Some("glm-asr-2512"),
                ("glm-4-voice" | "glm-4-voice-flash", "tts") => Some("glm-tts"),
                ("glm-asr", "stt") => Some("glm-asr-2512"),
                _ => None,
            };
            if let Some(new) = migrated {
                m.name = new.to_string();
            }
        }
        for name in ch.models.iter_mut() {
            if name == "glm-asr" {
                *name = "glm-asr-2512".to_string();
            }
        }
    }
    Ok(list)
}

/// 全量覆盖渠道账本。
pub async fn save_all_channels(env: &Env, list: &[Channel]) -> Result<()> {
    let body = serde_json::to_string(list)
        .map_err(|e| Error::RustError(format!("serialize channels: {e}")))?;
    store_rpc(env, Method::Put, "/channels", Some(&body)).await?;
    Ok(())
}

/// 读取完整 skill 账本（Err = DO 不可用，调用方回退空列表）。
pub async fn load_all_skills(env: &Env) -> Result<Vec<crate::channels::Skill>> {
    let mut resp = store_rpc(env, Method::Get, "/skills", None).await?;
    let text = resp.text().await?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    Ok(v
        .get("skills")
        .and_then(|c| serde_json::from_value::<Vec<crate::channels::Skill>>(c.clone()).ok())
        .unwrap_or_default())
}

/// 全量覆盖 skill 账本。
pub async fn save_all_skills(env: &Env, list: &[crate::channels::Skill]) -> Result<()> {
    let body = serde_json::to_string(list)
        .map_err(|e| Error::RustError(format!("serialize skills: {e}")))?;
    store_rpc(env, Method::Put, "/skills", Some(&body)).await?;
    Ok(())
}

/// 追加一条路由/降权事件。
pub async fn push_event(env: &Env, ev: &RouteEvent) {
    let body = match serde_json::to_string(ev) {
        Ok(b) => b,
        Err(_) => return,
    };
    if let Err(e) = store_rpc(env, Method::Post, "/events", Some(&body)).await {
        worker::console_error!("push_event failed: {e:?}");
    }
}

/// 读取事件账本（失败返回空列表）。
pub async fn load_events(env: &Env) -> Vec<RouteEvent> {
    match store_rpc(env, Method::Get, "/events", None).await {
        Ok(resp) => {
            let mut resp = resp;
            resp.text()
                .await
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| serde_json::from_value::<Vec<RouteEvent>>(v.get("events")?.clone()).ok())
                .unwrap_or_default()
        }
        Err(e) => {
            worker::console_error!("load_events failed: {e:?}");
            Vec::new()
        }
    }
}

/// 追加一条用量成本记录（每请求一条）。失败只记日志，不影响主链路。
pub async fn push_usage(env: &Env, rec: &UsageRecord) {
    let body = match serde_json::to_string(rec) {
        Ok(b) => b,
        Err(_) => return,
    };
    if let Err(e) = store_rpc(env, Method::Post, "/usage", Some(&body)).await {
        worker::console_error!("push_usage failed: {e:?}");
    }
}

/// 读取用量成本账本（失败返回空列表）。
pub async fn load_usage(env: &Env) -> Vec<UsageRecord> {
    match store_rpc(env, Method::Get, "/usage", None).await {
        Ok(resp) => {
            let mut resp = resp;
            resp.text()
                .await
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| serde_json::from_value::<Vec<UsageRecord>>(v.get("usage")?.clone()).ok())
                .unwrap_or_default()
        }
        Err(e) => {
            worker::console_error!("load_usage failed: {e:?}");
            Vec::new()
        }
    }
}
