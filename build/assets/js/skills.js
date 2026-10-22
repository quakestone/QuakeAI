/* ============================================================
   QuakeAI skills.js —— 提示词（Skill）子系统（F5/F20 数据层）
   与旧 UI 同口径：
   - 三流合并：系统级（深度思考/智能搜索，tier=system）→ 默认对话（≤3，可暂停）
     → 手选（本轮 activeSkill），去重保序；单条 >100KB 跳过并提示
   - 双通道生效：system 消息注入（全渠道保底）+ x-gw-features（网关按渠道注入真参数）
   - 种子化：首进入种「三个诸葛亮」；系统级幂等补种
   - 持久化键不变：gw_default_skills / gw_default_skill（旧单选迁移）/
     gw_sys_think / gw_sys_search / gw_skip_default
   ============================================================ */
(function (global) {
  "use strict";

  const { config } = global.QuakeStore;
  const { gateway } = global.QuakeAdapters;
  const { toast } = global.QuakeComponents;

  /* 单条 skill 提示词上限 100KB（与 attachments.LIMITS.skill 同值）。
     本模块自持 byteLen 常量而不 import attachments——避免脚本加载顺序耦合
     （skills 与附件管线互为独立子系统，限额口径一致即可）。 */
  const SKILL_MAX = 102400;
  function skillByteLen(s) {
    try { return new TextEncoder().encode(String(s || "")).length; }
    catch { return String(s || "").length; }
  }
  function fmtSkillSize(b) {
    if (b == null) return "";
    if (b < 1024) return b + "B";
    if (b < 1048576) return (b / 1024).toFixed(0) + "KB";
    return (b / 1048576).toFixed(1) + "MB";
  }

  const DRILL_PROMPT = `PRE-ANSWER DRILL PROTOCOL v1.0

角色

你是一个回答问题前的任务理解与质量校验引擎。你的首要任务不是立即回答，而是先准确确定用户真正需要解决的问题。

强制规则

在回答任何实质性问题之前，必须完成以下三轮 DRILL。三轮各自承担不同任务，不得合并，不得跳过，不得仅重复相同内容。

DRILL 1：理解问题

分析并提取：

1. 用户明确要求完成什么？
2. 用户真正想实现的目标是什么？
3. 有哪些明确约束、背景条件和输出要求？
4. 哪些关键词可能存在歧义？
5. 解决问题所需的关键信息是否齐全？

形成第一版任务定义。

DRILL 2：质疑理解

暂时假设第一版任务定义存在错误，主动检查：

1. 是否把用户的手段误认为最终目标？
2. 是否遗漏了重要条件、上下文或限制？
3. 是否存在其他合理解释？
4. 按照第一版理解回答，是否可能答非所问？
5. 是否存在事实错误、逻辑漏洞、未经证实的假设或不切实际的方案？

发现问题时，必须明确修正第一版任务定义，而不是只罗列风险。

DRILL 3：验证任务

对修正后的任务定义进行最终检查：

1. 它是否准确回应用户的核心意图？
2. 它是否覆盖所有明确要求？
3. 是否与已有上下文一致？
4. 是否需要外部证据、计算、工具调用或进一步澄清？
5. 什么样的答案才算真正解决了用户的问题？

形成最终任务定义，并据此组织答案。

最终回答规则

* 三轮 DRILL 必须先于实质性回答完成。
* 不得声称已经核实实际上没有核实的事实。
* 不得将猜测当作事实；对重要假设应明确标注。
* 如果缺少的信息会显著改变答案，且无法合理假设，则先提出最关键的澄清问题。
* 如果信息足够，则直接回答，不要为了展示思考过程而增加冗长内容。
* 简单问题可以快速完成三轮；复杂问题必须增加必要的拆解、验证和证据检查。
* 用户要求的格式、语言、长度和其他约束必须保留。
* 不得为了完成 DRILL 而强行制造歧义、风险或额外问题。

内部完成标准

只有在以下条件同时满足时，才允许进入最终回答：

A. 核心意图明确；
B. 关键约束已提取；
C. 主要歧义已处理；
D. 第一版理解已经过反向审查；
E. 最终任务定义与用户请求一致。

如果任一条件不满足，则针对缺失项补充分析或向用户澄清。`;

  const DEEP_THINK_PROMPT = `深度思考模式（推理增强）

在给出最终回答前，先进行充分的内部推理，并在回答中展示关键推理步骤：

1. 拆解问题：列出问题的核心目标、约束条件与关键要素；
2. 多路径比较：给出两种以上的可行思路，简要比较优劣；
3. 逐步推导：沿最优路径一步一步推导，每步说明依据；
4. 自我检查：检查边界情况、反例与前提假设是否成立；
5. 结论：给出明确结论，并标注置信度与主要不确定性。

结构清晰，推理过程用有序列表呈现，最终结论放在最后。`;

  const WEB_SEARCH_PROMPT = `智能搜索模式（联网检索）

回答前先基于问题检索互联网最新信息，并在回答中引用来源：

1. 将用户问题拆解为可检索的关键词（中英文各一组）；
2. 优先检索权威来源：官方文档、一手数据、近期新闻；
3. 引用来源链接，并标注信息的时效（发布日期）；
4. 检索结果与问题不完全匹配时，明确说明差异，再基于已知作答；
5. 没有可靠结果时如实说明，绝不编造链接、数据或引文。`;

  const SYS_THINK_ID = "sk-deep-think";
  const SYS_SEARCH_ID = "sk-web-search";
  const GW_SYS_THINK_KEY = "gw_sys_think";
  const GW_SYS_SEARCH_KEY = "gw_sys_search";
  const GW_SKIP_DEFAULT_KEY = "gw_skip_default";
  const GW_DEFAULT_SKILL_KEY = "gw_default_skill";    // 旧版单选 key（只读迁移用）
  const GW_DEFAULT_SKILLS_KEY = "gw_default_skills";  // 新版多选 key
  const MAX_DEFAULT_SKILLS = 3;
  const SKILL_CATS = ["日常应用", "专业场景", "笔记整理", "模型整理"];
  const SKILL_TIER_SYSTEM = "system";

  const SYS_SKILLS = [
    { id: SYS_THINK_ID, title: "深度思考", prompt: DEEP_THINK_PROMPT, summary: "推理模式：先充分推理再作答" },
    { id: SYS_SEARCH_ID, title: "智能搜索", prompt: WEB_SEARCH_PROMPT, summary: "联网搜索：先检索再作答" },
  ];

  const state = {
    list: [],          // SKILLS（列表流可见的也在内；系统级也在，前端按 tier 过滤展示）
    active: null,      // 本轮手选 skill（composer chip）
  };

  /* ---------- 默认对话 skill（多选，≤3） ---------- */
  function defaultSkillIds() {
    try {
      const raw = config.get(GW_DEFAULT_SKILLS_KEY, "");
      if (raw) {
        const arr = JSON.parse(raw);
        if (Array.isArray(arr)) return arr.filter(x => typeof x === "string").slice(0, MAX_DEFAULT_SKILLS);
      }
      const old = config.get(GW_DEFAULT_SKILL_KEY, "");
      if (old) { config.setJSON(GW_DEFAULT_SKILLS_KEY, [old]); return [old]; }
    } catch { /* 存储不可用：当没有默认 */ }
    return [];
  }
  function saveDefaultSkillIds(ids) {
    const uniq = [...new Set(ids.filter(Boolean))].slice(0, MAX_DEFAULT_SKILLS);
    config.setJSON(GW_DEFAULT_SKILLS_KEY, uniq);
    return uniq;
  }
  function isDefaultSkill(id) { return defaultSkillIds().includes(id); }
  /// 切换默认对话（true=加入默认，false=移出）。满了返回 false 并提示。
  function toggleDefaultSkill(id) {
    const ids = defaultSkillIds();
    const i = ids.indexOf(id);
    if (i >= 0) { ids.splice(i, 1); saveDefaultSkillIds(ids); toast("已移出默认对话（可再从提示词页加回）", "ok"); return true; }
    if (ids.length >= MAX_DEFAULT_SKILLS) { toast(`默认对话最多 ${MAX_DEFAULT_SKILLS} 个，请先移出一个`, "err"); return false; }
    ids.push(id); saveDefaultSkillIds(ids); toast("已设为默认对话 skill——每次发送必带", "ok");
    return true;
  }

  /* ---------- 系统级流开关 ---------- */
  function sysSkillOn(which) {
    return config.get(which === "search" ? GW_SYS_SEARCH_KEY : GW_SYS_THINK_KEY, "") === "1";
  }
  function toggleSysSkill(which) {
    const key = which === "search" ? GW_SYS_SEARCH_KEY : GW_SYS_THINK_KEY;
    const next = config.get(key, "") === "1" ? "" : "1";
    if (next) config.set(key, next); else config.remove(key);
    const s = SYS_SKILLS.find(x => x.id === (which === "search" ? SYS_SEARCH_ID : SYS_THINK_ID));
    toast(next ? `已开启「${s.title}」——${s.summary}` : `已关闭「${s.title}」`, next ? "ok" : "");
    return !!next;
  }
  function skipDefault() { return config.get(GW_SKIP_DEFAULT_KEY, "") === "1"; }
  function toggleSkipDefault() {
    if (skipDefault()) { config.remove(GW_SKIP_DEFAULT_KEY); toast("已恢复带上默认 skill", "ok"); }
    else { config.set(GW_SKIP_DEFAULT_KEY, "1"); toast("已暂停默认 skill——设置保留，可随时恢复", ""); }
    return !skipDefault();
  }

  /* ---------- 列表流口径 ---------- */
  function skillCat(s) { return SKILL_CATS.includes(s.category) ? s.category : SKILL_CATS[0]; }
  function skillTier(s) { return s.tier === SKILL_TIER_SYSTEM ? SKILL_TIER_SYSTEM : "normal"; }
  function skillListable(s) { return skillTier(s) !== SKILL_TIER_SYSTEM; }
  function summarizePrompt(p) {
    const lines = String(p || "").split("\n").map(s => s.trim()).filter(Boolean);
    if (!lines.length) return "";
    let out = lines[0];
    if (out.length < 10 && lines[1]) out = out + " " + lines[1];
    return out.length > 30 ? out.slice(0, 30) + "…" : out;
  }

  /* ---------- 三流合并（系统级 → 默认流 → 手选，去重保序） ---------- */
  function collectSkillMsgs() {
    const out = [];
    const feats = [];
    if (sysSkillOn("think")) { out.push(SYS_SKILLS[0]); feats.push("think"); }
    if (sysSkillOn("search")) { out.push(SYS_SKILLS[1]); feats.push("search"); }
    const defIds = skipDefault() ? [] : defaultSkillIds();
    for (const id of defIds) {
      const s = state.list.find(x => x.id === id);
      if (s && s.prompt) out.push(s);
    }
    if (state.active && state.active.prompt && !defIds.includes(state.active.id)) out.push(state.active);
    const list = out.filter(s => {
      const n = skillByteLen(s.prompt);
      if (n <= SKILL_MAX) return true;
      toast(`skill「${s.title}」提示词 ${fmtSkillSize(n)} 超过 100KB 上限，本轮未带上`, "err");
      return false;
    });
    return { list, feats };
  }

  /* ---------- 装载（含种子化；旧账本零迁移补默认值） ----------
     in-flight 去重：app.js / prompts.js / setView 可能并发调用，
     不去重会双双看到空列表 → 双双种子 → 存储端加后缀生成
     sk-three-sages-2 之类的重复条目（数据完整性 bug）。 */
  let loadInFlight = null;
  async function load() {
    if (loadInFlight) return loadInFlight;
    loadInFlight = (async () => {
      try {
        let data = await gateway.admin("/skills");
        state.list = data.skills || [];
        if (state.list.length === 0) {
          try {
            await gateway.admin("/skills", { method: "POST", body: {
              id: "sk-three-sages", title: "三个诸葛亮", prompt: DRILL_PROMPT,
              summary: "回答前三角度自辩，再统一作答", category: "日常应用", tier: "normal", enabled: true,
            } });
            data = await gateway.admin("/skills");
            state.list = data.skills || [];
          } catch { /* 种子失败不阻塞 */ }
        }
        // 系统级 skill 幂等补种（老账本列表非空时上面不会触发）
        for (const sys of SYS_SKILLS) {
          if (state.list.some(s => s.id === sys.id)) continue;
          try {
            await gateway.admin("/skills", { method: "POST", body: {
              id: sys.id, title: sys.title, prompt: sys.prompt, summary: sys.summary,
              category: "日常应用", tier: "system", enabled: true,
            } });
            state.list.push({ ...sys, category: "日常应用", tier: "system", enabled: true, used_count: 0 });
          } catch { /* 补种失败不阻塞（前端常量保底注入） */ }
        }
      } catch { state.list = []; }
      for (const s of state.list) {
        if (!s.summary) s.summary = summarizePrompt(s.prompt);
        if (!SKILL_CATS.includes(s.category)) s.category = SKILL_CATS[0];
        if (s.tier !== SKILL_TIER_SYSTEM) s.tier = "normal";
      }
      return state.list;
    })();
    try { return await loadInFlight; }
    finally { loadInFlight = null; }
  }

  /* ---------- CRUD（B3 提示词页用；壳层先备边界） ---------- */
  const api = {
    create(body) { return gateway.admin("/skills", { method: "POST", body }); },
    update(id, body) { return gateway.admin("/skills/" + encodeURIComponent(id), { method: "PUT", body }); },
    remove(id) { return gateway.admin("/skills/" + encodeURIComponent(id), { method: "DELETE" }); },
    use(id) { return gateway.admin("/skills/" + encodeURIComponent(id) + "/use", { method: "POST", body: {} }).catch(() => {}); },
  };

  global.QuakeSkills = {
    state, SYS_SKILLS, SYS_THINK_ID, SYS_SEARCH_ID, SKILL_CATS, SKILL_TIER_SYSTEM, MAX_DEFAULT_SKILLS,
    DRILL_PROMPT, DEEP_THINK_PROMPT, WEB_SEARCH_PROMPT,
    defaultSkillIds, saveDefaultSkillIds, isDefaultSkill, toggleDefaultSkill,
    sysSkillOn, toggleSysSkill, skipDefault, toggleSkipDefault,
    skillCat, skillTier, skillListable, summarizePrompt,
    collectSkillMsgs, load, api,
  };
})(window);
