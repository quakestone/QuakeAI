#!/usr/bin/env node
// 模型积分账本的行为测试（真跑一遍计分逻辑，不靠 grep 断言）。
//
// 为什么单独写：曾经出现过「POINTS_RULE 写 select、调用传 sel」的错位，
// 所有静态断言（grep 到 Points.add / POINTS_RULE）全都通过，但积分永远记不上。
// 静态断言只能证明"代码在那里"，证明不了"它算得对"。
//
// 做法：从 admin_ui.html 里抽出 Points 模块与相关常量，原样 eval 后驱动它。
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const html = fs.readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "src", "admin_ui.html"), "utf8");

// 抽片段：从 "const POINTS_KEY" 到 "Points.load();" 那一段
const start = html.indexOf("const POINTS_KEY");
const endMark = "Points.load();";
const end = html.indexOf(endMark, start);
if (start < 0 || end < 0) {
  console.error("✗ 无法在 admin_ui.html 里定位 Points 模块");
  process.exit(1);
}
const snippet = html.slice(start, end + endMark.length);

// 伪 localStorage（内存版）
const store = new Map();
global.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};

let PASS = 0, FAIL = 0;
function check(name, cond, detail) {
  if (cond) { PASS++; console.log("  [PASS] " + name); }
  else { FAIL++; console.log("  [FAIL] " + name + (detail ? " -- " + detail : "")); }
}

// eval 抽取出的模块（用 Function 隔离作用域，但把 Points 暴露出来）
const Points = new Function(snippet + "\nreturn Points;")();

console.log("== 积分账本行为测试 ==");

// 1) 初始为空
check("初始账本为空", Object.keys(Points.data.models).length === 0);

// 2) 选择 +1
Points.add("sel", "glm-4-flash");
check("选择一次 → 积分 +1", Points.score("glm-4-flash") === 1, "score=" + Points.score("glm-4-flash"));

// 3) 使用 +1
Points.add("use", "glm-4-flash");
check("使用一次 → 累计 +2", Points.score("glm-4-flash") === 2, "score=" + Points.score("glm-4-flash"));

// 4) 分享 +10
Points.add("share", "glm-4-flash");
check("分享一次 → 累计 +12", Points.score("glm-4-flash") === 12, "score=" + Points.score("glm-4-flash"));

// 5) 降权 −20
Points.add("degrade", "glm-4-flash");
check("降权一次 → 累计 −8", Points.score("glm-4-flash") === -8, "score=" + Points.score("glm-4-flash"));

// 6) 自动条目独立计分
Points.add("sel", "");
Points.add("use", "");
check("自动条目独立计分（选择+使用 = 2）", Points.score("__auto__") === 2, "score=" + Points.score("__auto__"));

// 7) 降权可以压到负分（分高的排前面，低的沉底）
Points.add("degrade", "glm-4-flash");
check("多次降权可压到负分", Points.score("glm-4-flash") === -28, "score=" + Points.score("glm-4-flash"));

// 8) 倒序排序：分高的在前
const list = Points.list();
const scores = list.map((x) => x.score);
check("倒序排列（分高在前）", scores.every((s, i) => i === 0 || scores[i - 1] >= s), JSON.stringify(scores));

// 9) 持久化：重新 load 后仍在
const reloaded = new Function(snippet + "\nreturn Points;")();
check("localStorage 持久化（重载后积分仍在）", reloaded.score("glm-4-flash") === -28, "score=" + reloaded.score("glm-4-flash"));

// 10) 重置
reloaded.reset();
check("reset 清空账本", Object.keys(reloaded.data.models).length === 0 && reloaded.score("glm-4-flash") === 0);

// 11) 未知计分键被拒（防键名错位静默失败）
const before = reloaded.score("x");
reloaded.add("select", "x"); // 旧错位用的键名
check("未知计分键被拒绝（不静默记错）", reloaded.score("x") === before, "score=" + reloaded.score("x"));

// 12) 未知模型名也能建条目（key 统一用模型名）
reloaded.add("sel", "deepseek-flash");
check("任意模型名可独立建条目", reloaded.score("deepseek-flash") === 1);

console.log(`\n结果: PASS=${PASS} FAIL=${FAIL}`);
process.exit(FAIL === 0 ? 0 : 1);
