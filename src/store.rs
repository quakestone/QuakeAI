//! 资源账本的持久层：浏览器 localStorage（单机单浏览器持久化）。
//!
//! 替代原 Cloudflare Durable Object 方案——彻底脱离 CF，应用生命周期独立于服务端。
//! 数据规模小（渠道 / skill / 事件 / 用量均为本地 JSON），localStorage 5MB 配额绰绰有余。
//!
//! 注意：本模块运行在浏览器端（window）时 localStorage 可用；为兼容现有调用点，
//! 函数签名仍保留 `env` 参数但不再使用（CF 绑定已废弃），后续阶段会统一去除。

use crate::{Env, Result};
use wasm_bindgen::prelude::*;
use crate::channels::*;

const K_CH: &str = "gw_channels";
const K_SK: &str = "gw_skills";
const K_EV: &str = "gw_events";
const K_US: &str = "gw_usage";

/// 取浏览器 localStorage（仅在 window 上下文返回 Some；worker 上下文 None → 降级空）。
fn ls() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

fn read_json<T: serde::de::DeserializeOwned>(key: &str) -> Vec<T> {
    match ls().and_then(|s| s.get(key).ok().flatten()) {
        Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn write_json<T: serde::Serialize>(key: &str, v: &[T]) {
    if let Some(ls) = ls() {
        let _ = ls.set(key, &serde_json::to_string(v).unwrap_or_default());
    }
}

/// 读取完整渠道账本（保持签名兼容，env 不再使用）。
/// 出口处做一次「语音端点模型」零迁移：老账本音频渠道登记的是
/// glm-4-voice（端到端语音模型，转写/合成端点都不认），按能力改成
/// glm-asr-2512（stt）/ glm-tts（tts）——只改内存态，不回写，显示层另行提示。
pub async fn load_all_channels(_env: &Env) -> Result<Vec<Channel>> {
    let mut list: Vec<Channel> = read_json(K_CH);
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
pub async fn save_all_channels(_env: &Env, list: &[Channel]) -> Result<()> {
    write_json(K_CH, list);
    Ok(())
}

/// 读取完整 skill 账本。
pub async fn load_all_skills(_env: &Env) -> Result<Vec<Skill>> {
    Ok(read_json(K_SK))
}

/// 全量覆盖 skill 账本。
pub async fn save_all_skills(_env: &Env, list: &[Skill]) -> Result<()> {
    write_json(K_SK, list);
    Ok(())
}

/// 追加一条路由 / 降权事件（滚动上限 200 条）。
pub async fn push_event(_env: &Env, ev: &RouteEvent) {
    let mut v: Vec<RouteEvent> = read_json(K_EV);
    v.push(ev.clone());
    if v.len() > 200 {
        let excess = v.len() - 200;
        v.drain(0..excess);
    }
    write_json(K_EV, &v);
}

/// 读取事件账本。
pub async fn load_events(_env: &Env) -> Vec<RouteEvent> {
    read_json(K_EV)
}

/// 追加一条用量成本记录（每请求一条，滚动上限 2000 条）。
pub async fn push_usage(_env: &Env, rec: &UsageRecord) {
    let mut v: Vec<UsageRecord> = read_json(K_US);
    v.push(rec.clone());
    if v.len() > 2000 {
        let excess = v.len() - 2000;
        v.drain(0..excess);
    }
    write_json(K_US, &v);
}

/// 读取用量成本账本。
pub async fn load_usage(_env: &Env) -> Vec<UsageRecord> {
    read_json(K_US)
}
