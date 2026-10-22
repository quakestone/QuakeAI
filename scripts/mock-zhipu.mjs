// 本地 mock：模拟智谱 BigModel OpenAI 兼容 API 的关键行为
//  - POST /chat/completions      ：校验 Authorization；stream=true 返回 SSE；否则 JSON
//  - POST /fail/chat/completions ：首包前失败①——HTTP 502（网关应立即换通道）
//  - POST /flaky-empty/...       ：首包前失败②——200 头 + 立即断连，一帧不发（首帧前中断）
//  - POST /flaky/chat/completions：已吐字后断流——发 2 帧后 destroy（网关不得切换）
//  - POST /audio/transcriptions ： multipart 上传（解析 boundary/part，返回转写结果）
//  - GET  /models
import { createServer } from "node:http";
import { appendFileSync } from "node:fs";

const PORT = 8788;
const VALID_KEY = "test-key-local";
const LOG = new URL("../mock-received.log", import.meta.url).pathname;

const server = createServer((req, res) => {
  // 浏览器 e2e：页面与 mock 不同源，WASM 侧 web_sys::fetch 直连需要 CORS 放行
  // （预检 + 简单请求均覆盖；仅测试基建使用，不影响 Node 侧 curl 调用）。
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET,POST,OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type,Authorization,x-gw-target,x-gw-features,x-admin-token,x-device-keys");
  if (req.method === "OPTIONS") { res.writeHead(204); res.end(); return; }
  // 二进制安全：Buffer 累积，latin1 保持多字节 1:1（multipart 分隔/头为 ASCII）
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  req.on("end", () => {
    const body = Buffer.concat(chunks).toString("latin1");
    const isFail = req.url?.startsWith("/fail/");
    const isFlakyEmpty = req.url?.startsWith("/flaky-empty/");
    const isFlaky = req.url?.startsWith("/flaky/");
    // B4：模拟用户自建「上游转发服务」——配了 gw_upstream_proxy 后，网关把请求
    // 发到这里、真实上游地址放在 x-gw-target 头。与 /chat/completions 同一处理器。
    const isProxy = req.url?.startsWith("/proxy");
    appendFileSync(
      LOG,
      JSON.stringify({
        ts: new Date().toISOString(),
        method: req.method,
        url: req.url,
        authorization: req.headers["authorization"] ?? null,
        contentType: req.headers["content-type"] ?? null,
        bodyLength: body.length,
        // B4：x-gw-target —— 配代理后网关转交的真实上游地址（断言代理链路用）
        gwTarget: req.headers["x-gw-target"] ?? null,
        proxyReceived: isProxy || undefined,
        // v0.19.25 测试基建：chat 请求摘要——验证网关侧系统级流参数注入
        // （thinking / web_search tools）与三流 system 消息合并。
        // B1 增补：x-gw-features 头 + 消息清单（ctx 边界 / 附件 parts 验证用）。
        // B4 增补：/proxy 路径（配了 gw_upstream_proxy 的上游转发）同样摘要素引。
        chatDigest: ((req.url?.includes("/chat/completions") || isProxy) ? (() => {
          try {
            // body 以 latin1 累积（二进制安全），解析前按 utf8 解码回原文——
            // 否则中文成 mojibake，system 提示词等断言无法匹配。
            const p = JSON.parse(Buffer.from(body, "latin1").toString("utf8"));
            const head = (c) => typeof c === "string" ? c.slice(0, 24)
              : (Array.isArray(c) ? "[" + c.map(x => x.type).join(",") + "]" : String(c).slice(0, 24));
            return {
              model: p.model ?? null,
              thinking: p.thinking ?? null,
              tools: p.tools ?? null,
              gwFeatures: req.headers["x-gw-features"] ?? null,
              systemCount: (p.messages || []).filter(m => m.role === "system").length,
              systemHeads: (p.messages || []).filter(m => m.role === "system").map(m => String(m.content).slice(0, 20)),
              msgHeads: (p.messages || []).map(m => m.role + ":" + head(m.content)),
            };
          } catch { return null; }
        })() : undefined),
      }) + "\n"
    );

    if (req.url === "/models" && req.method === "GET") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          object: "list",
          data: [
            { id: "glm-4-flash", object: "model", owned_by: "zhipu" },
            { id: "glm-4.7-flash", object: "model", owned_by: "zhipu" },
          ],
        })
      );
      return;
    }

    // ---- 故障路径：首包前失败（502） ----
    if (isFail && req.method === "POST") {
      res.writeHead(502, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: { code: "500", message: "mock upstream 502" } }));
      return;
    }

    if (req.url === "/chat/completions" && req.method === "POST" ||
        isFlaky || isFlakyEmpty || isProxy) {
      const auth = req.headers["authorization"];
      // 智谱 v4 端点两种鉴权都接受（模拟真实行为）：
      //   1. 原始 API Key（Bearer {id}.{secret}）
      //   2. 网关动态生成的短期 JWT（Bearer <header>.<payload>.<sig>）
      //      —— 解码验证智谱规范：header {"alg":"HS256","sign_type":"SIGN"}，
      //      payload {"api_key":{id},"exp":…,"timestamp":…}（毫秒 epoch）。
      const b64urlJson = (s) => {
        const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
        return JSON.parse(Buffer.from(b64, "base64").toString("utf8"));
      };
      const verifyZhipuJwt = (bearer) => {
        const parts = bearer.split(".");
        if (parts.length !== 3) return null;
        try {
          const header = b64urlJson(parts[0]);
          const payload = b64urlJson(parts[1]);
          if (header.alg !== "HS256" || header.sign_type !== "SIGN") return null;
          if (!payload.api_key || typeof payload.exp !== "number" ||
              typeof payload.timestamp !== "number") return null;
          return payload;
        } catch {
          return null;
        }
      };
      let jwtPayload = null;
      if (auth?.startsWith("Bearer ")) {
        const token = auth.slice(7);
        if (token === VALID_KEY) {
          // raw key 路径
        } else if ((jwtPayload = verifyZhipuJwt(token))) {
          console.log(JSON.stringify({
            ts: new Date().toISOString(), op: "jwt-verified",
            api_key: jwtPayload.api_key,
            ttl_ms: jwtPayload.exp - jwtPayload.timestamp,
          }));
        } else {
          res.writeHead(401, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ error: { code: "1001", message: "auth failed" } }));
          return;
        }
      } else {
        res.writeHead(401, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { code: "1001", message: "auth failed" } }));
        return;
      }
      let payload = {};
      try { payload = JSON.parse(body || "{}"); } catch {}
      const model = payload.model ?? "(none)";

      if (model === "glm-test-429") {
        res.writeHead(429, {
          "Content-Type": "application/json",
          "x-ratelimit-limit-requests": "20",
          "x-ratelimit-remaining-requests": "0",
          "x-ratelimit-reset-requests": "1m30s",
        });
        res.end(JSON.stringify({ error: { code: "1302", message: "速率限制，请稍后重试 (rate limited)" } }));
        return;
      }

      // ---- 首包前断开：200 头已发，一个内容帧都没有就 destroy ----
      if (isFlakyEmpty) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        res.socket.destroy();
        return;
      }

      // ---- 已吐字后断流：两帧后 destroy（网关应"不切换"只收尾） ----
      if (isFlaky) {
        res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
        const chunk = (delta) =>
          `data: ${JSON.stringify({
            id: "chatcmpl-flaky", object: "chat.completion.chunk", model,
            choices: [{ index: 0, delta, finish_reason: null }],
          })}\n\n`;
        res.write(chunk({ role: "assistant" }));
        res.write(chunk({ content: "第一帧" }));
        // FIN 优雅收尾：模拟上游流中断（非 RST；RST 会被 workerd fetch 判为网络错误）
        res.end();
        return;
      }

      if (payload.stream === true) {
        res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
        const chunk = (delta, finish) =>
          `data: ${JSON.stringify({
            id: "chatcmpl-mock-1", object: "chat.completion.chunk", model,
            choices: [{ index: 0, delta, finish_reason: finish ?? null }],
          })}\n\n`;
        const pieces = [
          chunk({ role: "assistant" }),
          chunk({ content: "你好" }),
          chunk({ content: `，来自智谱 ${model}` }),
          chunk({ content: "（WASM 流式透传验证）" }),
          chunk({}, "stop"),
        ];
        let i = 0;
        const timer = setInterval(() => {
          if (i < pieces.length) res.write(pieces[i++]);
          else {
            // 真实上游在 [DONE] 前会补一帧 usage（OpenAI 兼容惯例，stream_options.include_usage）
            res.write(`data: ${JSON.stringify({
              id: "chatcmpl-mock-1", object: "chat.completion.chunk", model,
              choices: [],
              usage: { prompt_tokens: 7, prompt_cache_hit_tokens: 2, completion_tokens: 9, total_tokens: 16 },
            })}\n\n`);
            res.write("data: [DONE]\n\n"); res.end(); clearInterval(timer);
          }
        }, 30);
        return;
      }

      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        id: "chatcmpl-mock-1", object: "chat.completion", created: Math.floor(Date.now() / 1000), model,
        choices: [{ index: 0, message: { role: "assistant", content: `收到："${payload.messages?.at(-1)?.content ?? ""}"（智谱 ${model}，非流式经由 WASM Worker）` }, finish_reason: "stop" }],
        usage: { prompt_tokens: 7, completion_tokens: 9, total_tokens: 16 },
      }));
      return;
    }

    // ---- multipart 音视频上传 ----
    if (req.url === "/audio/transcriptions" && req.method === "POST") {
      const ct = req.headers["content-type"] ?? "";
      const m = ct.match(/boundary=(.*)$/);
      if (!m) {
        res.writeHead(400, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { code: "no_boundary", message: "missing multipart boundary" } }));
        return;
      }
      const boundary = m[1];
      if (req.headers["authorization"] !== `Bearer ${VALID_KEY}`) {
        res.writeHead(401, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { code: "1001", message: "auth failed (multipart)" } }));
        return;
      }
      const parts = body.split(`--${boundary}`);
      let fileBytes = 0, fileField = "", modelField = "";
      for (const part of parts) {
        if (!/Content-Disposition/.test(part)) continue;
        const nameM = part.match(/name="([^"]+)"/);
        const fileM = part.match(/filename="([^"]+)"/);
        const idx = part.indexOf("\r\n\r\n");
        const val = idx >= 0 ? part.slice(idx + 4).replace(/\r\n$/, "") : "";
        if (nameM && fileM) { fileField = nameM[1]; fileBytes = Buffer.byteLength(val, "latin1"); }
        else if (nameM) { if (nameM[1] === "model") modelField = val; }
      }
      appendFileSync(LOG, JSON.stringify({
        ts: new Date().toISOString(), op: "multipart-received",
        boundary, fileField, fileBytes, modelField, filename: (parts.find(p=>/filename=/.test(p))?.match(/filename="([^"]+)"/)||[])[1],
        authorization: req.headers["authorization"] ?? "",
      }) + "\n");
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        text: `mock-transcription: received ${fileBytes} bytes (model=${modelField})`,
        bytes: fileBytes, model: modelField,
      }));
      return;
    }

    // ---- TTS 语音合成（播音 / 双向通话） ----
    if (req.url === "/audio/speech" && req.method === "POST") {
      if (req.headers["authorization"] !== `Bearer ${VALID_KEY}`) {
        res.writeHead(401, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { code: "1001", message: "auth failed (speech)" } }));
        return;
      }
      let payload = {};
      try { payload = JSON.parse(body || "{}"); } catch {}
      appendFileSync(LOG, JSON.stringify({
        ts: new Date().toISOString(), op: "tts-received",
        model: payload.model ?? null, voice: payload.voice ?? null,
        inputChars: (payload.input ?? "").length,
      }) + "\n");
      // 合法 WAV（RIFF 44 字节头 + 16bit 单声道 8kHz）：
      // 早期用「ID3+两帧」的畸形 MP3，浏览器 audio decode 直接拒收
      // （onerror → 播报失败），误导成前端 bug。智谱默认就回 wav，贴近真实。
      // 时长走 MOCK_TTS_SECONDS（默认 0.05s）：浏览器回归需要拉长播报窗口
      // 来复现「播报中连按第二次」。
      const sr = 8000, samples = Math.round(sr * (parseFloat(process.env.MOCK_TTS_SECONDS || "0.05") || 0.05));
      const dataBytes = samples * 2;
      const wav = Buffer.alloc(44 + dataBytes);
      wav.write("RIFF", 0); wav.writeUInt32LE(36 + dataBytes, 4); wav.write("WAVE", 8);
      wav.write("fmt ", 12); wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20);
      wav.writeUInt16LE(1, 22); wav.writeUInt32LE(sr, 24); wav.writeUInt32LE(sr * 2, 28);
      wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
      wav.write("data", 36); wav.writeUInt32LE(dataBytes, 40);
      res.writeHead(200, { "Content-Type": "audio/wav" });
      res.end(wav);
      return;
    }

    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: { code: "404", message: "not found (mock)" } }));
  });
});

server.listen(PORT, "127.0.0.1", () =>
  console.log(`mock-zhipu listening on http://127.0.0.1:${PORT}`)
);
