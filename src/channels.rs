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
/// skill 账本存储键（镜像 storage-do.js 的 /skills 路径）。
#[allow(dead_code)]
pub const STORAGE_SKILLS: &str = "skills";
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
    /// 连续失败次数：模型引擎据此加速冷却（失败越多冷却越久）
    #[serde(default)]
    pub fail_count: u32,
}

impl Health {
    /// 冷却中？（直接比较 JS 时钟，无定时器依赖）
    pub fn cooling(&self, now_ms: f64) -> bool {
        self.cooldown_until_ms.map(|until| now_ms < until).unwrap_or(false)
    }
}

impl Default for Health {
    fn default() -> Self {
        Self {
            status: "unknown".into(),
            latency_ms: None,
            last_check: None,
            last_error: None,
            cooldown_until_ms: None,
            fail_count: 0,
        }
    }
}

/// 模型条目：渠道内的单个模型，带自己的能力分类与独立健康状态。
/// 模型是路由/降权的一等公民——某个模型出错只冷却它自己，
/// 同渠道其他模型与其他渠道的同能力模型继续可路由（模型级自动更换）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelMeta {
    pub name: String,
    /// text（文本对话） | stt（语音转写） | vision（图片/视频理解） | tts（语音合成/播音）
    #[serde(default = "default_kind_text")]
    pub kind: String,
    /// **次要能力**（可空）。多模态模型常常「既懂图又能聊天」——DeepSeek
    /// `deepseek-flash` 原生支持 image 输入同时是主力文本模型，单个 `kind` 表达不了。
    /// 填了它，该模型在匹配 `alt_kind` 能力时同样进候选池，但**排序仍以 `kind` 为主**，
    /// 所以纯文本请求不会因为某视觉模型 priority 高就抢走本该给文本模型的流量。
    #[serde(default)]
    pub alt_kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 模型级优先级（同渠道内排序用；跨渠道由渠道 priority 主导）
    #[serde(default = "default_model_prio")]
    pub priority: i64,
    #[serde(default)]
    pub health: Health,
    // ---- 单价档案（用量成本账本用；0 = 免费/未知，不计价）----
    /// 每百万 token 价：输入·缓存命中（如 DeepSeek 前缀缓存命中价）
    #[serde(default)]
    pub price_in_hit: f64,
    /// 每百万 token 价：输入·缓存未命中（普通输入价）
    #[serde(default)]
    pub price_in_miss: f64,
    /// 每百万 token 价：输出
    #[serde(default)]
    pub price_out: f64,
    /// 计价币种：CNY / USD / ""（空 = 该模型不计价）
    #[serde(default)]
    pub currency: String,
}

impl ModelMeta {
    /// 该模型是否带可用的单价档案（能折算成本）。
    pub fn priced(&self) -> bool {
        self.price_in_miss > 0.0 || self.price_out > 0.0 || self.price_in_hit > 0.0
    }
    /// 计价币种（未标注时按"免费"处理，返回空串）。
    pub fn currency(&self) -> &str {
        self.currency.as_str()
    }
    /// 是否是该能力的候选（主能力 `kind` 或次要能力 `alt_kind` 命中）。
    /// **只判"能不能用"**；排序另由 `primary_rank` 区分主/次，避免高优先级视觉模型
    /// 抢走本该给文本模型的纯文本流量。
    pub fn serves(&self, want: &str) -> bool {
        self.kind == want || (!self.alt_kind.is_empty() && self.alt_kind == want)
    }
    /// 主能力命中记 1（排序在前），次要能力命中记 0（仅兜底）。
    pub fn primary_rank(&self, want: &str) -> u8 {
        if self.kind == want {
            1
        } else {
            0
        }
    }
}

fn default_kind_text() -> String {
    "text".into()
}
fn default_true() -> bool {
    true
}
fn default_model_prio() -> i64 {
    50
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
    /// 能力标签：vision（视觉/图片输入）/ audio（音频转写）等。
    /// 模型选择引擎按请求能力匹配渠道；`#[serde(default)]` 保证旧账本数据
    /// （无此字段）反序列化不失败。
    #[serde(default)]
    pub caps: Vec<String>,
    /// 结构化模型清单（空 = 由 models + kind 推导，兼容旧账本数据）。
    /// 每个模型带自己的能力（text/stt/vision/tts）与独立健康状态。
    #[serde(default)]
    pub models_meta: Vec<ModelMeta>,
    #[serde(default)]
    pub health: Health,
    pub created_at: String,
    pub updated_at: String,
}

/// skill 子系统的一等对象：与模型完全独立——模型登记「名字 + 能力」，
/// skill 登记「标题 + 提示词正文」。模型选择器与管理页共用同一份 skill 列表。
/// `used_count` 累计被应用到对话的次数，决定列表里「使用最频繁的前 6 条」。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    /// 列表只展示这一行标题（不展示提示词正文）
    pub title: String,
    /// 提示词正文：被选中并发送时，作为 system 消息注入本轮对话（独立表达线）
    pub prompt: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 使用次数（被应用到对话的累计次数），缺省 0
    #[serde(default)]
    pub used_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// 模型名是否支持视觉（图片输入）。覆盖主流国内免费资源的视觉模型命名：
/// 智谱 glm-4v / glm-4.1v 系列、Qwen-VL 系列、以及名字里带 vision 的。
pub fn model_supports_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("glm-4v") || m.contains("glm-4.1v") || m.contains("-vl") || m.contains("vision")
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
    /// 生效的模型清单：models_meta 非空则用之（管理页精细配置的结果）；
    /// 否则由 models 字符串列表推导——audio 渠道 → stt，chat 渠道里
    /// 视觉模型名 → vision、其余 → text。旧账本零迁移。
    /// 两者皆空（如只配了 base_url 的精简 ZHIPU_CHANNELS 渠道）→ 回退到
    /// 该渠道类型的默认模型，保证引擎仍能路由（兼容 v0.6 旧配置）。
    pub fn effective_models(&self) -> Vec<ModelMeta> {
        if !self.models_meta.is_empty() {
            return self.models_meta.clone();
        }
        if self.models.is_empty() {
            let (name, kind) = if self.kind == "audio" {
                ("glm-asr-2512".to_string(), "stt".to_string())
            } else {
                ("glm-4-flash".to_string(), "text".to_string())
            };
            return vec![ModelMeta {
                alt_kind: String::new(),
                name,
                kind,
                enabled: true,
                priority: default_model_prio(),
                health: Health::default(),
                price_in_hit: 0.0,
                price_in_miss: 0.0,
                price_out: 0.0,
                currency: String::new(),
            }];
        }
        self.models
            .iter()
            .map(|name| {
                // 音频渠道按模型名分能力：*-tts* → 播音，其余 → 转写
                // （智谱转写用 glm-asr-2512、合成用 glm-tts，分属两个端点模型）
                let kind = if self.kind == "audio" {
                    if name.to_lowercase().contains("tts") {
                        "tts".to_string()
                    } else {
                        "stt".to_string()
                    }
                } else if model_supports_vision(name) {
                    "vision".to_string()
                } else {
                    "text".to_string()
                };
                ModelMeta {
                    alt_kind: String::new(),
                    name: name.clone(),
                    kind,
                    enabled: true,
                    priority: default_model_prio(),
                    health: Health::default(),
                    price_in_hit: 0.0,
                    price_in_miss: 0.0,
                    price_out: 0.0,
                    currency: String::new(),
                }
            })
            .collect()
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
// 用量成本账本：每请求一条（源 × 模型 × token × 延迟 × 成本）
// ---------------------------------------------------------------------------

/// 一次上游调用的用量记录。网关此前完全不记 token/cost，
/// 导致「本地统计」与「供应商官方账单」无法对账；本条目补上这一层。
///
/// 成本由模型的单价档案（ModelMeta.price_*）折算：
/// 输入按 DeepSeek 式的两段计费（缓存命中 hit / 未命中 miss），输出按 price_out。
/// 上游未回 usage 时 tokens 全 0，但仍记 requests/延迟/状态（成功率可算，成本不可算）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    /// ISO 时间戳
    pub ts: String,
    /// 自然日（YYYY-MM-DD），聚合与对账的主键维度之一
    pub day: String,
    /// chat | stt | tts
    pub scope: String,
    pub provider: String,
    pub channel_id: String,
    pub channel_name: String,
    pub model: String,
    #[serde(default)]
    pub prompt_tokens: u64,
    /// 缓存命中的输入 token（DeepSeek 单独计价，故单列）
    #[serde(default)]
    pub prompt_cache_hit_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub latency_ms: u64,
    /// ok | failed
    pub status: String,
    /// 按单价档案折算的成本（0 = 免费模型或无单价档案）
    #[serde(default)]
    pub cost: f64,
    /// 计价币种（空 = 不计价）
    #[serde(default)]
    pub currency: String,
    /// 成本是否为「按标准价估算」——供应商若有峰谷分时计价，官方账单会与估算有差
    #[serde(default)]
    pub estimated: bool,
}

impl UsageRecord {
    /// 自然日（UTC）。JS 侧 Date.toISOString() 形如 2026-10-20T06:00:00.000Z，
    /// 取前 10 段即 YYYY-MM-DD。
    pub fn day_of(ts_iso: &str) -> String {
        ts_iso.chars().take(10).collect()
    }
    /// 成本是否可信（有 token 且有单价档案）。
    pub fn has_cost(&self) -> bool {
        self.total_tokens > 0 && self.cost > 0.0
    }
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
    Preset { provider: "deepseek", name: "DeepSeek", base_url: "https://api.deepseek.com/v1", models: &["deepseek-flash", "deepseek-v4-pro"], tier: "T1", kind: "chat", region: "CN", free_tier: "注册赠额；按量低价计费（Flash 输入未命中 ¥1/百万 token、输出 ¥2）", doc: "https://api-docs.deepseek.com/quick_start/pricing" },
    Preset { provider: "minimax", name: "MiniMax", base_url: "https://api.minimaxi.com/v1", models: &["MiniMax-Text-01"], tier: "T2", kind: "chat", region: "CN", free_tier: "MiniMax-M3 约 20 RPM / 100 万 TPM", doc: "https://platform.minimaxi.com/" },
    Preset { provider: "hunyuan", name: "腾讯混元", base_url: "https://api.hunyuan.cloud.tencent.com/v1", models: &["hunyuan-lite", "hunyuan-standard"], tier: "T2", kind: "chat", region: "CN", free_tier: "个人开发者每月 100 万 Token", doc: "https://cloud.tencent.com/product/hunyuan" },
    Preset { provider: "qianfan", name: "百度千帆 AppBuilder", base_url: "https://qianfan.baidubce.com/v2", models: &["ernie-speed", "ernie-lite"], tier: "T2", kind: "chat", region: "CN", free_tier: "ERNIE-Speed/Lite 长期免费，QPS ≤ 50", doc: "https://cloud.baidu.com/product/qianfan.html" },
    Preset { provider: "volces", name: "火山方舟 豆包", base_url: "https://ark.cn-beijing.volces.com/api/v3", models: &["doubao-lite"], tier: "T2", kind: "chat", region: "CN", free_tier: "火山引擎每月赠 100 万 Token", doc: "https://www.volcengine.com/product/doubao" },
    Preset { provider: "spark", name: "讯飞星火 Spark", base_url: "https://spark-api.xf-yun.com/v1", models: &["4.0Ultra"], tier: "T2", kind: "chat", region: "CN", free_tier: "注册赠免费额度", doc: "https://xinghuo.xfyun.cn/" },
    Preset { provider: "zhipu-audio", name: "智谱语音（GLM-ASR / GLM-TTS）", base_url: "https://open.bigmodel.cn/api/paas/v4", models: &["glm-asr-2512", "glm-tts"], tier: "T2", kind: "audio", region: "CN", free_tier: "语音转写 /audio/transcriptions + 播音 /audio/speech", doc: "https://open.bigmodel.cn/" },
    // ---- 海外补充（低优先，边缘部署时启用） ----
    Preset { provider: "openrouter", name: "OpenRouter", base_url: "https://openrouter.ai/api/v1", models: &["openrouter/free", "google/gemma-4-31b-it:free", "google/gemma-4-26b-a4b-it:free", "qwen/qwen3.8-27b:free", "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free", "thinkingmachines/inkling:free", "nvidia/nemotron-3-super-120b-a12b:free", "cohere/north-mini-code:free"], tier: "T0", kind: "chat", region: "OVERSEAS", free_tier: "20+ 个 :free 免卡模型（多数 50/天，部分 1000/天），含 7 个免费视觉/多模态；避 poolside 10-31 到期", doc: "https://openrouter.ai/" },
    Preset { provider: "groq", name: "Groq", base_url: "https://api.groq.com/openai/v1", models: &["llama-3.3-70b-versatile", "llama-3.1-8b-instant", "llama-3.2-11b-vision-preview"], tier: "T1", kind: "chat", region: "OVERSEAS", free_tier: "免费层 14400 RPD / 30 RPM 免卡", doc: "https://groq.com/" },
    Preset { provider: "nvidia", name: "NVIDIA NIM", base_url: "https://integrate.api.nvidia.com/v1", models: &["nvidia/nemotron-3-ultra-550b-a55b", "nvidia/nemotron-3.5-super-vl-120b-a12b", "deepseek-ai/deepseek-v4.1-flash", "meta/llama-3.3-70b-instruct"], tier: "T0", kind: "chat", region: "OVERSEAS", free_tier: "约 40 RPM 免卡，100+ 模型", doc: "https://build.nvidia.com/" },
];

/// "一键载入国内默认"：智谱主线 + 硅基流动，按国内优先排序。
/// Key 留空、enabled=false —— 登记 Key 后才参与路由（避免空 Key 打爆上游）。
/// models_meta 预填四类能力（text/vision/stt/tts），模型选择引擎据此路由；
/// models 里带上各自的免费模型（glm-4v-flash 视觉 / glm-4-voice 语音 / Qwen2.5-VL 视觉）。
pub fn default_cn_channels(now_iso: &str) -> Vec<Channel> {
    let mk = |provider: &str, name: &str, url: &str, models: &[&str], tier: &str, kind: &str, priority: i64, note: &str, caps: &[&str], meta: Vec<ModelMeta>| Channel {
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
        caps: caps.iter().map(|s| s.to_string()).collect(),
        models_meta: meta,
        health: Health::default(),
        created_at: now_iso.into(),
        updated_at: now_iso.into(),
    };
    let mm = |name: &str, kind: &str, prio: i64| ModelMeta {
        alt_kind: String::new(),
        name: name.into(),
        kind: kind.into(),
        enabled: true,
        priority: prio,
        health: Health::default(),
        price_in_hit: 0.0,
        price_in_miss: 0.0,
        price_out: 0.0,
        currency: String::new(),
    };
    vec![
        mk("zhipu", "智谱 GLM 主线", "https://open.bigmodel.cn/api/paas/v4",
           &["glm-4-flash", "glm-4.7-flash", "glm-4-plus", "glm-4v-flash"], "T0", "chat", 100,
           "国内 T0 主力；新用户 2000 万 Token。登记 Key 后启用。",
           &["text", "vision"],
           vec![
               mm("glm-4-flash", "text", 100),
               mm("glm-4.7-flash", "text", 90),
               mm("glm-4-plus", "text", 80),
               mm("glm-4v-flash", "vision", 70),
           ]),
        mk("zhipu", "智谱语音（GLM-ASR / GLM-TTS）", "https://open.bigmodel.cn/api/paas/v4",
           &["glm-asr-2512", "glm-tts"], "T2", "audio", 80,
           "语音转写(glm-asr-2512)/播音(glm-tts)；与主线同 Key（引擎会自动复用同 provider 主线 Key）。登记 Key 后启用。",
           &["stt", "tts"],
           vec![
               mm("glm-asr-2512", "stt", 100),
               mm("glm-tts", "tts", 100),
           ]),
        mk("siliconflow", "硅基流动 SiliconFlow", "https://api.siliconflow.cn/v1",
           &["Qwen/Qwen2.5-7B-Instruct", "Qwen/Qwen3-8B", "deepseek-ai/DeepSeek-R1-Distill-Qwen-7B", "Qwen/Qwen2.5-VL-7B-Instruct"], "T2", "chat", 90,
           "国内 OpenAI 兼容；≤10B 模型 0 元永久免费（含 Qwen2.5-VL 视觉模型）。登记 Key 后启用。",
           &["text", "vision"],
           vec![
               mm("Qwen/Qwen2.5-7B-Instruct", "text", 100),
               mm("Qwen/Qwen3-8B", "text", 90),
               mm("deepseek-ai/DeepSeek-R1-Distill-Qwen-7B", "text", 80),
               mm("Qwen/Qwen2.5-VL-7B-Instruct", "vision", 70),
           ]),
    ]
}

/// "一键载入" 的 DeepSeek 渠道（CN 付费主力之一）。
/// 模型名已是现行代（`deepseek-chat`/`deepseek-reasoner` 已于 2026-07-24 弃用，
/// 官方仍接受但分别对应 v4-flash 的非思考/思考模式）。
/// 单价档案按官方价（人民币/百万 token）：输入分「缓存命中/未命中」两段计费，
/// 输出单独计价——网关据此把每次调用折算成成本，再与 DeepSeek 官方账单导出对账。
/// enabled=false、优先级 70（低于智谱 100 / 硅基流动 90 / 智谱音频 80，高于海外 40/35/30）。
pub fn default_deepseek_channels(now_iso: &str) -> Vec<Channel> {
    let mm = |name: &str, kind: &str, alt: &str, prio: i64, hit: f64, miss: f64, out: f64| ModelMeta {
        alt_kind: alt.into(),
        name: name.into(),
        kind: kind.into(),
        enabled: true,
        priority: prio,
        health: Health::default(),
        price_in_hit: hit,
        price_in_miss: miss,
        price_out: out,
        currency: "CNY".into(),
    };
    vec![Channel {
        id: "seed-deepseek-chat".into(),
        provider: "deepseek".into(),
        name: "DeepSeek（国内主力 · 带计价）".into(),
        kind: "chat".into(),
        base_url: "https://api.deepseek.com/v1".into(),
        api_key: String::new(),
        models: vec!["deepseek-flash".into(), "deepseek-v4-pro".into()],
        tier: "T1".into(),
        priority: 70,
        enabled: false,
        note: "国内主力付费渠道：1M 上下文、OpenAI 兼容。deepseek-flash 为主力低价模型且原生支持 image 输入（text/vision 双能力，纯文本与看图都走它），deepseek-v4-pro 纯文本旗舰。内置单价档案（CNY/百万 token，命中/未命中两段输入计价），每次调用自动折算成本并可导出对齐官方 cost-*.csv 的账单做对账。登记 Key 后启用。".into(),
        caps: vec!["text".into(), "vision".into()],
        models_meta: vec![
            // deepseek-flash 主能力 text（它是 DeepSeek 的主力低价模型，纯文本请求
            // 首选它），次要能力 vision（实测原生支持 image 输入）——所以图片请求
            // 也能路由到它，而纯文本请求不会被它的 alt 身份影响排序。
            mm("deepseek-flash", "text", "vision", 100, 0.02, 1.0, 2.0),
            mm("deepseek-v4-pro", "text", "", 90, 0.025, 3.0, 6.0),
        ],
        health: Health::default(),
        created_at: now_iso.into(),
        updated_at: now_iso.into(),
    }]
}

/// "一键载入" 的海外补充：OpenRouter + Groq + NVIDIA NIM。
/// enabled=false、优先级低于国内（40/35/30），作为「国内渠道全挂时的兜底」。
/// 各家免费层免卡即可用，但需 Key 且大概率需科学上网；caps 含 text+vision，
/// models_meta 标注各家视觉模型（gemma-4-31b / llama-3.2-11b-vision / nemotron-3.5-super-vl），
/// 这样当国内视觉渠道不可用时，image_url 请求可被海外视觉渠道承接。
pub fn default_overseas_channels(now_iso: &str) -> Vec<Channel> {
    let mk = |provider: &str, name: &str, url: &str, models: &[&str], tier: &str, kind: &str, priority: i64, note: &str, caps: &[&str], meta: Vec<ModelMeta>| Channel {
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
        caps: caps.iter().map(|s| s.to_string()).collect(),
        models_meta: meta,
        health: Health::default(),
        created_at: now_iso.into(),

        updated_at: now_iso.into(),
    };
    let mm = |name: &str, kind: &str, prio: i64| ModelMeta {
        alt_kind: String::new(),
        name: name.into(),
        kind: kind.into(),
        enabled: true,
        priority: prio,
        health: Health::default(),
        price_in_hit: 0.0,
        price_in_miss: 0.0,
        price_out: 0.0,
        currency: String::new(),
    };
    vec![
        mk("openrouter", "OpenRouter（海外兜底）", "https://openrouter.ai/api/v1",
           &["openrouter/free", "google/gemma-4-31b-it:free", "google/gemma-4-26b-a4b-it:free", "qwen/qwen3.8-27b:free", "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free", "thinkingmachines/inkling:free", "thinkingmachines/inkling-small:free", "nvidia/nemotron-3-super-120b-a12b:free", "cohere/north-mini-code:free"], "T0", "chat", 40,
           "海外聚合网关，20+ 个 :free 免卡模型（多数 50/天，部分 1000/天），含 7 个免费视觉/多模态模型可轮换防 429。国内全挂时兜底；需 Key + 可能需科学上网。避 poolside 10-31 到期。",
           &["text", "vision"],
           vec![
               mm("openrouter/free", "vision", 100),
               mm("google/gemma-4-31b-it:free", "vision", 95),
               mm("google/gemma-4-26b-a4b-it:free", "vision", 90),
               mm("qwen/qwen3.8-27b:free", "vision", 85),
               mm("nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free", "vision", 80),
               mm("thinkingmachines/inkling:free", "vision", 75),
               mm("thinkingmachines/inkling-small:free", "vision", 70),
               mm("nvidia/nemotron-3-super-120b-a12b:free", "text", 60),
               mm("cohere/north-mini-code:free", "text", 50),
           ]),
        mk("groq", "Groq（海外兜底）", "https://api.groq.com/openai/v1",
           &["llama-3.3-70b-versatile", "llama-3.1-8b-instant", "llama-3.2-11b-vision-preview"], "T1", "chat", 35,
           "免费层 14400 RPD / 30 RPM 免卡；极速推理。国内全挂时兜底；需 Key + 可能需科学上网。",
           &["text", "vision"],
           vec![
               mm("llama-3.3-70b-versatile", "text", 100),
               mm("llama-3.1-8b-instant", "text", 90),
               mm("llama-3.2-11b-vision-preview", "vision", 70),
           ]),
        mk("nvidia", "NVIDIA NIM（海外兜底）", "https://integrate.api.nvidia.com/v1",
           &["nvidia/nemotron-3-ultra-550b-a55b", "nvidia/nemotron-3.5-super-vl-120b-a12b", "deepseek-ai/deepseek-v4.1-flash", "meta/llama-3.3-70b-instruct"], "T0", "chat", 30,
           "约 40 RPM 免卡，100+ 模型。国内全挂时兜底；需 Key + 可能需科学上网。",
           &["text", "vision"],
           vec![
               mm("nvidia/nemotron-3-ultra-550b-a55b", "text", 100),
               mm("nvidia/nemotron-3.5-super-vl-120b-a12b", "vision", 70),
               mm("deepseek-ai/deepseek-v4.1-flash", "text", 90),
               mm("meta/llama-3.3-70b-instruct", "text", 80),
           ]),
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
            caps: vec![],
            models_meta: vec![],
            health: Health::default(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}
