// ResourceStore：资源账本 Durable Object（JS 实现）。
//
// 为什么用 JS 而不是 Rust #[durable_object]：
// worker-build 的 DO 原型链包装只处理 wasm-bindgen 产出的 `export class X`
// 声明，而本项目的 DO 类经 re-export + Proxy 导出，workerd 静态识别不到。
// 直接在 worker 入口模块里 `export class ResourceStore extends DurableObject`
// 是最稳的注册方式。Rust 主线通过 stub.fetch_with_request 走同一套 JSON
// 协议读写（见 src/store.rs 的 store_rpc）。
//
// 构建时由 scripts/build.sh 追加到 build/index.js 末尾。

import { DurableObject } from "cloudflare:workers";

// 保活间隔：本地 workerd 的 inMemory storage 会随 DO 实例 idle 逐出而丢失
// （与 schema 文档"persist for the lifetime of the process"不符的已知行为）。
// 每 5s 的 alarm 让实例持续被唤醒， storage 数据得以留存。
// 生产 Cloudflare 上 storage 本就跨实例持久，alarm 保活只是少量空唤醒，
// 对免费额度影响可忽略；若不在意本地重启保留数据，可删掉本保活逻辑。
const KEEPALIVE_MS = 5_000;

export class ResourceStore extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      if ((await ctx.storage.getAlarm()) === null) {
        await ctx.storage.setAlarm(Date.now() + KEEPALIVE_MS);
      }
    });
  }

  async alarm() {
    await this.ctx.storage.setAlarm(Date.now() + KEEPALIVE_MS);
  }

  async fetch(req) {
    const path = new URL(req.url).pathname;
    const st = this.ctx.storage;
    if (req.method === "GET" && path === "/channels") {
      const list = (await st.get("channels")) || [];
      return Response.json({ channels: list });
    }
    if (req.method === "PUT" && path === "/channels") {
      const body = await req.json();
      await st.put("channels", Array.isArray(body) ? body : (body.channels || []));
      return Response.json({ ok: true });
    }
    if (req.method === "GET" && path === "/events") {
      const list = (await st.get("events")) || [];
      return Response.json({ events: list });
    }
    if (req.method === "POST" && path === "/events") {
      const ev = await req.json();
      const list = ((await st.get("events")) || []).slice();
      list.push(ev);
      if (list.length > 50) list.splice(0, list.length - 50); // 环形账本，最多 50 条
      await st.put("events", list);
      return Response.json({ ok: true });
    }
    // 用量成本账本：每请求一条（源 × 模型 × token × 延迟 × 成本）
    if (req.method === "GET" && path === "/usage") {
      const list = (await st.get("usage")) || [];
      return Response.json({ usage: list });
    }
    if (req.method === "POST" && path === "/usage") {
      const rec = await req.json();
      const list = ((await st.get("usage")) || []).slice();
      list.push(rec);
      // 环形账本：按天保留（保留天数由调用方传入 ?keep_days=，缺省 30 天）
      const keep = Number(new URL(req.url).searchParams.get("keep_days") || 30);
      const cap = Math.max(1, Math.min(400, keep)) * 400; // 每天最多 400 条
      if (list.length > cap) list.splice(0, list.length - cap);
      await st.put("usage", list);
      return Response.json({ ok: true, size: list.length });
    }
    if (req.method === "DELETE" && path === "/usage") {
      await st.delete("usage");
      return Response.json({ ok: true });
    }
    // skill 账本（独立于模型的表达线：标题 + 提示词正文）。
    // 与 /channels 同构：全量 GET / PUT。单条增删改由 admin.rs 读-改-写完成。
    if (req.method === "GET" && path === "/skills") {
      const list = (await st.get("skills")) || [];
      return Response.json({ skills: list });
    }
    if (req.method === "PUT" && path === "/skills") {
      const body = await req.json();
      await st.put("skills", Array.isArray(body) ? body : (body.skills || []));
      return Response.json({ ok: true });
    }
    return new Response("unsupported store path", { status: 404 });
  }
}
