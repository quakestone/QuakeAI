#!/usr/bin/env node
// 发送流水线（send_pipeline）行为测试——不靠 grep，真跑逻辑：
//   ① 真加载 src/engine_wasm.bin 内核，加载 WF_SEED_SEND 种子跑一条 run，
//      用记录型 mock 插件验证 **串行语义**（每批就绪步恰好 1 个、顺序 stt→video→image）；
//   ② 验证「插件返回 {ok:false} 不杀 run」与「插件 throw 才杀 run」的内核语义；
//   ③ 抽测 MediaFSM 状态词汇 / 阶段表 / buildApiContent 的 desc 感知 / modelHasVision。
//
// 为什么值得单测：串行依赖是 depends_on 链保证的，一旦种子写错（漏依赖 → 三步同时
// 就绪变成并行），静态断言全过但流水线语义已坏——这里用执行顺序真实卡住它。
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const html = fs.readFileSync(path.join(ROOT, "src", "admin_ui.html"), "utf8");

let PASS = 0, FAIL = 0;
function check(name, cond, detail) {
  if (cond) { PASS++; console.log("  [PASS] " + name); }
  else { FAIL++; console.log("  [FAIL] " + name + (detail ? " -- " + detail : "")); }
}

// ---------- 片段抽取 ----------
function extractFn(name) {
  const start = html.indexOf("function " + name + "(");
  if (start < 0) return null;
  const end = html.indexOf("\n}", start);
  return html.slice(start, end + 2);
}
/// 抽 `const NAME = <字面量>;`：括号配平扫描（兼容单行对象与多行数组，不吃后续代码）
function extractConst(name) {
  const start = html.indexOf("const " + name + " =");
  if (start < 0) return null;
  let i = html.indexOf("=", start) + 1, depth = 0, inStr = null;
  for (; i < html.length; i++) {
    const c = html[i];
    if (inStr) { if (c === inStr && html[i - 1] !== "\\") inStr = null; continue; }
    if (c === '"' || c === "'" || c === "`") { inStr = c; continue; }
    if (c === "{" || c === "[") depth++;
    else if (c === "}" || c === "]") { depth--; if (depth === 0) break; }
  }
  let end = html.indexOf(";", i);
  if (end < 0) end = i + 1;
  return html.slice(start, end + 1);
}
const seedMatch = html.match(/const WF_SEED_SEND = (JSON\.stringify\([\s\S]*?\]\s*\}\);)/);
if (!seedMatch) { console.error("✗ 无法定位 WF_SEED_SEND 种子"); process.exit(1); }

console.log("== ① WASM 内核：send_pipeline 串行语义 ==");

const wasmBytes = fs.readFileSync(path.join(ROOT, "src", "engine_wasm.bin"));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
ex.mem_reserve(1024 * 1024);

function wfStr(s) {
  const b = new TextEncoder().encode(s);
  const p = ex.alloc(b.length);
  new Uint8Array(ex.memory.buffer, p, b.length).set(b);
  return [p, b.length];
}
function wfErr() {
  const buf = new Uint8Array(ex.memory.buffer, ex.last_error_ptr(), ex.last_error_len());
  return new TextDecoder().decode(buf);
}
function readScratch() {
  const buf = new Uint8Array(ex.memory.buffer, ex.scratch_ptr(), ex.scratch_len());
  try { return JSON.parse(new TextDecoder().decode(buf)); } catch { return {}; }
}

// 加载种子（与前端 WF.ensure 的 load() 同路：种子本身就是 JSON 文本，直接编码传入）
const WF_SEED_SEND = new Function("return " + seedMatch[1])();
const hSeed = ex.engine_new();
{
  const [p, l] = wfStr(WF_SEED_SEND);
  const r = ex.engine_load(hSeed, p, l);
  check("种子通过内核校验（DAG 合法）", r === 0, r !== 0 ? wfErr() : "");
}

/// 驱动一条 run：mock 插件记录 (stepId, batchSize)；返回 { order, batches, status }
async function driveRun(event, plugin) {
  ex.arena_reset();
  const [ep, el] = wfStr(JSON.stringify(event));
  const rid = ex.engine_start(hSeed, ep, el);
  if (rid < 0) throw new Error("start: " + wfErr());
  const order = [], batches = [];
  let guard = 0;
  for (;;) {
    if (++guard > 16) throw new Error("驱动超过步数上限");
    if (!ex.engine_ready(rid)) throw new Error("ready: " + wfErr());
    const blk = readScratch();
    const steps = blk.steps || [];
    if (!steps.length) break;
    batches.push(steps.length);
    for (const st of steps) {
      order.push(st.id);
      try {
        const out = await plugin(st.id, st.input);
        const [sp, sl] = wfStr(st.id);
        const [op, ol] = wfStr(JSON.stringify(out ?? null));
        if (ex.engine_complete(rid, sp, sl, op, ol) !== 0) throw new Error("complete: " + wfErr());
      } catch (e) {
        const [sp, sl] = wfStr(st.id);
        const [ep2, el2] = wfStr(String((e && e.message) || e));
        const canRetry = ex.engine_fail(rid, sp, sl, ep2, el2);
        if (canRetry !== 1 && canRetry !== true) break;
      }
    }
  }
  // engine_status(run_id) 成功时把状态 JSON 写进 scratch（与前端 readScratch 同路）
  if (ex.engine_status(rid) !== 1) throw new Error("status: " + wfErr());
  const st = readScratch();
  return { order, batches, status: st.status };
}

// 场景 1：三类附件齐全 → 严格串行
{
  const seen = [];
  const r = await driveRun(
    { event: "send.start", atts: [{ kind: "audio", name: "a.wav" }, { kind: "video", name: "v.mp4" }, { kind: "image", name: "p.png" }], imageDirect: false },
    async (sid) => { seen.push(sid); return { ok: true }; });
  check("三步全部执行", r.order.length === 3, JSON.stringify(r.order));
  check("执行顺序 stt→video→image（depends_on 链串行）", r.order.join(",") === "stt,video,image", r.order.join(","));
  check("每批就绪步恰好 1 个（无并行泄漏）", r.batches.every(b => b === 1), JSON.stringify(r.batches));
  check("run 终态 DONE", r.status === 1, "status=" + r.status);
}

// 场景 2：无附件 → 三步仍走完（插件自行 skipped），不卡死
{
  const r = await driveRun({ event: "send.start", atts: [], imageDirect: false }, async () => ({ skipped: true }));
  check("空附件：三步照常完成（skipped 不阻塞依赖）", r.order.join(",") === "stt,video,image" && r.status === 1, r.order.join(","));
}

// 场景 3：imageDirect=true 时第③步拿到 direct 标记（模板 ${/event/imageDirect} 解析）
{
  let gotDirect = null;
  await driveRun({ event: "send.start", atts: [{ kind: "image", name: "p.png" }], imageDirect: true },
    async (sid, inp) => { if (sid === "image") gotDirect = inp.direct; return { ok: true }; });
  check("事件字段透传：image 步收到 direct=true", gotDirect === true, "got=" + gotDirect);
}

// 场景 4：插件返回 {ok:false}（降级信号）→ run 继续，不杀
{
  const r = await driveRun({ event: "send.start", atts: [{ kind: "audio", name: "a.wav" }], imageDirect: false },
    async (sid) => (sid === "stt" ? { ok: false, error: "识别失败" } : { ok: true }));
  check("插件 ok:false 不杀 run（降级由上层决定）", r.status === 1 && r.order.length === 3, "status=" + r.status);
}

// 场景 5：插件 throw（真异常）→ run FAILED，后续步 SKIPPED（这就是插件必须自吞异常的原因）
{
  const r = await driveRun({ event: "send.start", atts: [{ kind: "audio", name: "a.wav" }], imageDirect: false },
    async (sid) => { if (sid === "stt") throw new Error("boom"); return { ok: true }; });
  check("插件 throw → run FAILED（内核重试语义）", r.status === 2, "status=" + r.status);
  check("失败后后续步不再执行", r.order.length === 1, JSON.stringify(r.order));
}

console.log("== ② MediaFSM 状态词汇与阶段表 ==");
{
  const FSM = new Function(extractConst("FSM") + "\nreturn FSM;")();
  const PIPE_STAGES = new Function(extractConst("PIPE_STAGES") + "\nreturn PIPE_STAGES;")();
  const want = ["ready", "queued", "working", "done", "failed", "skipped"];
  check("FSM 六态齐全", want.every(w => Object.values(FSM).includes(w)), JSON.stringify(Object.values(FSM)));
  check("阶段顺序 文本→语音→视频→图片→回答", PIPE_STAGES.map(s => s.key).join(",") === "text,stt,video,image,chat", JSON.stringify(PIPE_STAGES.map(s => s.key)));
  const FSM_MARK = new Function(extractConst("FSM_MARK") + "\nreturn FSM_MARK;")();
  check("每态都有展示标记", want.every(w => FSM_MARK[w]), JSON.stringify(FSM_MARK));
}

console.log("== ③ buildApiContent：desc 感知（直连/降级/占位） ==");
{
  const fmtSize = (b) => { if (b == null) return ""; if (b < 1024) return b + "B"; if (b < 1048576) return (b / 1024).toFixed(0) + "KB"; return (b / 1048576).toFixed(1) + "MB"; };
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const buildApiContent = new Function("fmtSize", "esc", extractFn("buildApiContent") + "\nreturn buildApiContent;")(fmtSize, esc);

  // 图片无 desc → vision parts 直连
  const direct = buildApiContent({ content: "看图", attachments: [{ kind: "image", name: "a.png", dataUrl: "data:image/png;base64,XX" }] });
  check("图片无 desc → vision parts 直连", Array.isArray(direct) && direct.some(p => p.type === "image_url"), JSON.stringify(direct));

  // 图片有 desc → 降级为文字注记，不再直连
  const degraded = buildApiContent({ content: "看图", attachments: [{ kind: "image", name: "a.png", dataUrl: "data:image/png;base64,XX", desc: "一只猫" }] });
  check("图片有 desc → 「图片 xxx 理解」注记", typeof degraded === "string" && degraded.includes("[图片 a.png 理解：一只猫]"), degraded);
  check("图片降级后不含 image_url parts", typeof degraded === "string" && !degraded.includes("image_url"), degraded);

  // 视频有 desc → 理解注记
  const vDesc = buildApiContent({ content: "", attachments: [{ kind: "video", name: "v.mp4", size: 1048576, desc: "一个人在跑步" }] });
  check("视频有 desc → 「视频 xxx 理解」注记", vDesc.includes("[视频 v.mp4 理解：一个人在跑步]"), vDesc);

  // 视频无 desc → 老占位
  const vPlain = buildApiContent({ content: "", attachments: [{ kind: "video", name: "v.mp4", size: 1048576 }] });
  check("视频无 desc → 文件名占位（保持旧观感）", vPlain.includes("[附件: v.mp4（1.0MB）]"), vPlain);

  // 无附件 → 原样返回正文
  check("无附件 → 正文原样", buildApiContent({ content: "hi", attachments: [] }) === "hi");
}

console.log("== ④ modelHasVision：智能混合路由决策 ==");
{
  const modelHasVision = new Function("modelsByCap", extractFn("modelHasVision") + "\nreturn modelHasVision;");
  const withVision = (name) => modelHasVision.call(null, () => [{ name: "glm-4v" }, { name: "qwen-vl" }])(name);
  check("空模型（自动）+ 池有视觉模型 → 直连", withVision("") === true);
  check("显式选视觉模型 → 直连", withVision("glm-4v") === true);
  check("显式选纯文本模型 → 降级", withVision("glm-4-flash") === false);
  const noVision = modelHasVision.call(null, () => []);
  check("空模型 + 池无视觉模型 → 降级", noVision("") === false);
}

// ⑤ v0.19.23 SKILL 选择器抽测：二级分类过滤 + 系统级不罗列 + 各分类独立页码 + 分页/搜索。
//    v0.19.22 起这里是纯渲染（输入 → HTML 字符串），用 stub DOM 真跑，比 grep 结实。
console.log("== ⑤ v0.19.23：SKILL 分类/台/分页 + 附件双形态 ==");
{
  const mkPicker = new Function("$", "SKILLS", "SKILL_CATS", "activeSkill", "defaultSkillId", "mpQuery",
    "mpSkillCat", "mpSkillPages", "MP_PAGE_SIZE", "esc", "openSkillDialog", "summarizePrompt", "SKILL_LOGO",
    "skillCat", "skillListable",
    extractFn("skCatBar") + "\n" + extractFn("setSkCat") + "\n" + extractFn("renderSkillPicker") + "\nreturn renderSkillPicker;");
  const mkBtn = () => ({ disabled: false });
  const mkEl = () => ({ innerHTML: "", textContent: "", hidden: false, querySelectorAll: () => [mkBtn(), mkBtn()] });
  const els = { "model-list": mkEl(), "model-pager": mkEl(), "model-page-n": mkEl() };
  const $ = (id) => els[id] || null;
  const esc = (s) => String(s);
  const SKILL_CATS = ["日常应用", "专业场景", "笔记整理", "模型整理"];
  const skillCat = (s) => SKILL_CATS.includes(s.category) ? s.category : SKILL_CATS[0];
  const skillTier = (s) => s.tier === "system" ? "system" : "normal";
  const skillListable = (s) => skillTier(s) !== "system";
  const mkSkills = (n, cat, tier) => Array.from({ length: n }, (_, i) =>
    ({ id: "s" + i, title: "技能" + i, prompt: "p" + i, summary: "简" + i, used_count: n - i, enabled: true, category: cat || "日常应用", tier: tier || "normal" }));
  const run = (skills, q, page, cat) => {
    const c = cat || "日常应用";
    const mpSkillPages = { [c]: page || 1 };
    // 双调用：先以桩参数取到内层函数，再执行（内层函数自身无参，全靠闭包读桩）
    mkPicker.call(null, $, skills, SKILL_CATS, null, () => "", q || "", c, mpSkillPages, 5, esc, () => {}, (p) => "摘要", "<svg/>", skillCat, skillListable)();
    return { list: els["model-list"].innerHTML, pager: els["model-pager"].hidden, pageN: els["model-page-n"].textContent };
  };
  const nItem = (h) => (h.match(/sk-item/g) || []).length;
  const r8 = run(mkSkills(8), "", 1);
  check("⑤ 8 条 SKILL 首页只渲染 5 条（限 5）", nItem(r8.list) === 5, "got " + nItem(r8.list));
  check("⑤ 分页条显示且标注「1 / 2 · 共 8 个」", r8.pager === false && r8.pageN === "1 / 2 · 共 8 个", r8.pageN);
  const r8p2 = run(mkSkills(8), "", 2);
  check("⑤ 第 2 页渲染剩余 3 条", nItem(r8p2.list) === 3 && r8p2.pageN === "2 / 2 · 共 8 个", r8p2.pageN);
  const r3 = run(mkSkills(3), "", 1);
  check("⑤ 3 条 SKILL 单页隐藏分页条", r3.pager === true && nItem(r3.list) === 3);
  const rq = run(mkSkills(8), "技能7", 1);
  check("⑤ 搜索命中 1 条且隐藏分页条", nItem(rq.list) === 1 && rq.pager === true);
  const rmiss = run(mkSkills(8), "不存在", 1);
  check("⑤ 搜索无命中给空态提示（不展示陈旧列表）", rmiss.list.includes("没有匹配") && nItem(rmiss.list) === 0);
  // —— v0.19.23 新增：二级分类 + 台属性 ——
  const nCat = (h) => (h.match(/class="sk-cat[\s"]/g) || []).length;
  check("⑤ 分类标签条渲染四个二级分类", nCat(r8.list) === 4, "got " + nCat(r8.list));
  check("⑤ 当前分类高亮（sel）", (r8.list.match(/sk-cat sel/g) || []).length === 1);
  const rOther = run(mkSkills(3, "专业场景"), "", 1, "专业场景");
  check("⑤ 按分类过滤：专业场景分类只渲染该分类条目", nItem(rOther.list) === 3);
  const rMixed = run([...mkSkills(2, "日常应用"), ...mkSkills(3, "专业场景")], "", 1, "专业场景");
  check("⑤ 混合数据按当前分类过滤（只出 3 条专业场景）", nItem(rMixed.list) === 3, "got " + nItem(rMixed.list));
  const rSys = run([...mkSkills(2, "日常应用"), ...mkSkills(2, "日常应用", "system")], "", 1, "日常应用");
  check("⑤ 系统级 skill 不罗列在列表流", nItem(rSys.list) === 2, "got " + nItem(rSys.list));
  const rEmpty = run([], "", 1);
  check("⑤ 分类下真空态给分类名提示", rEmpty.list.includes("「日常应用」分类下还没有 SKILL"));
  const r1 = run(mkSkills(1), "", 1);
  check("⑤ 条目用 SKILL_LOGO 徽标（非 🧩）", r1.list.includes('<span class="sk-rank"><svg/></span>') && !r1.list.includes("🧩"));
  check("⑤ 新增入口为大写「＋ 扩展 SKILL」", r1.list.includes("＋ 扩展 SKILL（新增）"));
  const rOver = run(mkSkills(2), "", 9);
  // 页码越界钳到末页：条目数与隐藏态是契约；page-n 的陈旧文本在隐藏态不可见
  // （与模型列表渲染器同模式，不苛求）
  check("⑤ 页码越界钳到末页（渲染全部条目且分页条隐藏）", nItem(rOver.list) === 2 && rOver.pager === true, JSON.stringify(rOver));
}
{
  const mkAtts = new Function("$", "PIPE", "attachments", "esc",
    extractFn("renderAttachments") + "\nreturn renderAttachments;");
  const bar = { innerHTML: "", hidden: true };
  const $ = (id) => (id === "attach-bar" ? bar : null);
  const esc = (s) => String(s);
  const PIPE = { visible: false, atts: [] };
  const atts = [
    { kind: "image", name: "a.png", dataUrl: "data:image/png;base64,xx", st: "working" },
    { kind: "audio", name: "b.wav", st: "" },
    { kind: "video", name: "c.mp4", poster: "data:image/jpeg;base64,yy" },
  ];
  const render = (list) => mkAtts.call(null, $, PIPE, list, esc)();
  render(atts);
  check("⑤ 图片走 att-pic 小方片", bar.innerHTML.includes('class="att-chip att-pic"'));
  check("⑤ 视频首帧也走 att-pic 小方片", (bar.innerHTML.match(/att-pic/g) || []).length === 2);
  check("⑤ 音频走 att-file 小 pill 且带文件名", bar.innerHTML.includes('class="att-chip att-file"') && bar.innerHTML.includes(">b.wav<"));
  check("⑤ 图片 chip 无名称行（名称收进 title）", !/<div class="att-nm">/.test(bar.innerHTML) && bar.innerHTML.includes('title="a.png"'));
  check("⑤ working 态徽标渲染", bar.innerHTML.includes('data-st="working"') && bar.innerHTML.includes('class="att-st"'));
  check("⑤ 有附件时附件栏展开", bar.hidden === false);
  PIPE.visible = true; PIPE.atts = atts;   // 流水线期间：输入区 attachments 已清空
  render([]);
  check("⑤ 流水线期间回退渲染 PIPE.atts（chip 不消失）", bar.innerHTML.includes("att-pic"));
  check("⑤ 流水线期间不渲染删除钮（防误删）", !bar.innerHTML.includes("att-x"));
  PIPE.visible = false;
  render([]);
  check("⑤ 无附件时附件栏收起", bar.hidden === true);
}

console.log(`\n结果: PASS=${PASS} FAIL=${FAIL}`);
process.exit(FAIL > 0 ? 1 : 0);
