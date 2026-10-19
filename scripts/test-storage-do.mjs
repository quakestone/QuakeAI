// storage-do.js 的 node 协议自测：模拟 DO storage，验证 channels/events 四个端点。
// 用法：node scripts/test-storage-do.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";

// ---- 最小 DO 运行时模拟 ----
class MemStorage {
  constructor() { this.m = new Map(); this._alarm = null; }
  async get(k) { return this.m.get(k); }
  async put(k, v) { this.m.set(k, v); }
  async getAlarm() { return this._alarm; }
  async setAlarm(t) { this._alarm = t; }
}
class DurableObject {
  constructor(ctx, env) { this.ctx = ctx; this.env = env; }
}
const ctx = {
  storage: new MemStorage(),
  blockConcurrencyWhile(p) { this._p = p; },
};

const src = readFileSync(new URL("./storage-do.js", import.meta.url), "utf-8");
const sandbox = vm.createContext({
  DurableObject, Response, Request, URL, Date, console,
  exports: {},
});
vm.runInContext(src.replace(/^import .*$/m, "").replace(/^export class /m, "class "), sandbox);
const ResourceStore = vm.runInContext("ResourceStore", sandbox);

let pass = 0, fail = 0;
const check = (name, ok, detail = "") => { ok ? pass++ : fail++; console.log(`  [${ok ? "PASS" : "FAIL"}] ${name}${ok ? "" : " -- " + detail}`); };

const store = new ResourceStore(ctx, {});
await ctx._p; // blockConcurrencyWhile

// 空读
let r = await store.fetch(new Request("http://resource-store/channels"));
let j = await r.json();
check("GET /channels 空账本返回 []", Array.isArray(j.channels) && j.channels.length === 0);

// 写两条渠道
const chans = [
  { id: "a", provider: "zhipu" },
  { id: "b", provider: "siliconflow" },
];
r = await store.fetch(new Request("http://resource-store/channels", { method: "PUT", body: JSON.stringify(chans) }));
j = await r.json();
check("PUT /channels 返回 ok", j.ok === true);
r = await store.fetch(new Request("http://resource-store/channels"));
j = await r.json();
check("GET /channels 读回 2 条", j.channels.length === 2 && j.channels[1].provider === "siliconflow");

// 事件账本：环形 50 条
for (let i = 0; i < 60; i++) {
  await store.fetch(new Request("http://resource-store/events", { method: "POST", body: JSON.stringify({ ts: `t${i}`, result: "failed" }) }));
}
r = await store.fetch(new Request("http://resource-store/events"));
j = await r.json();
check("事件账本环形截断为 50 条", j.events.length === 50, `got ${j.events.length}`);
check("保留最新（最旧 10 条被挤出）", j.events[0].ts === "t10" && j.events[49].ts === "t59");

// 未知路径 404
r = await store.fetch(new Request("http://resource-store/whatever"));
check("未知路径 404", r.status === 404, `got ${r.status}`);

// alarm 保活
await store.alarm();
check("alarm 保活已设置", typeof ctx.storage._alarm === "number" && ctx.storage._alarm > 0);

console.log(`\n结果: PASS=${pass} FAIL=${fail}`);
process.exit(fail ? 1 : 0);
