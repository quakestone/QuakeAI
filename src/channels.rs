//! 资源（渠道）账本：Durable Object 持久化 + 国内优先的预设目录 + 掩码与降权冷却。
//!
//! 为什么用 Durable Object 而不是 KV：本地 workerd 的 KV namespace 绑定要求外部
//! HTTP 服务（kvNamespace 是 ServiceDesignator），本地无法开箱测试；而 DO 在
//! workerd/生产 Cloudflare Workers 上均原生支持，天然串行化（无锁写），还能存降权
//! 事件账本。Rust 侧通过 `stub.fetch_with_request` 与 DO 通信，协议见 admin.rs。


use serde::{Deserialize, Serialize};

/// DO 绑定名与单例名：所有渠道数据存在同一个 DO 实例里（全局账本）。
pub const DO_BINDING: &str = "RESOURCE_STORE";
pub const DO_NAME: &str = "global-channel-ledger";
// 下面三个常量镜像 scripts/storage-do.js 里 DO 侧的存储键名/上限，
// 作为协议文档保留（Rust 侧走 /channels、/events 路径，不直接引用字面量）。
#[allow(dead_code)]
pub const STORAGE_CHANNELS: &str = "channels";
#[allow(dead_code)]
pub const STORAGE_EVENTS: &str = "events";
/// 事件账本上限（环形）：DO storage 里只保留最近这么多次降权/切换记录。
#[allow(dead_code)]
pub const EVENT_LIMIT: usize = 50;

// ---------------------------------------------------------------------------
// 数据模型
// ---------------------------------------------------------------------------

/// 渠道健康状态（管理页展示 + 路由过滤的依据）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    /// unknown | healthy | degraded | down
    pub status: String,
    pub latency_ms: Option<u64>,
    /// 最近一次探测/使用时间（ISO 字符串，JS 侧 Date.toISOString()）
    pub last_check: Option<String>,
    pub last_error: Option<String>,
    /// 降权冷却截止（JS Date.now() 毫秒 epoch）：429/限流时写入，未过期前不参与路由
    pub cooldown_until_ms: Option<f64>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            status: "unknown".into(),
            latency_ms: None,
            last_check: None,
            last_error: None,
            cooldown_until_ms: None,
        }
    }
}

/// 一个可切换资源单元：Provider × Base URL（+ 该资源登记的 API Key）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    /// zhipu | siliconflow | moonshot | bailian | deepseek | ...
    pub provider: String,
    pub name: String,
    /// chat | audio
    pub kind: String,
    pub base_url: String,
    /// 在管理页登记/轮换；明文只存在于 DO storage，永不出现在 API 响应里
    pub api_key: String,
    pub models: Vec<String>,
    /// T0 | T1 | T2（“先选能力强的”的排序基础）
    pub tier: String,
    /// 大者优先
    pub priority: i64,
    pub enabled: bool,
    pub note: String,
    #[serde(default)]
    pub health: Health,
    pub created_at: String,
    pub updated_at: String,
}

impl Channel {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }
    /// 冷却中？（无定时器依赖：直接比较 JS 时钟）
    pub fn cooling(&self, now_ms: f64) -> bool {
        self.health
            .cooldown_until_ms
            .map(|until| now_ms < until)
            .unwrap_or(false)
    }
}

/// 降权/切换事件账本条目。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteEvent {
    pub ts: String,
    /// chat | audio
    pub scope: String,
    pub model: String,
    /// 形如 "ch-a=HTTP429;ch-b=ok"
    pub attempts: Vec<String>,
    pub channel: String,
    pub provider: String,
    /// ok | degraded | failed
    pub result: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// API Key 掩码：登记/更新/删除都只把掩码给前端
// ---------------------------------------------------------------------------

pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.is_empty() {
        return "未登记".into();
    }
    if chars.len() <= 8 {
        return "****".into();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}***{tail}")
}

/// 把 `api_key` 掩码化后的渠道副本（用于所有返回前端的响应）。
pub fn masked_channels(list: &[Channel]) -> Vec<serde_json::Value> {
    list.iter()
        .map(|ch| {
            let mut v = serde_json::to_value(ch).unwrap_or(serde_json::Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "api_key_masked".into(),
                    serde_json::Value::String(mask_key(&ch.api_key)),
                );
                obj.insert("api_key".into(), serde_json::Value::String(String::new()));
            }
            v
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 预设资源目录（国内优先）
// ---------------------------------------------------------------------------

pub struct Preset {
    pub provider: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
    pub tier: &'static str,
    pub kind: &'static str,
    /// CN（国内优先） | OVERSEAS
    pub region: &'static str,
    pub free_tier: &'static str,
    pub doc: &'static str,
}

pub const PRESETS: &[Preset] = &[
    // ---- 国内优先 ----
    Preset { provider: "zhipu", name: "智谱 BigModel", base_url: "https://open.bigmodel.cn/api/paas/v4", models: &["glm-4-flash", "glm-4.7-flash", "glm-4-plus", "glm-4-voice"], tier: "T0", kind: "chat", region: "CN", free_tier: "新用户注册送 2,000 万 Token", doc: "https://open.bigmodel.cn/" },
    Preset { provider: "siliconflow", name: "硅基流动 SiliconFlow", base_url: "https://api.siliconflow.cn/v1", models: &["Qwen/Qwen2.5-7B-Instruct", "Qwen/Qwen3-8B", "deepseek-ai/DeepSeek-R1-Distill-Qwen-7B"], tier: "T2", kind: "chat", region: "CN", free_tier: "注册实名送 16 元；≤10B 参数模型 0 元永久免费", doc: "https://siliconflow.cn/" },
    Preset { provider: "moonshot", name: "Moonshot Kimi", base_url: "https://api.moonshot.cn/v1", models: &["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"], tier: "T1", kind: "chat", region: "CN", free_tier: "免费版 1,000 次/月；注册送 15 元", doc: "https://platform.moonshot.cn/" },
    Preset { provider: "bailian", name: "阿里百炼 DashScope", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: &["qwen-turbo", "qwen-plus", "qwen-max", "qwen-audio"], tier: "T0", kind: "chat", region: "CN", free_tier: "新用户 90 天内每模型 100 万 Token", doc: "https://bailian.console.aliyun.com/" },
    Preset { provider: "deepseek", name: "DeepSeek", base_url: "https://api.deepseek.com/v1", models: &["deepseek-chat", "deepseek-reasoner"], tier: "T1", kind: "chat", region: "CN", free_tier: "注册赠额；按量低价计费", doc: "https://platform.deepseek.com/" },
    Preset { provider: "minimax", name: "MiniMax", base_url: "https://api.minimaxi.com/v1", models: &["MiniMax-Text-01"], tier: "T2", kind: "chat", region: "CN", free_tier: "MiniMax-M3 约 20 RPM / 100 万 TPM", doc: "https://platform.minimaxi.com/" },
    Preset { provider: "hunyuan", name: "腾讯混元", base_url: "https://api.hunyuan.cloud.tencent.com/v1", models: &["hunyuan-lite", "hunyuan-standard"], tier: "T2", kind: "chat", region: "CN", free_tier: "个人开发者每月 100 万 Token", doc: "https://cloud.tencent.com/product/hunyuan" },
    Preset { provider: "qianfan", name: "百度千帆 AppBuilder", base_url: "https://qianfan.baidubce.com/v2", models: &["ernie-speed", "ernie-lite"], tier: "T2", kind: "chat", region: "CN", free_tier: "ERNIE-Speed/Lite 长期免费，QPS ≤ 50", doc: "https://cloud.baidu.com/product/qianfan.html" },
    Preset { provider: "volces", name: "火山方舟 豆包", base_url: "https://ark.cn-beijing.volces.com/api/v3", models: &["doubao-lite"], tier: "T2", kind: "chat", region: "CN", free_tier: "火山引擎每月赠 100 万 Token", doc: "https://www.volcengine.com/product/doubao" },
    Preset { provider: "spark", name: "讯飞星火 Spark", base_url: "https://spark-api.xf-yun.com/v1", models: &["4.0Ultra"], tier: "T2", kind: "chat", region: "CN", free_tier: "注册赠免费额度", doc: "https://xinghuo.xfyun.cn/" },
    Preset { provider: "zhipu-audio", name: "智谱音频（GLM-4-Voice）", base_url: "https://open.bigmodel.cn/api/paas/v4", models: &["glm-4-voice"], tier: "T2", kind: "audio", region: "CN", free_tier: "语音转写端点 /audio/transcriptions", doc: "https://open.bigmodel.cn/" },
    // ---- 海外补充（低优先，边缘部署时启用） ----
    Preset { provider: "openrouter", name: "OpenRouter", base_url: "https://openrouter.ai/api/v1", models: &[":free"], tier: "T0", kind: "chat", region: "OVERSEAS", free_tier: "免费模型 50 次/天", doc: "https://openrouter.ai/" },
    Preset { provider: "groq", name: "Groq", base_url: "https://api.groq.com/openai/v1", models: &["llama-3.1-8b-instant"], tier: "T1", kind: "chat", region: "OVERSEAS", free_tier: "免费层限速", doc: "https://groq.com/" },
    Preset { provider: "nvidia", name: "NVIDIA NIM", base_url: "https://integrate.api.nvidia.com/v1", models: &["nemotron"], tier: "T0", kind: "chat", region: "OVERSEAS", free_tier: "注册赠额度", doc: "https://build.nvidia.com/" },
];

/// "一键载入国内默认"：智谱主线 + 硅基流动，按国内优先排序。
/// Key 留空、enabled=false —— 登记 Key 后才参与路由（避免空 Key 打爆上游）。
pub fn default_cn_channels(now_iso: &str) -> Vec<Channel> {
    let mk = |provider: &str, name: &str, url: &str, models: &[&str], tier: &str, kind: &str, priority: i64, note: &str| Channel {
        id: format!("seed-{provider}-{kind}"),
        provider: provider.into(),
        name: name.into(),
        kind: kind.into(),
        base_url: url.into(),
        api_key: String::new(),
        models: models.iter().map(|s| s.to_string()).collect(),
        tier: tier.into(),
        priority,
        enabled: false,
        note: note.into(),
        health: Health::default(),
        created_at: now_iso.into(),
        updated_at: now_iso.into(),
    };
    vec![
        mk("zhipu", "智谱 GLM 主线", "https://open.bigmodel.cn/api/paas/v4",
           &["glm-4-flash", "glm-4.7-flash", "glm-4-plus"], "T0", "chat", 100,
           "国内 T0 主力；新用户 2000 万 Token。登记 Key 后启用。"),
        mk("zhipu", "智谱音频 GLM-4-Voice", "https://open.bigmodel.cn/api/paas/v4",
           &["glm-4-voice"], "T2", "audio", 80,
           "音频转写；与主线同 Key。登记 Key 后启用。"),
        mk("siliconflow", "硅基流动 SiliconFlow", "https://api.siliconflow.cn/v1",
           &["Qwen/Qwen2.5-7B-Instruct", "Qwen/Qwen3-8B", "deepseek-ai/DeepSeek-R1-Distill-Qwen-7B"], "T2", "chat", 90,
           "国内 OpenAI 兼容；≤10B 模型 0 元永久免费。登记 Key 后启用。"),
    ]
}

/// 静态回退通道（DO 不可用/未登记时）：从 ZHIPU_CHANNELS JSON 或默认 Base URL 生成，
/// API Key 取 ZHIPU_API_KEY 绑定 —— 保持老行为不变。
pub fn static_channels(static_urls: &[(String, String)], kind: &str, api_key: String) -> Vec<Channel> {
    static_urls
        .iter()
        .map(|(name, url)| Channel {
            id: format!("static-{name}"),
            provider: "zhipu".into(),
            name: name.clone(),
            kind: kind.into(),
            base_url: url.clone(),
            api_key: api_key.clone(),
            models: vec![],
            tier: "T0".into(),
            priority: 0,
            enabled: true,
            note: "static-fallback (ZHIPU_CHANNELS binding)".into(),
            health: Health::default(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}
