//! workflow-engine：浏览器侧工作流引擎内核（编译为 wasm32）。
//!
//! 设计总纲（与项目侧约定）：
//! - **内核是同步状态机**：wasm 里没有 Tokio/无线程，异步全部由 JS Host 驱动。
//! - ** Host 驱动循环**：JS 每轮调 `engine_ready` 取得就绪步 → 本地并发执行插件
//!   （fetch/IDB/定时器都在 JS 侧）→ `engine_complete` / `engine_fail` 回报 → 再取
//!   就绪步，直到 run 终态。超时与重试退避由 Host 决定，内核只数次数。
//! - **内存态，不持久化**：run 状态只在 wasm 实例存活期内有效；页面关闭即消失
//!   （侧链路工作流可承受丢失，诚实降级，不引入恢复机制）。
//! - **ABI 零导入**：内核不 import 任何宿主函数，只导出内存 + 几个读写函数，
//!   输入输出全部走「bump 竞技场（入）+ 暂存缓冲区（出）」。
//!
//! 宿主使用顺序：
//!   1. `mem_reserve(cap)` → 竞技场基址（进程内一次）
//!   2. `engine_new()` → 引擎句柄；`engine_load(h, ptr, len)` 加载工作流 JSON
//!   3. `engine_start(h, ev_ptr, ev_len)` → run_id（事件名不匹配返回 -1）
//!   4. 循环：`engine_ready(run_id)`（就绪步 JSON 写入暂存区）→ 执行 →
//!      `engine_complete(run_id, sid, out)` / `engine_fail(run_id, sid, err)`
//!   5. `engine_status(run_id)` / `engine_result(run_id)` 读终态与全部输出

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

// ---------------------------------------------------------------------------
// Schema（与后端评审版同义：JSON 驱动、插件白名单、显式依赖）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Workflow {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: u32,
    pub trigger: Trigger,
    #[serde(default = "default_concurrency")]
    #[allow(dead_code)]
    pub max_concurrency: usize,
    pub steps: Vec<Step>,
}

fn default_version() -> u32 { 1 }
fn default_concurrency() -> usize { 4 }

#[derive(Debug, Deserialize)]
pub struct Trigger {
    pub event: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    pub id: String,
    pub plugin: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub input: Value,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub retries: u32,
}

fn default_timeout() -> u64 { 5000 }

// ---------------------------------------------------------------------------
// 校验（含静态环检测——评审版"跑起来才发现环"的缺口在这里补上）
// ---------------------------------------------------------------------------

pub fn validate(wf: &Workflow) -> Result<(), String> {
    if wf.id.trim().is_empty() {
        return Err("workflow.id 不能为空".into());
    }
    let ids: HashSet<&str> = wf.steps.iter().map(|s| s.id.as_str()).collect();
    if ids.len() != wf.steps.len() {
        return Err("步骤 id 不能重复".into());
    }
    for s in &wf.steps {
        for dep in &s.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(format!("步骤 {} 依赖不存在的步骤 {}", s.id, dep));
            }
            if dep == &s.id {
                return Err(format!("步骤 {} 不能依赖自身", s.id));
            }
        }
    }
    // Kahn 拓扑排序：剩余入度非零 ⇒ 有环
    let mut indeg: HashMap<&str, usize> = wf.steps.iter().map(|s| (s.id.as_str(), 0)).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for s in &wf.steps {
        for dep in &s.depends_on {
            indeg.entry(s.id.as_str()).and_modify(|d| *d += 1);
            dependents.entry(dep.as_str()).or_default().push(s.id.as_str());
        }
    }
    let mut queue: Vec<&str> = indeg.iter().filter(|(_, &d)| d == 0).map(|(&k, _)| k).collect();
    let mut seen = 0usize;
    while let Some(n) = queue.pop() {
        seen += 1;
        if let Some(ds) = dependents.get(n) {
            for d in ds {
                let e = indeg.get_mut(*d).unwrap();
                *e -= 1;
                if *e == 0 {
                    queue.push(d);
                }
            }
        }
    }
    if seen != wf.steps.len() {
        return Err("工作流存在循环依赖".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 模板解析：`${/json/pointer}` 整串替换（serde_json::Value::pointer 白嫖）。
// 只命中整串；串内混排（"a${x}b"）不展开，取不到时落 Null —— 与评审版语义一致。
// ---------------------------------------------------------------------------

pub fn resolve_templates(value: &Value, context: &Value) -> Value {
    match value {
        Value::String(s) if s.starts_with("${") && s.ends_with('}') => {
            let pointer = &s[2..s.len() - 1];
            if pointer.is_empty() {
                return context.clone();
            }
            context.pointer(pointer).cloned().unwrap_or(Value::Null)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), resolve_templates(v, context)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| resolve_templates(v, context)).collect())
        }
        _ => value.clone(),
    }
}

// ---------------------------------------------------------------------------
// 运行时状态机（内存态）
// ---------------------------------------------------------------------------

const STEP_PENDING: u8 = 0;
const STEP_RUNNING: u8 = 2;
const STEP_DONE: u8 = 3;
const STEP_FAILED: u8 = 4;
const STEP_SKIPPED: u8 = 5;

const RUN_RUNNING: u8 = 0;
const RUN_DONE: u8 = 1;
const RUN_FAILED: u8 = 2;

#[derive(Clone)]
struct StepRt {
    id: String,
    plugin: String,
    depends_on: Vec<String>,
    input: Value, // 模板（未解析）
    #[allow(dead_code)]
    timeout_ms: u64,
    retries: u32,
    state: u8,
    attempts: u32,
    output: Option<Value>,
}

struct Run {
    status: u8,
    steps: Vec<StepRt>,
    ctx: Value, // {event, steps:{id: output}}
}

impl Run {
    fn new(wf: &Workflow, event: Value) -> Self {
        Run {
            status: RUN_RUNNING,
            steps: wf
                .steps
                .iter()
                .map(|s| StepRt {
                    id: s.id.clone(),
                    plugin: s.plugin.clone(),
                    depends_on: s.depends_on.clone(),
                    input: s.input.clone(),
                    timeout_ms: s.timeout_ms,
                    retries: s.retries,
                    state: STEP_PENDING,
                    attempts: 0,
                    output: None,
                })
                .collect(),
            ctx: json!({ "event": event, "steps": {} }),
        }
    }

    /// 收集就绪步：依赖全部 DONE 且自身 PENDING。命中即置 RUNNING 并解析输入，
    /// 同一轮不会重复返回（Host 并发执行后由 complete/fail 收敛）。
    fn take_ready(&mut self) -> Vec<(String, Value)> {
        // 先收完成集（owned，避开与 iter_mut 的双借）
        let done: HashSet<String> = self
            .steps
            .iter()
            .filter(|s| s.state == STEP_DONE)
            .map(|s| s.id.clone())
            .collect();
        let mut out = Vec::new();
        for s in self.steps.iter_mut() {
            if s.state != STEP_PENDING {
                continue;
            }
            if !s.depends_on.iter().all(|d| done.contains(d)) {
                continue;
            }
            s.state = STEP_RUNNING;
            out.push((s.id.clone(), resolve_templates(&s.input, &self.ctx)));
        }
        out
    }

    fn complete(&mut self, sid: &str, output: Value) -> Result<(), String> {
        let s = self
            .steps
            .iter_mut()
            .find(|s| s.id == sid)
            .ok_or_else(|| format!("未知步骤 {sid}"))?;
        s.state = STEP_DONE;
        s.output = Some(output.clone());
        self.ctx["steps"][sid] = output;
        self.recompute();
        Ok(())
    }

    /// 回报一次失败。返回 true = 允许重试（次数未用尽，步回 PENDING）；
    /// false = 次数用尽，步 FAILED，整个 run 置 FAILED，其余步 SKIPPED。
    fn fail(&mut self, sid: &str, _err: &str) -> Result<bool, String> {
        let s = self
            .steps
            .iter_mut()
            .find(|s| s.id == sid)
            .ok_or_else(|| format!("未知步骤 {sid}"))?;
        s.attempts += 1;
        if s.attempts <= s.retries {
            s.state = STEP_PENDING; // 回就绪池，Host 下一轮会再拿到
            return Ok(true);
        }
        s.state = STEP_FAILED;
        self.status = RUN_FAILED;
        for o in self.steps.iter_mut() {
            if o.state == STEP_PENDING || o.state == STEP_RUNNING {
                o.state = STEP_SKIPPED;
            }
        }
        Ok(false)
    }

    fn recompute(&mut self) {
        if self.status != RUN_RUNNING {
            return;
        }
        if self.steps.iter().all(|s| s.state == STEP_DONE) {
            self.status = RUN_DONE;
        }
    }

    fn status_json(&self) -> Value {
        let steps: BTreeMap<String, Value> = self
            .steps
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    json!({
                        "state": s.state,
                        "attempts": s.attempts,
                        "plugin": s.plugin,
                    }),
                )
            })
            .collect();
        json!({ "status": self.status, "steps": steps })
    }

    fn result_json(&self) -> Value {
        let outs: BTreeMap<String, Value> = self
            .steps
            .iter()
            .filter_map(|s| s.output.clone().map(|o| (s.id.clone(), o)))
            .collect();
        json!({
            "status": self.status,
            "state_name": match self.status { RUN_DONE => "done", RUN_FAILED => "failed", _ => "running" },
            "steps": outs,
        })
    }
}

// ---------------------------------------------------------------------------
// 内核主体（宿主/WASM 共用）
// ---------------------------------------------------------------------------

pub struct EngineInst {
    workflow: Workflow,
}

pub struct EngineCore {
    engines: Vec<EngineInst>,
    // run 全局注册表：run_id = 全局索引。这样多个引擎（不同工作流）并存时，
    // ready/complete/fail 只需传 run_id 即可唯一定位，ABI 无需改动。
    runs: Vec<Run>,
    last_err: String,
}

impl Default for EngineCore {
    fn default() -> Self {
        EngineCore { engines: Vec::new(), runs: Vec::new(), last_err: String::new() }
    }
}

impl EngineCore {
    pub fn new_engine(&mut self) -> i32 {
        self.engines.push(EngineInst { workflow: placeholder_wf() });
        (self.engines.len() - 1) as i32
    }

    /// 加载（或替换）工作流 JSON。句柄越界返回 -3；格式/校验错误 -1（原因见 last_err）。
    pub fn load(&mut self, handle: i32, json: &str) -> i32 {
        let idx = handle as usize;
        if idx >= self.engines.len() {
            self.last_err = format!("引擎句柄越界: {handle}");
            return -3;
        }
        let wf: Workflow = match serde_json::from_str(json) {
            Ok(w) => w,
            Err(e) => {
                self.last_err = format!("工作流 JSON 解析失败: {e}");
                return -1;
            }
        };
        if let Err(e) = validate(&wf) {
            self.last_err = e;
            return -1;
        }
        self.engines[idx].workflow = wf;
        0
    }

    /// 以事件启动一个 run。事件名不匹配 trigger 返回 -2；返回 run_id。
    pub fn start(&mut self, handle: i32, event_json: &str) -> i32 {
        let idx = handle as usize;
        if idx >= self.engines.len() {
            self.last_err = format!("引擎句柄越界: {handle}");
            return -3;
        }
        let event: Value = match serde_json::from_str(event_json) {
            Ok(v) => v,
            Err(e) => {
                self.last_err = format!("事件 JSON 解析失败: {e}");
                return -1;
            }
        };
        let ev_name = event.get("event").and_then(Value::as_str).unwrap_or("");
        if ev_name != self.engines[idx].workflow.trigger.event {
            self.last_err = format!(
                "事件不匹配：工作流监听 {}，收到 {ev_name}",
                self.engines[idx].workflow.trigger.event
            );
            return -2;
        }
        let run = Run::new(&self.engines[idx].workflow, event);
        let rid = self.runs.len() as i32;
        self.runs.push(run);
        rid
    }

    /// 取就绪步（同时置 RUNNING）。返回 JSON：{steps:[{id,input}], status, done}
    pub fn ready(&mut self, run_id: i32) -> Result<String, String> {
        let r = self.run_mut(run_id)?;
        let ready = r.take_ready();
        let st = r.status_json();
        Ok(serde_json::to_string(&json!({
            "steps": ready.into_iter().map(|(id, input)| json!({"id": id, "input": input})).collect::<Vec<_>>(),
            "status": st["status"],
        }))
        .unwrap())
    }

    pub fn complete(&mut self, run_id: i32, sid: &str, output: &str) -> Result<(), String> {
        let v: Value = if output.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(output).map_err(|e| format!("步骤 {sid} 输出不是合法 JSON: {e}"))?
        };
        self.run_mut(run_id)?.complete(sid, v)
    }

    /// 回报失败：Ok(true)=可重试；Ok(false)=次数用尽（run 已 FAILED）。
    pub fn fail(&mut self, run_id: i32, sid: &str, err: &str) -> Result<bool, String> {
        self.run_mut(run_id)?.fail(sid, err)
    }

    pub fn status(&self, run_id: i32) -> Result<String, String> {
        Ok(self.run_ref(run_id)?.status_json().to_string())
    }

    pub fn result(&self, run_id: i32) -> Result<String, String> {
        Ok(self.run_ref(run_id)?.result_json().to_string())
    }

    fn run_mut(&mut self, run_id: i32) -> Result<&mut Run, String> {
        let rid = run_id as usize;
        self.runs
            .get_mut(rid)
            .ok_or_else(|| format!("run_id 越界: {run_id}"))
    }

    fn run_ref(&self, run_id: i32) -> Result<&Run, String> {
        let rid = run_id as usize;
        self.runs
            .get(rid)
            .ok_or_else(|| format!("run_id 越界: {run_id}"))
    }
}

// ---------------------------------------------------------------------------
// 会话记忆状态机（Memory FSM）：纯数值、零 JSON、体积最小。
// 职责：根据累计 token 与消息数做状态跃迁 + 计算上下文窗口（keep_from）。
// 原始对话文本存在外部（浏览器 IndexedDB / Worker 侧 KV·D1），Wasm 只驻留
// 这个几十字节的结构体，绝不长期持有大文本 —— 天然规避 CF Worker 内存压力。
// 真压缩（语义摘要）由宿主侧 LLM 调用完成，FSM 只负责「何时压缩 + 窗口边界」。
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle = 0,        // 初始/空闲
    Collecting = 1,  // 正常收集期（保留最近 N 条）
    Summarizing = 2, // 触发长文本摘要/压缩状态
    Locked = 3,      // 会话超限锁定
}

impl SessionState {
    fn from_code(c: i32) -> SessionState {
        match c {
            1 => SessionState::Collecting,
            2 => SessionState::Summarizing,
            3 => SessionState::Locked,
            _ => SessionState::Idle,
        }
    }
    fn code(self) -> i32 { self as i32 }
}

#[derive(Clone)]
pub struct MemoryStateMachine {
    state: SessionState,
    token_count: u32,
    max_tokens: u32,
    hard_max: u32,
    keep_recent: u32,
    /// 已被折叠进摘要的历史边界（filtered 消息数组下标）。
    summary_pointer: usize,
    /// 已见消息总数（与 filtered 消息数组长度对齐，仅增）。
    msg_count: u32,
}

impl MemoryStateMachine {
    pub fn new(max_tokens: u32, hard_max: u32, keep_recent: u32) -> Self {
        MemoryStateMachine {
            state: SessionState::Idle,
            token_count: 0,
            max_tokens: max_tokens.max(1),
            hard_max: hard_max.max(max_tokens.max(1)),
            keep_recent: keep_recent.max(1),
            summary_pointer: 0,
            msg_count: 0,
        }
    }

    /// 恢复持久化状态（跨页面/会话）。
    pub fn restore(&mut self, state: i32, token_count: u32, summary_pointer: i32, msg_count: u32) {
        self.state = SessionState::from_code(state);
        self.token_count = token_count;
        self.summary_pointer = summary_pointer.max(0) as usize;
        self.msg_count = msg_count;
    }

    /// 喂入本轮增量：token 数 + 新增消息条数。返回新状态。
    pub fn transition(&mut self, incoming_tokens: u32, new_msgs: u32) -> SessionState {
        self.msg_count += new_msgs;
        self.token_count += incoming_tokens;
        if self.state == SessionState::Idle {
            self.state = SessionState::Collecting;
        }
        if self.token_count > self.hard_max {
            self.state = SessionState::Locked;
        } else if self.token_count > self.max_tokens && self.state == SessionState::Collecting {
            self.state = SessionState::Summarizing;
        }
        self.state
    }

    /// 计算「应保留的消息起始下标」：不早于已折叠边界，也不少于最近 keep_recent 条。
    pub fn window_keep_from(&self) -> usize {
        let recent_floor = if self.msg_count > self.keep_recent {
            (self.msg_count - self.keep_recent) as usize
        } else {
            0
        };
        recent_floor.max(self.summary_pointer)
    }

    pub fn need_summary(&self) -> bool { self.state == SessionState::Summarizing }

    /// 摘要完成后回报：重置 token 计数（用摘要自身 token 成本），推进边界，回到 Collecting。
    pub fn note_summary(&mut self, summary_tokens: u32, new_pointer: i32) {
        self.token_count = summary_tokens;
        self.summary_pointer = new_pointer.max(0) as usize;
        self.state = if self.token_count > self.hard_max {
            SessionState::Locked
        } else {
            SessionState::Collecting
        };
    }

    pub fn state(&self) -> SessionState { self.state }
    pub fn token_count(&self) -> u32 { self.token_count }
    pub fn summary_pointer(&self) -> usize { self.summary_pointer }
    pub fn msg_count(&self) -> u32 { self.msg_count }
}

// ===== 模型选择状态机（ModelSelFSM） =====
// 取代原来散落在网关/前端的 `model_unavailable` / `fell_back` 布尔标志，把「选了 /
// 生效 / 回落切换 / 全部失败」做成显式状态。宿主（前端）只喂两样：用户所选模型名、
// 实际采用的模型名；FSM 算出唯一状态，前端据此精准提示「你选了 X，实际用 Y，原因 Z」。
// 纯字符串/数值，Wasm 内不驻留任何对话文本，状态结构体仅几十字节。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelState {
    Auto = 0,        // 未显式选模型（网关按能力自动选路）
    Explicit = 1,    // 用户显式选了模型，待响应确认是否生效
    Honored = 2,     // 所选模型确实被采用
    Substituted = 3, // 所选未生效，实际由别的模型应答（池中切换 / 回落自动选路）
    Locked = 4,      // 所选模型全部候选失败，最终无任何模型应答
}

impl SelState {
    #[allow(dead_code)]
    fn from_code(c: i32) -> SelState {
        match c {
            1 => SelState::Explicit,
            2 => SelState::Honored,
            3 => SelState::Substituted,
            4 => SelState::Locked,
            _ => SelState::Auto,
        }
    }
    fn code(self) -> i32 { self as i32 }
    /// 人类可读状态说明（前端 toast / badge 用）。
    fn label(self) -> &'static str {
        match self {
            SelState::Auto => "自动选路",
            SelState::Explicit => "已选择·待确认",
            SelState::Honored => "已采用所选",
            SelState::Substituted => "所选未生效·改用他模",
            SelState::Locked => "所选全部失败",
        }
    }
}

pub struct ModelSelFSM {
    state: SelState,
    requested: String,
    applied: String,
}

impl ModelSelFSM {
    pub fn new() -> Self {
        ModelSelFSM { state: SelState::Auto, requested: String::new(), applied: String::new() }
    }

    /// 喂入用户所选模型（空串 = 自动选路）。
    pub fn select(&mut self, requested: &str) {
        self.requested = requested.to_string();
        self.state = if self.requested.is_empty() { SelState::Auto } else { SelState::Explicit };
    }

    /// 喂入实际采用的模型名（空串 = 最终没有任何模型应答）。
    pub fn set_applied(&mut self, applied: &str) {
        self.applied = applied.to_string();
        self.commit();
    }

    /// 由 (所选, 实际) 推导确定性终态——调用顺序无关，最后一次 set_applied 落定。
    fn commit(&mut self) {
        self.state = if self.requested.is_empty() {
            SelState::Auto
        } else if self.applied.is_empty() {
            SelState::Locked
        } else if self.applied == self.requested {
            SelState::Honored
        } else {
            SelState::Substituted
        };
    }

    pub fn state(&self) -> SelState { self.state }
    pub fn requested(&self) -> &str { &self.requested }
    pub fn applied(&self) -> &str { &self.applied }
    pub fn label(&self) -> &'static str { self.state.label() }
}

// ===== 模型可用性状态机（ModelAvailFSM） =====
// 管的是「这个模型**现在**能不能用、为什么不能用」——与 ModelSelFSM（这一次请求选了谁）
// 和 MemoryFSM（会话上下文窗口）互不重叠。
//
// 设计要点：
// 1. **配置级原因不发网络请求**：渠道未启用 / 未配 Key 直接短路成 Blocked，
//    既省额度又快（这是"选了不生效"的最高频原因）。
// 2. **TTL 的唯一实现**：should_scan 是"2 小时内不重扫"的唯一判定点，
//    放前端只会散成多处 if，且没法单测。
// 3. **原因可比较**：同一模型可能挂在多个渠道，全部失败时取"修复代价最低"的那条，
//    让括号里给的是用户能立刻动手的那句话。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailState {
    Unknown = 0,     // 未探测
    Probing = 1,     // 探测中
    Ok = 2,          // 实测通过，且上游自报 == 所选
    Substituted = 3, // 可用，但上游换成了别的模型
    Blocked = 4,     // 受限，但用户可修复（未启用 / 未配 Key / Key 无效 / 限流 / 5xx / 超时 / 网络）
    Gone = 5,        // 不可用且用户无法自行修复（无渠道 / 上游 404）
}

impl AvailState {
    #[allow(dead_code)]
    fn from_code(c: i32) -> AvailState {
        match c {
            1 => AvailState::Probing,
            2 => AvailState::Ok,
            3 => AvailState::Substituted,
            4 => AvailState::Blocked,
            5 => AvailState::Gone,
            _ => AvailState::Unknown,
        }
    }
    fn code(self) -> i32 { self as i32 }
    /// 是否算"能用"（决定行样式是否走可用态）
    pub fn usable(self) -> bool {
        matches!(self, AvailState::Ok | AvailState::Substituted)
    }
}

/// 受限原因。数值顺序 = 修复代价从低到高，**数值越小越优先展示**。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailReason {
    None = 0,
    Disabled = 1,     // 渠道未启用 —— 点一下就好
    NoKey = 2,        // 未配置 Key —— 填一个就好
    Auth = 3,         // 401/403
    RateLimited = 4,  // 429
    Upstream = 5,     // 5xx
    Timeout = 6,      // 超时
    Network = 7,      // 网络不通
    NotFound = 8,     // 404 模型不存在
    Substituted = 9,  // 上游换家（不是"受限"，仅用于文案）
}

impl AvailReason {
    #[allow(dead_code)]
    fn from_code(c: i32) -> AvailReason {
        match c {
            1 => AvailReason::Disabled,
            2 => AvailReason::NoKey,
            3 => AvailReason::Auth,
            4 => AvailReason::RateLimited,
            5 => AvailReason::Upstream,
            6 => AvailReason::Timeout,
            7 => AvailReason::Network,
            8 => AvailReason::NotFound,
            9 => AvailReason::Substituted,
            _ => AvailReason::None,
        }
    }
    fn code(self) -> i32 { self as i32 }
    /// 括号里给用户看的那句话（要能指导下一步动作，不要写"错误码 429"这种）
    pub fn text(self) -> &'static str {
        match self {
            AvailReason::None => "",
            AvailReason::Disabled => "渠道未启用",
            AvailReason::NoKey => "未配置 Key",
            AvailReason::Auth => "Key 无效",
            AvailReason::RateLimited => "限流中",
            AvailReason::Upstream => "上游异常",
            AvailReason::Timeout => "超时",
            AvailReason::Network => "网络不通",
            AvailReason::NotFound => "模型不存在",
            AvailReason::Substituted => "上游换家",
        }
    }
    /// 从 HTTP 状态码归类（0 = 没拿到响应，按网络错误处理）
    pub fn from_http(status: u16) -> AvailReason {
        match status {
            401 | 403 => AvailReason::Auth,
            404 => AvailReason::NotFound,
            429 => AvailReason::RateLimited,
            0 => AvailReason::Network,
            500..=599 => AvailReason::Upstream,
            _ => AvailReason::Upstream,
        }
    }
    /// 取更优先（修复代价更低）的那条；None 永远让位
    pub fn prefer(self, other: AvailReason) -> AvailReason {
        match (self, other) {
            (AvailReason::None, o) => o,
            (s, AvailReason::None) => s,
            (s, o) => if o.code() < s.code() { o } else { s },
        }
    }
}

pub struct ModelAvailFSM {
    state: AvailState,
    reason: AvailReason,
    requested: String,
    served: String,
    http: u16,
    latency: u32,
    last_ts: f64,
}

impl ModelAvailFSM {
    pub fn new() -> Self {
        ModelAvailFSM {
            state: AvailState::Unknown,
            reason: AvailReason::None,
            requested: String::new(),
            served: String::new(),
            http: 0,
            latency: 0,
            last_ts: 0.0,
        }
    }

    pub fn begin(&mut self) {
        self.state = AvailState::Probing;
        self.reason = AvailReason::None;
        self.http = 0;
        self.latency = 0;
        self.served.clear();
    }

    /// 配置级短路：渠道未启用。不发请求就有结论。
    pub fn mark_disabled(&mut self) {
        self.state = AvailState::Blocked;
        self.reason = AvailReason::Disabled;
        self.http = 0;
        self.latency = 0;
        self.served.clear();
    }

    /// 配置级短路：已启用但没登记 Key。不发请求。
    pub fn mark_no_key(&mut self) {
        self.state = AvailState::Blocked;
        self.reason = AvailReason::NoKey;
        self.http = 0;
        self.latency = 0;
        self.served.clear();
    }

    /// 上游返回了（含错误状态）。served 为空表示上游没自报模型名。
    pub fn result(&mut self, http: u16, latency_ms: u32, served: &str) {
        self.http = http;
        self.latency = latency_ms;
        self.served = served.to_string();
        if (200..300).contains(&http) {
            if self.served.is_empty() || self.served == self.requested {
                self.state = AvailState::Ok;
                self.reason = AvailReason::None;
            } else {
                // 上游换家：能用，但不是你选的那个 —— 不算受限，但要警示
                self.state = AvailState::Substituted;
                self.reason = AvailReason::Substituted;
            }
        } else {
            let r = AvailReason::from_http(http);
            self.state = if r == AvailReason::NotFound { AvailState::Gone } else { AvailState::Blocked };
            self.reason = r;
        }
    }

    /// 传输层失败（fetch 抛错 / 超时）
    pub fn fail(&mut self, reason: AvailReason, latency_ms: u32) {
        self.state = if reason == AvailReason::NotFound { AvailState::Gone } else { AvailState::Blocked };
        self.reason = reason;
        self.http = 0;
        self.latency = latency_ms;
        self.served.clear();
    }

    /// 另一渠道的结论并入本模型：任一渠道可用即可用；全挂则保留"最可行动"的原因。
    pub fn merge(&mut self, other_state: AvailState, other_reason: AvailReason) {
        if other_state.usable() && !self.state.usable() {
            self.state = other_state;
            self.reason = other_reason;
            return;
        }
        if other_state.usable() {
            // 两边都能用：Substituted 让位给 Ok（有一个真用了所选模型就够了）
            if other_state == AvailState::Ok {
                self.state = AvailState::Ok;
                self.reason = AvailReason::None;
            }
            return;
        }
        if self.state.usable() {
            return;
        }
        // 都不可用：取修复代价更低的原因；Gone 让位给 Blocked（能修优先）
        self.reason = self.reason.prefer(other_reason);
        if other_state == AvailState::Blocked && self.state == AvailState::Gone {
            self.state = AvailState::Blocked;
        }
    }

    /// TTL 规则的唯一实现：距上次探测是否已超过 ttl（last_ts == 0 视为从未探测）。
    pub fn should_scan(&self, now_ms: f64, ttl_ms: f64) -> bool {
        if self.last_ts <= 0.0 { return true; }
        now_ms - self.last_ts >= ttl_ms
    }

    pub fn touch(&mut self, now_ms: f64) { self.last_ts = now_ms; }
    pub fn last_ts(&self) -> f64 { self.last_ts }
    pub fn state(&self) -> AvailState { self.state }
    pub fn reason(&self) -> AvailReason { self.reason }
    pub fn http(&self) -> u16 { self.http }
    pub fn latency(&self) -> u32 { self.latency }
    pub fn served(&self) -> &str { &self.served }
    pub fn requested(&self) -> &str { &self.requested }

    pub fn set_requested(&mut self, m: &str) { self.requested = m.to_string(); }

    /// 括号文案。**可用态返回空串**——满屏括号是噪音，只有需要解释时才出现。
    pub fn bracket(&self) -> String {
        match self.state {
            AvailState::Unknown | AvailState::Ok => String::new(),
            AvailState::Probing => "检测中…".to_string(),
            AvailState::Substituted => {
                if self.served.is_empty() {
                    "上游换家".to_string()
                } else {
                    format!("上游实为 {}", self.served)
                }
            }
            AvailState::Blocked | AvailState::Gone => self.reason.text().to_string(),
        }
    }
}


// ---- FSM 的 WASM ABI（仅 wasm32；宿主 cargo test 走下方单测） ----
#[cfg(target_arch = "wasm32")]
mod fsm_abi {
    use super::*;
    use std::sync::Mutex;

    static FSM_REG: Mutex<Vec<Option<MemoryStateMachine>>> = Mutex::new(Vec::new());

    /// 句柄 = 注册表下标；越界/已释放返回 -1。
    #[no_mangle]
    pub extern "C" fn memfsm_new(max_tokens: u32, hard_max: u32, keep_recent: u32) -> i32 {
        let mut reg = FSM_REG.lock().unwrap();
        reg.push(Some(MemoryStateMachine::new(max_tokens, hard_max, keep_recent)));
        (reg.len() as i32) - 1
    }

    #[no_mangle]
    pub extern "C" fn memfsm_drop(h: i32) {
        let mut reg = FSM_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            *slot = None;
        }
    }

    /// 仅在闭包返回 Copy 值时使用（不逃逸借用）。
    fn with_fsm<R: Copy>(h: i32, f: impl FnOnce(&mut MemoryStateMachine) -> R) -> Option<R> {
        let mut reg = FSM_REG.lock().unwrap();
        reg.get_mut(h as usize).and_then(|slot| slot.as_mut()).map(f)
    }

    #[no_mangle]
    pub extern "C" fn memfsm_set(h: i32, state: i32, token_count: u32, summary_pointer: i32, msg_count: u32) {
        let mut reg = FSM_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            if let Some(fsm) = slot.as_mut() {
                fsm.restore(state, token_count, summary_pointer, msg_count);
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn memfsm_transition(h: i32, incoming_tokens: u32, new_msgs: u32) -> i32 {
        with_fsm(h, |f| f.transition(incoming_tokens, new_msgs).code()).unwrap_or(-1)
    }

    #[no_mangle]
    pub extern "C" fn memfsm_state(h: i32) -> i32 {
        with_fsm(h, |f| f.state().code()).unwrap_or(-1)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_token_count(h: i32) -> u32 {
        with_fsm(h, |f| f.token_count()).unwrap_or(0)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_summary_pointer(h: i32) -> i32 {
        with_fsm(h, |f| f.summary_pointer() as i32).unwrap_or(-1)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_msg_count(h: i32) -> u32 {
        with_fsm(h, |f| f.msg_count()).unwrap_or(0)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_window_keep_from(h: i32) -> i32 {
        with_fsm(h, |f| f.window_keep_from() as i32).unwrap_or(-1)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_need_summary(h: i32) -> i32 {
        with_fsm(h, |f| if f.need_summary() { 1 } else { 0 }).unwrap_or(0)
    }
    #[no_mangle]
    pub extern "C" fn memfsm_note_summary(h: i32, summary_tokens: u32, new_pointer: i32) {
        let mut reg = FSM_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            if let Some(fsm) = slot.as_mut() {
                fsm.note_summary(summary_tokens, new_pointer);
            }
        }
    }
}

// ===== 模型选择 FSM 的 WASM ABI =====
#[cfg(target_arch = "wasm32")]
mod modelsel_abi {
    use super::*;
    use std::sync::Mutex;

    static SEL_REG: Mutex<Vec<Option<ModelSelFSM>>> = Mutex::new(Vec::new());
    static SEL_SCRATCH: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    /// 入参读取：从线性内存绝对地址取字节（调用方保证有效）。
    unsafe fn read_input(ptr: i32, len: i32) -> &'static [u8] {
        std::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize)
    }
    /// 把字符串拷入暂存缓冲，返回其线性内存指针（JS 紧随读取）。
    fn scratch_copy(s: &str) -> i32 {
        let mut buf = SEL_SCRATCH.lock().unwrap();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.as_ptr() as i32
    }

    #[no_mangle]
    pub extern "C" fn modelsel_new() -> i32 {
        let mut reg = SEL_REG.lock().unwrap();
        reg.push(Some(ModelSelFSM::new()));
        (reg.len() as i32) - 1
    }

    #[no_mangle]
    pub extern "C" fn modelsel_drop(h: i32) {
        let mut reg = SEL_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            *slot = None;
        }
    }

    /// 喂入用户所选模型（空串 = 自动选路）。
    #[no_mangle]
    pub extern "C" fn modelsel_select(h: i32, ptr: i32, len: i32) {
        let req = unsafe { read_input(ptr, len) };
        let req = String::from_utf8_lossy(req).to_string();
        let mut reg = SEL_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            if let Some(f) = slot.as_mut() {
                f.select(&req);
            }
        }
    }

    /// 喂入实际采用的模型名（空串 = 最终无模型应答）。
    #[no_mangle]
    pub extern "C" fn modelsel_set_applied(h: i32, ptr: i32, len: i32) {
        let ap = unsafe { read_input(ptr, len) };
        let ap = String::from_utf8_lossy(ap).to_string();
        let mut reg = SEL_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            if let Some(f) = slot.as_mut() {
                f.set_applied(&ap);
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn modelsel_state(h: i32) -> i32 {
        let reg = SEL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|s| s.as_ref()).map(|f| f.state().code()).unwrap_or(-1)
    }

    #[no_mangle]
    pub extern "C" fn modelsel_requested_ptr(h: i32) -> i32 {
        let s = SEL_REG.lock().unwrap()
            .get(h as usize).and_then(|x| x.as_ref()).map(|f| f.requested().to_string()).unwrap_or_default();
        scratch_copy(&s)
    }
    #[no_mangle]
    pub extern "C" fn modelsel_requested_len(h: i32) -> i32 {
        SEL_REG.lock().unwrap().get(h as usize).and_then(|x| x.as_ref()).map(|f| f.requested().len() as i32).unwrap_or(0)
    }
    #[no_mangle]
    pub extern "C" fn modelsel_applied_ptr(h: i32) -> i32 {
        let s = SEL_REG.lock().unwrap()
            .get(h as usize).and_then(|x| x.as_ref()).map(|f| f.applied().to_string()).unwrap_or_default();
        scratch_copy(&s)
    }
    #[no_mangle]
    pub extern "C" fn modelsel_applied_len(h: i32) -> i32 {
        SEL_REG.lock().unwrap().get(h as usize).and_then(|x| x.as_ref()).map(|f| f.applied().len() as i32).unwrap_or(0)
    }
}

// ---- 模型可用性状态机的 WASM ABI（仅 wasm32；宿主 cargo test 走下方单测） ----
#[cfg(target_arch = "wasm32")]
mod avail_abi {
    use super::*;
    use std::sync::Mutex;

    static AVAIL_REG: Mutex<Vec<Option<ModelAvailFSM>>> = Mutex::new(Vec::new());
    static AVAIL_SCRATCH: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    unsafe fn read_input(ptr: i32, len: i32) -> &'static [u8] {
        std::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize)
    }
    fn scratch_copy(s: &str) -> i32 {
        let mut buf = AVAIL_SCRATCH.lock().unwrap();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.as_ptr() as i32
    }
    fn with<F: FnOnce(&mut ModelAvailFSM)>(h: i32, f: F) {
        let mut reg = AVAIL_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) {
            if let Some(fsm) = slot.as_mut() { f(fsm); }
        }
    }
    fn read_str(h: i32, f: impl Fn(&ModelAvailFSM) -> String) -> i32 {
        let s = {
            let reg = AVAIL_REG.lock().unwrap();
            reg.get(h as usize).and_then(|x| x.as_ref()).map(f).unwrap_or_default()
        };
        scratch_copy(&s)
    }

    #[no_mangle]
    pub extern "C" fn avail_new() -> i32 {
        let mut reg = AVAIL_REG.lock().unwrap();
        reg.push(Some(ModelAvailFSM::new()));
        (reg.len() as i32) - 1
    }

    #[no_mangle]
    pub extern "C" fn avail_drop(h: i32) {
        let mut reg = AVAIL_REG.lock().unwrap();
        if let Some(slot) = reg.get_mut(h as usize) { *slot = None; }
    }

    /// 绑定被探测的模型名（用于判断上游是否换家）
    #[no_mangle]
    pub extern "C" fn avail_set_requested(h: i32, ptr: i32, len: i32) {
        let s = String::from_utf8_lossy(unsafe { read_input(ptr, len) }).to_string();
        with(h, |f| f.set_requested(&s));
    }

    #[no_mangle]
    pub extern "C" fn avail_begin(h: i32) { with(h, |f| f.begin()); }

    /// 配置级短路：1 = 渠道未启用，2 = 未配置 Key
    #[no_mangle]
    pub extern "C" fn avail_mark(h: i32, kind: i32) {
        with(h, |f| match kind {
            1 => f.mark_disabled(),
            2 => f.mark_no_key(),
            _ => {}
        });
    }

    /// 上游结果：http = 0 表示传输层失败，此时 reason 由调用方指定
    #[no_mangle]
    pub extern "C" fn avail_result(h: i32, http: u32, latency_ms: u32, served_ptr: i32, served_len: i32) {
        let served = String::from_utf8_lossy(unsafe { read_input(served_ptr, served_len) }).to_string();
        with(h, |f| f.result(http as u16, latency_ms, &served));
    }

    /// 传输层失败：reason 传 AvailReason 的码值（6 超时 / 7 网络）
    #[no_mangle]
    pub extern "C" fn avail_fail(h: i32, reason: i32, latency_ms: u32) {
        with(h, |f| f.fail(AvailReason::from_code(reason), latency_ms));
    }

    /// 并入另一个渠道的结论
    #[no_mangle]
    pub extern "C" fn avail_merge(h: i32, state: i32, reason: i32) {
        with(h, |f| f.merge(AvailState::from_code(state), AvailReason::from_code(reason)));
    }

    #[no_mangle]
    pub extern "C" fn avail_state(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.state().code()).unwrap_or(-1)
    }

    #[no_mangle]
    pub extern "C" fn avail_reason(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.reason().code()).unwrap_or(0)
    }

    #[no_mangle]
    pub extern "C" fn avail_http(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.http() as i32).unwrap_or(0)
    }

    #[no_mangle]
    pub extern "C" fn avail_latency(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.latency() as i32).unwrap_or(0)
    }

    /// 括号文案（可用态为空串）
    #[no_mangle]
    pub extern "C" fn avail_bracket_ptr(h: i32) -> i32 { read_str(h, |f| f.bracket()) }
    #[no_mangle]
    pub extern "C" fn avail_bracket_len(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.bracket().len() as i32).unwrap_or(0)
    }

    /// 上游自报的模型名
    #[no_mangle]
    pub extern "C" fn avail_served_ptr(h: i32) -> i32 { read_str(h, |f| f.served().to_string()) }
    #[no_mangle]
    pub extern "C" fn avail_served_len(h: i32) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.served().len() as i32).unwrap_or(0)
    }

    /// TTL 规则：1 = 该扫，0 = 命中缓存不扫
    #[no_mangle]
    pub extern "C" fn avail_should_scan(h: i32, now_ms: f64, ttl_ms: f64) -> i32 {
        let reg = AVAIL_REG.lock().unwrap();
        match reg.get(h as usize).and_then(|x| x.as_ref()) {
            Some(f) => if f.should_scan(now_ms, ttl_ms) { 1 } else { 0 },
            None => 1,
        }
    }

    #[no_mangle]
    pub extern "C" fn avail_touch(h: i32, now_ms: f64) { with(h, |f| f.touch(now_ms)); }

    #[no_mangle]
    pub extern "C" fn avail_last_ts(h: i32) -> f64 {
        let reg = AVAIL_REG.lock().unwrap();
        reg.get(h as usize).and_then(|x| x.as_ref()).map(|f| f.last_ts()).unwrap_or(0.0)
    }
}

fn placeholder_wf() -> Workflow {
    // 空占位：engine_new 后、engine_load 前若误调 start 会因事件不匹配安全失败。
    Workflow {
        id: "__unloaded__".into(),
        version: 0,
        trigger: Trigger { event: "__none__".into() },
        max_concurrency: 1,
        steps: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// WASM ABI（仅 wasm32 编译；宿主机 cargo test 不 포함这部分）
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod abi {
    use super::*;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use std::sync::Mutex;

    static CORE: Mutex<EngineCore> = Mutex::new(EngineCore { engines: Vec::new(), runs: Vec::new(), last_err: String::new() });
    static SCRATCH: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static ARENA_BASE: AtomicI32 = AtomicI32::new(0);
    static ARENA_OFF: AtomicUsize = AtomicUsize::new(0);

    fn set_scratch(bytes: &[u8]) {
        let mut s = SCRATCH.lock().unwrap();
        s.clear();
        s.extend_from_slice(bytes);
    }

    /// 入参读取：从竞技场绝对地址取字节。调用方保证 [ptr, ptr+len) 有效。
    unsafe fn read_input(ptr: i32, len: i32) -> &'static [u8] {
        std::slice::from_raw_parts(ptr as *const u8, len as usize)
    }

    fn last_err_ptr() -> (i32, i32) {
        let core = CORE.lock().unwrap();
        set_scratch(core.last_err.as_bytes());
        let s = SCRATCH.lock().unwrap();
        (s.as_ptr() as i32, s.len() as i32)
    }

    /// 竞技场预留（进程一次）。返回基址；之后 alloc 在其上 bump 分配。
    #[no_mangle]
    pub extern "C" fn mem_reserve(cap: i32) -> i32 {
        let v = vec![0u8; cap as usize].into_boxed_slice();
        let base = v.as_ptr() as i32;
        std::mem::forget(v); // 故意泄漏：竞技场活到实例销毁
        ARENA_BASE.store(base, Ordering::SeqCst);
        ARENA_OFF.store(0, Ordering::SeqCst);
        base
    }

    /// bump 分配 len 字节，返回绝对地址（1 字节对齐，够放 u8/JSON）。
    #[no_mangle]
    pub extern "C" fn alloc(len: i32) -> i32 {
        let off = ARENA_OFF.fetch_add(len as usize, Ordering::SeqCst);
        ARENA_BASE.load(Ordering::SeqCst) + off as i32
    }

    #[no_mangle]
    pub extern "C" fn arena_reset() {
        ARENA_OFF.store(0, Ordering::SeqCst);
    }

    #[no_mangle]
    pub extern "C" fn scratch_ptr() -> i32 {
        let s = SCRATCH.lock().unwrap();
        s.as_ptr() as i32
    }

    #[no_mangle]
    pub extern "C" fn scratch_len() -> i32 {
        let s = SCRATCH.lock().unwrap();
        s.len() as i32
    }

    #[no_mangle]
    pub extern "C" fn last_error_ptr() -> i32 {
        last_err_ptr().0
    }
    #[no_mangle]
    pub extern "C" fn last_error_len() -> i32 {
        last_err_ptr().1
    }

    #[no_mangle]
    pub extern "C" fn engine_new() -> i32 {
        CORE.lock().unwrap().new_engine()
    }

    #[no_mangle]
    pub extern "C" fn engine_load(handle: i32, ptr: i32, len: i32) -> i32 {
        let json = unsafe { std::str::from_utf8(read_input(ptr, len)).unwrap_or("") };
        CORE.lock().unwrap().load(handle, json)
    }

    #[no_mangle]
    pub extern "C" fn engine_start(handle: i32, ptr: i32, len: i32) -> i32 {
        let ev = unsafe { std::str::from_utf8(read_input(ptr, len)).unwrap_or("") };
        CORE.lock().unwrap().start(handle, ev)
    }

    #[no_mangle]
    pub extern "C" fn engine_ready(run_id: i32) -> i32 {
        let mut core = CORE.lock().unwrap();
        match core.ready(run_id) {
            Ok(s) => {
                set_scratch(s.as_bytes());
                1
            }
            Err(e) => {
                core.last_err = e;
                0
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn engine_complete(run_id: i32, sid_ptr: i32, sid_len: i32, out_ptr: i32, out_len: i32) -> i32 {
        let sid = unsafe { std::str::from_utf8(read_input(sid_ptr, sid_len)).unwrap_or("") };
        let out = unsafe { std::str::from_utf8(read_input(out_ptr, out_len)).unwrap_or("") };
        let mut core = CORE.lock().unwrap();
        match core.complete(run_id, sid, out) {
            Ok(()) => 0,
            Err(e) => {
                core.last_err = e;
                -1
            }
        }
    }

    /// 回报失败；返回 1 = 可重试，0 = 次数用尽（run FAILED），-1 = 调用错误。
    #[no_mangle]
    pub extern "C" fn engine_fail(run_id: i32, sid_ptr: i32, sid_len: i32, err_ptr: i32, err_len: i32) -> i32 {
        let sid = unsafe { std::str::from_utf8(read_input(sid_ptr, sid_len)).unwrap_or("") };
        let err = unsafe { std::str::from_utf8(read_input(err_ptr, err_len)).unwrap_or("") };
        let mut core = CORE.lock().unwrap();
        match core.fail(run_id, sid, err) {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(e) => {
                core.last_err = e;
                -1
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn engine_status(run_id: i32) -> i32 {
        let mut core = CORE.lock().unwrap();
        match core.status(run_id) {
            Ok(s) => {
                set_scratch(s.as_bytes());
                1
            }
            Err(e) => {
                core.last_err = e;
                0
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn engine_result(run_id: i32) -> i32 {
        let core = CORE.lock().unwrap();
        match core.result(run_id) {
            Ok(s) => {
                drop(core);
                set_scratch(s.as_bytes());
                1
            }
            Err(e) => {
                drop(core);
                CORE.lock().unwrap().last_err = e;
                0
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 宿主机单测（cargo test，逻辑与编译靶无关）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(json: &str) -> Workflow {
        serde_json::from_str(json).unwrap()
    }

    const W: &str = r#"{
      "id": "t", "version": 1, "trigger": { "event": "e" },
      "steps": [
        { "id": "a", "plugin": "p.a", "input": { "x": "${/event/payload/v}" }, "timeout_ms": 100, "retries": 1 },
        { "id": "b", "plugin": "p.b", "depends_on": ["a"], "input": { "prev": "${/steps/a/out}" } },
        { "id": "c", "plugin": "p.c" }
      ]
    }"#;

    #[test]
    fn validate_rejects_cycle_and_dups() {
        let cyc = r#"{"id":"c","trigger":{"event":"e"},"steps":[
            {"id":"a","plugin":"p","depends_on":["b"]},{"id":"b","plugin":"p","depends_on":["a"]}]}"#;
        assert!(validate(&wf(cyc)).unwrap_err().contains("循环依赖"));
        let dup = r#"{"id":"c","trigger":{"event":"e"},"steps":[
            {"id":"a","plugin":"p"},{"id":"a","plugin":"p"}]}"#;
        assert!(validate(&wf(dup)).unwrap_err().contains("重复"));
        let unknown = r#"{"id":"c","trigger":{"event":"e"},"steps":[
            {"id":"a","plugin":"p","depends_on":["zzz"]}]}"#;
        assert!(validate(&wf(unknown)).unwrap_err().contains("不存在"));
        assert!(validate(&wf(W)).is_ok());
    }

    #[test]
    fn template_resolution() {
        let ctx = json!({"event": {"payload": {"v": 42}}, "steps": {"a": {"out": "hi"}}});
        assert_eq!(resolve_templates(&json!("${/event/payload/v}"), &ctx), json!(42));
        assert_eq!(resolve_templates(&json!("${/steps/a/out}"), &ctx), json!("hi"));
        assert_eq!(resolve_templates(&json!("${/nope/x}"), &ctx), Value::Null);
        // 整串命中才解析；混排原样
        assert_eq!(resolve_templates(&json!("pre${/event/payload/v}"), &ctx), json!("pre${/event/payload/v}"));
        // 对象/数组递归
        let t = json!({"k": ["${/steps/a/out}", 1]});
        assert_eq!(resolve_templates(&t, &ctx), json!({"k": ["hi", 1]}));
    }

    #[test]
    fn dag_ready_and_flow() {
        let mut core = EngineCore::default();
        let h = core.new_engine();
        assert_eq!(core.load(h, W), 0);
        let rid = core.start(h, r#"{"event":"e","payload":{"v":7}}"#);
        assert!(rid >= 0);
        // 第一轮就绪：a、c（b 等 a）
        let r: Value = serde_json::from_str(&core.ready(rid).unwrap()).unwrap();
        let ids: Vec<&str> = r["steps"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(r["steps"][0]["input"]["x"], json!(7));
        core.complete(rid, "a", r#"{"out":"A"}"#).unwrap();
        core.complete(rid, "c", "null").unwrap();
        let r: Value = serde_json::from_str(&core.ready(rid).unwrap()).unwrap();
        assert_eq!(r["steps"][0]["id"], "b");
        assert_eq!(r["steps"][0]["input"]["prev"], json!("A")); // 步骤间传数
        core.complete(rid, "b", "null").unwrap();
        let st: Value = serde_json::from_str(&core.status(rid).unwrap()).unwrap();
        assert_eq!(st["status"], json!(RUN_DONE));
    }

    #[test]
    fn retry_then_fail_kills_run() {
        let mut core = EngineCore::default();
        let h = core.new_engine();
        assert_eq!(core.load(h, W), 0);
        let rid = core.start(h, r#"{"event":"e","payload":{}}"#);
        assert!(rid >= 0);
        let _ = core.ready(rid).unwrap(); // a, c running
        // a 第一次失败 → 可重试（retries=1）
        assert!(core.fail(rid, "a", "boom").unwrap());
        let r: Value = serde_json::from_str(&core.ready(rid).unwrap()).unwrap();
        let arr = r["steps"].as_array().unwrap();
        let a_again = arr.iter().any(|s| s["id"] == "a");
        assert!(a_again, "重试应回就绪池");
        // 第二次失败 → 次数用尽，run FAILED
        assert!(!core.fail(rid, "a", "boom2").unwrap());
        let st: Value = serde_json::from_str(&core.status(rid).unwrap()).unwrap();
        assert_eq!(st["status"], json!(RUN_FAILED));
        assert_eq!(st["steps"]["b"]["state"], json!(STEP_SKIPPED));
    }

    #[test]
    fn event_mismatch_rejected() {
        let mut core = EngineCore::default();
        let h = core.new_engine();
        assert_eq!(core.load(h, W), 0);
        assert_eq!(core.start(h, r#"{"event":"other"}"#), -2);
    }
}

#[cfg(test)]
mod fsm_tests {
    use super::*;

    #[test]
    fn idle_to_collecting_on_first_transition() {
        let mut f = MemoryStateMachine::new(100, 1000, 4);
        assert_eq!(f.transition(10, 2), SessionState::Collecting);
        assert_eq!(f.token_count(), 10);
        assert_eq!(f.msg_count(), 2);
    }

    #[test]
    fn summarizing_when_soft_threshold_exceeded() {
        let mut f = MemoryStateMachine::new(100, 1000, 4);
        f.transition(60, 2); // Collecting
        f.transition(50, 2); // 110 > 100 → Summarizing
        assert_eq!(f.state(), SessionState::Summarizing);
        assert!(f.need_summary());
    }

    #[test]
    fn locked_when_hard_max_exceeded() {
        let mut f = MemoryStateMachine::new(100, 200, 4);
        f.transition(150, 2);
        assert_eq!(f.state(), SessionState::Summarizing); // 150 > 100 软阈 → Summarizing
        f.transition(100, 2); // 250 > 200 硬上限 → Locked
        assert_eq!(f.state(), SessionState::Locked);
    }

    #[test]
    fn window_respects_recent_floor_and_summary_pointer() {
        let mut f = MemoryStateMachine::new(100, 1000, 4);
        f.transition(10, 10); // 10 messages, summary_pointer 0
        assert_eq!(f.window_keep_from(), 6); // 10 - 4
        f.note_summary(5, 6);
        assert_eq!(f.summary_pointer(), 6);
        f.transition(10, 2); // 12 total; recent_floor 8, summary_pointer 6 → max = 8
        assert_eq!(f.window_keep_from(), 8);
    }

    #[test]
    fn note_summary_resets_and_returns_to_collecting() {
        let mut f = MemoryStateMachine::new(100, 1000, 4);
        f.transition(200, 10); // Summarizing
        assert!(f.need_summary());
        f.note_summary(8, 10);
        assert_eq!(f.state(), SessionState::Collecting);
        assert_eq!(f.token_count(), 8);
        assert!(!f.need_summary());
    }

    #[test]
    fn restore_roundtrip_keeps_boundary() {
        let mut f = MemoryStateMachine::new(100, 1000, 4);
        f.transition(200, 10);
        f.note_summary(8, 10);
        let (st, tk, sp, mc) = (f.state().code(), f.token_count(), f.summary_pointer() as i32, f.msg_count());
        let mut g = MemoryStateMachine::new(100, 1000, 4);
        g.restore(st, tk, sp, mc);
        assert_eq!(g.state(), SessionState::Collecting);
        assert_eq!(g.token_count(), 8);
        assert_eq!(g.summary_pointer(), 10);
        assert_eq!(g.msg_count(), 10);
        // 继续追加：窗口不应低于已折叠边界
        g.transition(5, 2);
        assert_eq!(g.window_keep_from(), 10); // max(10, 12-4=8)
    }

    #[test]
    fn sel_auto_when_no_explicit_choice() {
        let mut f = ModelSelFSM::new();
        f.select(""); // 自动选路
        f.set_applied("deepseek-flash"); // 网关选了某个模型
        assert_eq!(f.state(), SelState::Auto);
        assert_eq!(f.requested(), "");
        assert_eq!(f.applied(), "deepseek-flash");
    }

    #[test]
    fn sel_honored_when_applied_equals_requested() {
        let mut f = ModelSelFSM::new();
        f.select("openrouter/free");
        f.set_applied("openrouter/free"); // 所选确实被采用
        assert_eq!(f.state(), SelState::Honored);
        assert_eq!(f.label(), "已采用所选");
    }

    #[test]
    fn sel_substituted_when_different_model_applied() {
        let mut f = ModelSelFSM::new();
        f.select("openrouter/free");
        // 所选在池中但首包前失败 → 模型级切换；或所选不在池中 → 回落自动选路。
        // 二者最终都表现成「实际用别的模型」，FSM 统一归为 Substituted。
        f.set_applied("deepseek-flash");
        assert_eq!(f.state(), SelState::Substituted);
        assert_eq!(f.requested(), "openrouter/free");
        assert_eq!(f.applied(), "deepseek-flash");
    }

    #[test]
    fn sel_locked_when_nothing_answered() {
        let mut f = ModelSelFSM::new();
        f.select("openrouter/free");
        f.set_applied(""); // 全部候选失败，无模型应答
        assert_eq!(f.state(), SelState::Locked);
    }

    #[test]
    fn sel_state_code_roundtrip() {
        for (code, st) in [
            (0, SelState::Auto),
            (1, SelState::Explicit),
            (2, SelState::Honored),
            (3, SelState::Substituted),
            (4, SelState::Locked),
        ] {
            assert_eq!(SelState::from_code(code), st);
        }
    }

    // ---- ModelAvailFSM：TTL / 判定 / 原因优先级 / 文案 ----

    const TTL: f64 = 2.0 * 3600.0 * 1000.0; // 2 小时

    /// 2 小时内再进页面不重扫——这是"不打扰、不耗额度"的核心规则
    #[test]
    fn avail_ttl_blocks_rescan() {
        let mut f = ModelAvailFSM::new();
        let t0 = 1_700_000_000_000.0;
        f.touch(t0);
        assert!(!f.should_scan(t0, TTL), "刚扫完不该再扫");
        assert!(!f.should_scan(t0 + 3600_000.0, TTL), "1 小时后仍不该扫");
        assert!(!f.should_scan(t0 + TTL - 1.0, TTL), "差 1ms 到 2 小时仍不该扫");
    }

    /// 超过 2 小时必须重扫
    #[test]
    fn avail_ttl_allows_rescan() {
        let mut f = ModelAvailFSM::new();
        let t0 = 1_700_000_000_000.0;
        f.touch(t0);
        assert!(f.should_scan(t0 + TTL, TTL), "正好 2 小时应重扫");
        assert!(f.should_scan(t0 + TTL + 1.0, TTL), "超过 2 小时应重扫");
        // 从未探测过（last_ts == 0）必须扫
        let fresh = ModelAvailFSM::new();
        assert!(fresh.should_scan(t0, TTL), "从未探测过应扫");
    }

    /// 上游自报 == 所选 → Ok，且**不加括号**（满屏括号是噪音）
    #[test]
    fn avail_ok_when_served_matches() {
        let mut f = ModelAvailFSM::new();
        f.set_requested("cohere/north-mini-code:free");
        f.begin();
        assert_eq!(f.state(), AvailState::Probing);
        assert_eq!(f.bracket(), "检测中…");
        f.result(200, 980, "cohere/north-mini-code:free");
        assert_eq!(f.state(), AvailState::Ok);
        assert_eq!(f.bracket(), "", "可用态不得有括号");
        assert!(f.state().usable());
    }

    /// 上游换家 → Substituted：能用但警示，括号写明实际是谁
    #[test]
    fn avail_substituted_when_served_differs() {
        let mut f = ModelAvailFSM::new();
        f.set_requested("google/gemma-4-31b-it:free");
        f.result(200, 700, "qwen/qwen3-8b:free");
        assert_eq!(f.state(), AvailState::Substituted);
        assert_eq!(f.bracket(), "上游实为 qwen/qwen3-8b:free");
        assert!(f.state().usable(), "换家仍算可用，只是警示");

        // 上游没自报模型名 → 视为正常，不误报换家
        let mut g = ModelAvailFSM::new();
        g.set_requested("a/b:free");
        g.result(200, 500, "");
        assert_eq!(g.state(), AvailState::Ok);
    }

    /// 401/403/429 都是 Blocked，但原因不同（文案要能指导动作）
    #[test]
    fn avail_blocked_on_401_403_429() {
        for (http, want) in [
            (401u16, "Key 无效"),
            (403, "Key 无效"),
            (429, "限流中"),
            (502, "上游异常"),
        ] {
            let mut f = ModelAvailFSM::new();
            f.set_requested("m");
            f.result(http, 300, "");
            assert_eq!(f.state(), AvailState::Blocked, "HTTP {http} 应判受限");
            assert_eq!(f.bracket(), want, "HTTP {http} 文案不对");
            assert!(!f.state().usable());
        }
    }

    /// 404 → Gone（用户改不了，只能删掉/改名）
    #[test]
    fn avail_gone_on_404() {
        let mut f = ModelAvailFSM::new();
        f.set_requested("nope/not-a-model:free");
        f.result(404, 200, "");
        assert_eq!(f.state(), AvailState::Gone);
        assert_eq!(f.bracket(), "模型不存在");
        assert!(!f.state().usable());
    }

    /// 多渠道全挂时取"修复代价最低"的原因：未启用 > 未配 Key > Key 无效 > 限流
    #[test]
    fn avail_reason_priority() {
        // 单看 prefer
        assert_eq!(AvailReason::RateLimited.prefer(AvailReason::Disabled), AvailReason::Disabled);
        assert_eq!(AvailReason::Disabled.prefer(AvailReason::RateLimited), AvailReason::Disabled);
        assert_eq!(AvailReason::None.prefer(AvailReason::NoKey), AvailReason::NoKey);
        assert_eq!(AvailReason::Auth.prefer(AvailReason::None), AvailReason::Auth);

        // 合并：A 渠道限流、B 渠道未配 Key → 展示"未配置 Key"（更可行动）
        let mut f = ModelAvailFSM::new();
        f.set_requested("m");
        f.result(429, 100, "");
        f.merge(AvailState::Blocked, AvailReason::NoKey);
        assert_eq!(f.reason(), AvailReason::NoKey);
        assert_eq!(f.bracket(), "未配置 Key");

        // 任一渠道可用 → 整体可用（不被别的渠道的失败拖累）
        let mut g = ModelAvailFSM::new();
        g.set_requested("m");
        g.result(429, 100, "");
        g.merge(AvailState::Ok, AvailReason::None);
        assert_eq!(g.state(), AvailState::Ok);
        assert_eq!(g.bracket(), "");

        // Gone 让位给 Blocked：能修的优先告诉用户
        let mut k = ModelAvailFSM::new();
        k.set_requested("m");
        k.result(404, 100, "");
        assert_eq!(k.state(), AvailState::Gone);
        k.merge(AvailState::Blocked, AvailReason::Disabled);
        assert_eq!(k.state(), AvailState::Blocked);
        assert_eq!(k.bracket(), "渠道未启用");
    }

    /// 配置级短路不发请求：未启用 / 未配 Key 直接出结论
    #[test]
    fn avail_config_shortcircuit() {
        let mut f = ModelAvailFSM::new();
        f.mark_disabled();
        assert_eq!(f.state(), AvailState::Blocked);
        assert_eq!(f.bracket(), "渠道未启用");
        assert_eq!(f.http(), 0, "短路不得有 HTTP 状态");

        let mut g = ModelAvailFSM::new();
        g.mark_no_key();
        assert_eq!(g.state(), AvailState::Blocked);
        assert_eq!(g.bracket(), "未配置 Key");

        // 传输层失败
        let mut h = ModelAvailFSM::new();
        h.fail(AvailReason::Timeout, 8000);
        assert_eq!(h.state(), AvailState::Blocked);
        assert_eq!(h.bracket(), "超时");
        h.fail(AvailReason::Network, 0);
        assert_eq!(h.bracket(), "网络不通");
    }

    /// 状态码 roundtrip（前端按数字读）
    #[test]
    fn avail_state_code_roundtrip() {
        for (code, st) in [
            (0, AvailState::Unknown),
            (1, AvailState::Probing),
            (2, AvailState::Ok),
            (3, AvailState::Substituted),
            (4, AvailState::Blocked),
            (5, AvailState::Gone),
        ] {
            assert_eq!(AvailState::from_code(code), st);
        }
        for (code, rs) in [
            (1, AvailReason::Disabled),
            (2, AvailReason::NoKey),
            (3, AvailReason::Auth),
            (4, AvailReason::RateLimited),
            (5, AvailReason::Upstream),
            (6, AvailReason::Timeout),
            (7, AvailReason::Network),
            (8, AvailReason::NotFound),
        ] {
            assert_eq!(AvailReason::from_code(code), rs);
        }
    }
}
