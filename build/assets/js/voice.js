/* ============================================================================
   QuakeVoice —— 语音与多媒体（B3 / F21 TTS 递降链 · F22 STT 递降链 · F23 模型范围）
   ----------------------------------------------------------------------------
   与旧 UI（admin_ui.html 语音能力层）同口径迁移，表现层换 Clarity Kit：
   - VoiceCfg：localStorage `gw_voice_cfg`（键与字段不变，旧数据无缝读取）
   - TTS 三级递降链：网关 /v1/audio/speech → 自定义端点（OpenAI 兼容）→
     浏览器 speechSynthesis（Speakit-JS 融合体：音色加载超时兜底 / 中文优选 /
     长文本按句切片）
   - STT 两级递降链：浏览器 SpeechRecognition（实时 partial，零上传）→
     网关 /v1/audio/transcriptions（MediaRecorder 录音 + WAV 重编码）
   - MicHub：麦克风单所有者 + 强制释放（iOS 双 MediaStream 互斥）
   - F23：音频（stt/tts）与图片视频（vision/video + mmScope）模型映射
   依赖：adapters（speech/transcribe/channels）/ components（toast）。
   app.js 只做视图装配；发送链路经 mmModelFor() 接入视觉选路。
   脚本加载序：… → providers → prompts → voice → workflow → app。
   ==========================================================================*/
(function (global) {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const { toast } = global.QuakeComponents;
  const gw = global.QuakeAdapters.gateway;
  const effModels = global.QuakeAdapters.effModels;
  const Store = global.QuakeStore;

  /* ================= 配置（gw_voice_cfg，字段与旧 UI 一致） ================= */
  const VoiceCfg = {
    KEY: "gw_voice_cfg",
    def: {
      ttsMode: "auto",        // auto 网关递降 / gateway 仅网关 / custom 自定义端点 / browser 仅浏览器
      sttMode: "auto",        // auto 浏览器优先 / gateway 仅网关 / browser 仅浏览器
      callMode: "duplex",
      ttsVoice: "", ttsRate: 1.05,
      sttModel: "", ttsModel: "", visionModel: "", videoModel: "",
      mmScope: "attach",      // attach 仅带附件的消息 / always 所有消息
      customTts: { url: "", key: "", model: "", voice: "" },
    },
    load() {
      try {
        const raw = JSON.parse(Store.config.get(this.KEY, "") || "{}");
        return { ...this.def, ...raw, customTts: { ...this.def.customTts, ...(raw.customTts || {}) } };
      } catch { return { ...this.def, customTts: { ...this.def.customTts } }; }
    },
    save(c) { try { Store.config.setJSON(this.KEY, c); } catch { /* 存储不可用：仅本次生效 */ } },
  };
  let voiceCfg = VoiceCfg.load();

  /* ================= 麦克风资源管理器（单所有者 + 强制释放） ================= */
  const MicHub = {
    stream: null, owner: null,
    async acquire(owner) {
      if (this.stream && this.owner === owner) return this.stream;
      this.release(); // 抢占：先彻底释放旧所有者的流，再要新的
      if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) throw new Error("当前浏览器不支持录音（需 HTTPS 或 localhost）");
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      this.owner = owner;
      return this.stream;
    },
    release(owner) {
      if (this.stream && (!owner || owner === this.owner)) {
        this.stream.getTracks().forEach(t => { try { t.stop(); } catch (e) {} });
        this.stream = null; this.owner = null;
      }
    },
    busy() { return !!this.stream; },
  };

  /* ================= 浏览器 TTS（Speakit-JS 融合体） ================= */
  const BrowserTts = {
    supported() { return "speechSynthesis" in window && "SpeechSynthesisUtterance" in window; },
    voices: [], voiceName: "", ready: false,
    /// 语音列表加载：getVoices 立即返回 → 直接用；为空则等 onvoiceschanged，
    /// 1.5s 还不来就轮询兜底（个别浏览器事件不触发）。
    async init() {
      if (!this.supported() || this.ready) return this.ready;
      const pick = () => {
        const vs = speechSynthesis.getVoices() || [];
        if (vs.length) { this.voices = vs; return true; }
        return false;
      };
      if (pick()) { this.ready = true; }
      else {
        this.ready = await new Promise(res => {
          let done = false;
          const finish = ok => { if (!done) { done = true; res(ok); } };
          speechSynthesis.onvoiceschanged = () => finish(pick());
          const t0 = Date.now();
          const poll = setInterval(() => {
            if (pick()) { clearInterval(poll); finish(true); }
            else if (Date.now() - t0 > 1500) { clearInterval(poll); finish(pick()); }
          }, 200);
        });
      }
      if (this.ready) this.autoPickVoice();
      return this.ready;
    },
    /// 音色优选：设置里指定的 → 中文（zh-CN > zh）→ 默认
    autoPickVoice() {
      if (voiceCfg.ttsVoice && this.voices.some(v => v.name === voiceCfg.ttsVoice)) { this.voiceName = voiceCfg.ttsVoice; return; }
      const zh = this.voices.find(v => v.lang === "zh-CN") || this.voices.find(v => (v.lang || "").startsWith("zh")) || this.voices.find(v => v.default);
      this.voiceName = zh ? zh.name : (this.voices[0] ? this.voices[0].name : "");
    },
    voiceOptions() { return this.voices.map(v => ({ name: v.name, lang: v.lang, local: v.localService })); },
    /// 播放一段文本：end resolve / error reject；被 cancel 时 reject Error("stopped")。
    /// 长文本按句切片顺序播——超长 utterance 在 Chrome 会被静默截断。
    speak(text) {
      return new Promise((resolve, reject) => {
        if (!this.supported()) return reject(new Error("浏览器不支持语音合成"));
        const parts = String(text || "").split(/(?<=[。！？；!?\n])/).map(s => s.trim()).filter(Boolean);
        const chunks = [];
        for (const p of parts) { for (let i = 0; i < p.length; i += 120) chunks.push(p.slice(i, i + 120)); }
        if (!chunks.length) return resolve();
        const zh = /[\u4e00-\u9fff]/.test(text);
        let idx = 0;
        const next = () => {
          if (idx >= chunks.length) return resolve();
          const u = new SpeechSynthesisUtterance(chunks[idx++]);
          u.lang = zh ? "zh-CN" : "en-US";
          u.rate = Number(voiceCfg.ttsRate) || 1.05;
          const v = this.voices.find(x => x.name === this.voiceName);
          if (v) u.voice = v;
          u.onend = next;
          u.onerror = e => {
            const err = (e && e.error) || "";
            if (err === "canceled" || err === "interrupted") reject(new Error("stopped")); // 主动停止
            else reject(new Error("浏览器播音失败"));
          };
          speechSynthesis.speak(u);
        };
        next();
      });
    },
    stop() { try { speechSynthesis.cancel(); } catch (e) {} },
  };

  /* ================= 浏览器 STT（SpeechRecognition 实时识别） ================= */
  const BrowserStt = {
    supported() { return !!(window.SpeechRecognition || window.webkitSpeechRecognition); },
    /// onPartial(interim) / onFinal(final) / onEnd(err|null)
    start(onPartial, onFinal, onEnd) {
      const R = window.SpeechRecognition || window.webkitSpeechRecognition;
      if (!R) return null;
      let rec;
      try { rec = new R(); } catch (e) { return null; }
      rec.continuous = true; rec.interimResults = true; rec.lang = "zh-CN";
      let finalText = "", stopped = false;
      rec.onresult = e => {
        let interim = "";
        for (let i = e.resultIndex; i < e.results.length; i++) {
          const r = e.results[i];
          if (r.isFinal) finalText += r[0].transcript;
          else interim += r[0].transcript;
        }
        if (interim && onPartial) onPartial(interim);
        if (finalText && onFinal) onFinal(finalText);
      };
      rec.onerror = e => { if (!stopped && onEnd) onEnd(e.error || "error"); };
      rec.onend = () => { if (!stopped && onEnd) onEnd(null); };
      try { rec.start(); } catch (e) { if (onEnd) onEnd(e.message || "start-failed"); return null; }
      return { stop() { stopped = true; try { rec.stop(); } catch (e) {} }, get final() { return finalText; } };
    },
  };

  /* ================= 统一语音输入（一次输入只用一个麦克风消费者） =================
     浏览器 SpeechRecognition 与 MediaRecorder 绝不并存——iOS/Android 上双消费者
     互相踩。识别可用（且模式允许）时零上传、真实时；否则 MediaRecorder + 网关转写。 */
  const VoiceInput = {
    cur: null,
    get mode() { return this.cur ? this.cur.mode : null; },
    async start(owner, onPartial) {
      this.abort(owner);
      if (voiceCfg.sttMode !== "gateway" && BrowserStt.supported()) {
        const h = BrowserStt.start(onPartial, null, null);
        if (!h) throw new Error("浏览器识别启动失败");
        this.cur = { owner, mode: "browser", handle: h };
        return "browser";
      }
      if (voiceCfg.sttMode === "browser") throw new Error("当前浏览器不支持语音识别（需 Chrome/Edge），请切换 STT 模式");
      const stream = await MicHub.acquire(owner);
      let rec;
      try { rec = new MediaRecorder(stream); }
      catch (e) { MicHub.release(owner); throw new Error("录音启动失败：" + e.message); }
      const chunks = [];
      rec.ondataavailable = ev => { if (ev.data && ev.data.size) chunks.push(ev.data); };
      rec.start(250);
      this.cur = { owner, mode: "recorder", rec, chunks, mime: rec.mimeType };
      return "recorder";
    },
    /// 停止并取最终文本：{ text, source: "browser"|"gateway"|"none" }
    async stop(owner) {
      const c = this.cur;
      if (!c || c.owner !== owner) return { text: "", source: "none" };
      this.cur = null;
      if (c.mode === "browser") {
        // 停识别后 Chrome 可能还把最后一句 interim 补成 final：轮询 600ms 兜底
        const text = await new Promise(res => {
          let done = false;
          const finish = v => { if (!done) { done = true; res(v); } };
          const t = setTimeout(() => finish(c.handle.final || ""), 600);
          c.handle.stop();
          const poll = setInterval(() => {
            if (done) return clearInterval(poll);
            if (c.handle.final) { clearTimeout(t); finish(c.handle.final); }
          }, 100);
        });
        return { text: (text || "").trim(), source: "browser" };
      }
      const blob = await new Promise(res => {
        c.rec.onstop = () => res(new Blob(c.chunks, { type: c.mime || "audio/webm" }));
        try { c.rec.stop(); } catch (e) { res(null); }
      });
      MicHub.release(owner);
      if (!blob || !blob.size) return { text: "", source: "gateway" };
      const ext = blob.type.includes("mp4") ? "m4a" : blob.type.includes("ogg") ? "ogg" : "webm";
      return { text: (await transcribeBlob(blob, "voice." + ext)).trim(), source: "gateway" };
    },
    /// 同步取消（打断 / 关闭浮层）：不等识别结果，立即释放麦克风
    abort(owner) {
      const c = this.cur;
      if (!c || (owner && c.owner !== owner)) return;
      this.cur = null;
      if (c.mode === "browser") c.handle.stop();
      else { try { c.rec.stop(); } catch (e) {} MicHub.release(c.owner); }
    },
  };

  /* ================= WAV 重编码（MediaRecorder → 智谱可识别 WAV） ================= */
  let _wavCtx = null;
  async function toWavBlob(blob) {
    const t = (blob.type || "").toLowerCase();
    if (t.includes("wav") || t.includes("mpeg") || t.includes("mp3")) return blob;
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC || !blob.size) return blob; // 无 WebAudio：原样上传，让上游给出明确错误
    try {
      if (!_wavCtx) _wavCtx = new AC();
      const decoded = await _wavCtx.decodeAudioData(await blob.arrayBuffer());
      const sr = 16000;
      const OC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
      const offline = new OC(1, Math.max(1, Math.ceil(decoded.duration * sr)), sr);
      const src = offline.createBufferSource();
      src.buffer = decoded;
      src.connect(offline.destination);
      src.start(0);
      const rendered = await offline.startRendering();
      return new Blob([encodeWav(rendered)], { type: "audio/wav" });
    } catch (e) {
      _wavCtx = null; // 解码失败：丢弃这个 context，下次重建，避免坏状态被复用
      return blob;
    }
  }
  function encodeWav(buf) {
    const sr = buf.sampleRate;
    const data = buf.getChannelData(0);
    const ab = new ArrayBuffer(44 + data.length * 2);
    const v = new DataView(ab);
    const str = (o, s) => { for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i)); };
    str(0, "RIFF"); v.setUint32(4, ab.byteLength - 8, true); str(8, "WAVE");
    str(12, "fmt "); v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, 1, true);
    v.setUint32(24, sr, true); v.setUint32(28, sr * 2, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
    str(36, "data"); v.setUint32(40, data.length * 2, true);
    let o = 44;
    for (let i = 0; i < data.length; i++) {
      const s = Math.max(-1, Math.min(1, data[i]));
      v.setInt16(o, s < 0 ? s * 0x8000 : s * 0x7fff, true);
      o += 2;
    }
    return ab;
  }

  /* ================= STT：音频 → 文本（经适配层调网关） ================= */
  async function transcribeBlob(blob, name) {
    const wav = await toWavBlob(blob);
    const nm = (wav.type || "").includes("wav") ? "audio.wav" : name;
    const fd = new FormData();
    fd.append("file", wav, nm);
    if (voiceCfg.sttModel) fd.append("model", voiceCfg.sttModel); // 留空 = 网关按 stt 能力自动选路
    const resp = await gw.transcribe(fd);
    const j = await resp.json().catch(() => ({}));
    if (!resp.ok) throw new Error((j.error && j.error.message) || ("HTTP " + resp.status));
    return (j.text || "").trim();
  }

  /* ================= TTS：文本 → 音频（三级递降链） ================= */
  let speakAudio = null;      // 网关/自定义端点播放器（统一 Audio 栈）
  let ttsCur = null, ttsCurResolve = null;
  function ttsStopAll() {
    BrowserTts.stop();
    if (speakAudio) { try { speakAudio.pause(); } catch (e) {} speakAudio = null; }
    if (ttsCur) { try { ttsCur.pause(); } catch (e) {} ttsCur = null; }
    if (ttsCurResolve) { const r = ttsCurResolve; ttsCurResolve = null; r(); }
  }
  /// 播放一段音频 blob（end resolve / 主动停止静默 resolve）
  function playBlob(blob) {
    return new Promise((resolve, reject) => {
      const url = URL.createObjectURL(blob);
      const a = new Audio(url);
      speakAudio = a;
      a.onended = () => { speakAudio = null; URL.revokeObjectURL(url); resolve(); };
      a.onerror = () => { speakAudio = null; URL.revokeObjectURL(url); reject(new Error("音频播放失败")); };
      a.play().catch(e => { speakAudio = null; URL.revokeObjectURL(url); reject(e); });
    });
  }
  /// 网关播音：/v1/audio/speech（经适配层；ttsModel 留空 = 网关按能力自动选路）
  async function ttsBlob(text) {
    const payload = { input: text.slice(0, 2000) };
    if (voiceCfg.ttsModel) payload.model = voiceCfg.ttsModel;
    const resp = await gw.speech(payload);
    if (!resp.ok) {
      const j = await resp.json().catch(() => ({}));
      throw new Error((j.error && j.error.message) || ("HTTP " + resp.status));
    }
    const blob = await resp.blob();
    if (!blob.size) throw new Error("音频为空");
    return blob;
  }
  /// 自定义 TTS 端点（OpenAI 兼容）：客户自带接口资源的替换点
  async function customTtsBlob(text) {
    const c = voiceCfg.customTts || {};
    if (!c.url) throw new Error("未配置自定义 TTS 端点");
    const h = { "Content-Type": "application/json" };
    if (c.key) h["Authorization"] = "Bearer " + c.key;
    const body = { input: text.slice(0, 2000), response_format: "wav" };
    if (c.model) body.model = c.model;
    if (c.voice) body.voice = c.voice;
    const resp = await fetch(c.url, { method: "POST", headers: h, body: JSON.stringify(body) });
    if (!resp.ok) {
      const j = await resp.json().catch(() => ({}));
      throw new Error("自定义 TTS：" + ((j.error && j.error.message) || ("HTTP " + resp.status)));
    }
    const blob = await resp.blob();
    if (!blob.size) throw new Error("自定义 TTS 音频为空");
    return blob;
  }
  let ttsDegradeNotified = false; // 降级提示只弹一次，避免每条回答都刷 toast
  /// 播放一句话（按当前模式走递降链）。「stopped」是主动停止的控制流信号——静默。
  async function ttsPlaySentence(text) {
    const mode = voiceCfg.ttsMode;
    if (mode === "browser") { await BrowserTts.init(); return BrowserTts.speak(text); }
    try {
      const blob = mode === "custom" ? await customTtsBlob(text) : await ttsBlob(text);
      await playBlob(blob);
      ttsDegradeNotified = false; // 网关恢复：重置提示，下次故障再告知
    } catch (e) {
      if (String(e && e.message) === "stopped") return; // 被主动停止（打断/关闭）：静默
      if (mode === "gateway") throw e; // 仅网关：不降级，错误上抛让 UI 明示
      await BrowserTts.init();
      if (!BrowserTts.ready) throw e;
      if (!ttsDegradeNotified) {
        ttsDegradeNotified = true;
        toast("网关播音不可用，已切浏览器语音（检查渠道 Key 或语音设置）", "");
      }
      return BrowserTts.speak(text);
    }
  }

  /* ================= 媒体栈手势预热（iOS/Android 自动播放策略对策） ================= */
  let mediaUnlocked = false;
  function unlockMedia() {
    if (mediaUnlocked) return;
    mediaUnlocked = true;
    try {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (AC) {
        const ctx = new AC();
        if (ctx.state === "suspended") ctx.resume().catch(() => {});
      }
    } catch (e) {}
    try {
      const a = new Audio("data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=");
      a.play().then(() => { a.pause(); a.src = ""; }).catch(() => {});
    } catch (e) {}
    try {
      if ("speechSynthesis" in window) {
        const u = new SpeechSynthesisUtterance(" ");
        u.volume = 0;
        speechSynthesis.speak(u); // iOS：首次 speak 唤醒语音引擎与 voices 列表
      }
    } catch (e) {}
  }
  window.addEventListener("pointerdown", unlockMedia, { once: true, capture: true });

  /* ================= 能力模型（F23：从渠道账本实时计算） ================= */
  const state = { channels: [], caps: { stt: [], tts: [], vision: [] }, loaded: false };
  function computeCaps() {
    const caps = { stt: [], tts: [], vision: [] };
    const seen = { stt: new Set(), tts: new Set(), vision: new Set() };
    for (const c of state.channels) {
      if (!c.enabled) continue;
      for (const m of effModels(c)) {
        const kind = m.kind || "text";
        if (!caps[kind] || seen[kind].has(m.name)) continue;
        seen[kind].add(m.name);
        caps[kind].push({ name: m.name, channel: c.name || c.id });
      }
    }
    state.caps = caps;
    return caps;
  }
  function modelsByCap(cap) { return state.caps[cap] || []; }
  /// F23 选路：本条消息是否带图片/视频附件 → 视觉/视频模型覆盖（与旧 UI pickMmModel 同口径）
  function mmModelFor(hasMedia, curModel) {
    const scope = voiceCfg.mmScope || "attach";
    // 本轮带附件：无论哪种作用范围都走多模态模型。
    // 本轮无附件：仅「always」作用范围才可能覆盖，且收敛为仅自动模式——用户显式
    // 选过模型时以选择为准（否则 always 会永久抢走纯文本的模型选择）。
    if (!hasMedia && (scope !== "always" || curModel)) return "";
    return voiceCfg.videoModel || voiceCfg.visionModel || "";
  }

  /* ================= 视图渲染 ================= */
  function fillCapSelect(selId, cap, current, autoLabel) {
    const sel = $(selId);
    if (!sel) return;
    const list = modelsByCap(cap);
    sel.innerHTML = '<option value="">' + (autoLabel || "自动") + "</option>" +
      list.map(m => '<option value="' + escapeAttr(m.name) + '">' + escapeAttr(m.name) + "（" + escapeAttr(m.channel) + "）</option>").join("");
    sel.value = current || "";
    if (sel.value !== (current || "")) sel.value = ""; // 选项不存在（渠道变更）→ 回落自动
  }
  function escapeAttr(s) { return String(s == null ? "" : s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }

  function chainStatusText() {
    const tts = [];
    if (voiceCfg.ttsMode === "auto") tts.push("网关 /v1/audio/speech", "自定义端点", "浏览器语音");
    else if (voiceCfg.ttsMode === "gateway") tts.push("网关 /v1/audio/speech（不降级）");
    else if (voiceCfg.ttsMode === "custom") tts.push("自定义端点" + (voiceCfg.customTts && voiceCfg.customTts.url ? "" : "（未配置 URL，将自动降级）"), "浏览器语音");
    else tts.push("浏览器语音（离线可用）");
    const stt = [];
    if (voiceCfg.sttMode !== "browser") stt.push("网关 /v1/audio/transcriptions");
    if (voiceCfg.sttMode !== "gateway") stt.push(BrowserStt.supported() ? "浏览器实时识别（零上传）" : "浏览器识别不可用");
    const caps = modelsByCap("vision");
    const el = $("#vcChainStatus");
    if (el) {
      el.innerHTML =
        "<b>TTS 链</b>：" + tts.map(escapeAttr).join(' <span class="chain-arrow">→</span> ') +
        "<br><b>STT 链</b>：" + stt.map(escapeAttr).join(' <span class="chain-arrow">→</span> ') +
        "<br><b>已登记视觉模型</b>：" + (caps.length ? caps.map(m => escapeAttr(m.name)).join("、") : "（暂无——去服务商页给模型加 vision 能力）");
    }
  }

  function fillVoiceUI() {
    const c = voiceCfg;
    fillCapSelect("#vc-stt-model", "stt", c.sttModel, "自动（按能力自动选路）");
    fillCapSelect("#vc-tts-model", "tts", c.ttsModel, "自动（按能力自动选路）");
    fillCapSelect("#vc-vision-model", "vision", c.visionModel, "自动（按能力自动选路）");
    fillCapSelect("#vc-video-model", "vision", c.videoModel, "跟随视觉模型");
    const scope = $("#vc-mm-scope");
    if (scope) scope.value = c.mmScope || "attach";
    $("#vc-tts-mode").value = c.ttsMode;
    $("#vc-stt-mode").value = c.sttMode;
    $("#vc-custom-url").value = (c.customTts && c.customTts.url) || "";
    $("#vc-custom-key").value = (c.customTts && c.customTts.key) || "";
    $("#vc-custom-model").value = (c.customTts && c.customTts.model) || "";
    $("#vc-custom-voice").value = (c.customTts && c.customTts.voice) || "";
    $("#vc-rate").value = c.ttsRate;
    $("#vc-rate-v").textContent = Number(c.ttsRate).toFixed(2);
    refreshBrowserVoices();
    chainStatusText();
  }

  /// 浏览器音色列表：异步加载（个别浏览器首次 getVoices 为空，init 内含轮询兜底）
  async function refreshBrowserVoices() {
    const sel = $("#vc-browser-voice");
    if (!sel) return;
    if (BrowserTts.supported()) {
      await BrowserTts.init();
      if (BrowserTts.ready) {
        const opts = BrowserTts.voiceOptions();
        sel.innerHTML = '<option value="">自动（优选中文）</option>' +
          opts.map(v => '<option value="' + escapeAttr(v.name) + '">' + escapeAttr(v.name) + "（" + escapeAttr(v.lang || "?") + (v.local ? " · 本地" : "") + "）</option>").join("");
        if (voiceCfg.ttsVoice) sel.value = voiceCfg.ttsVoice;
        return;
      }
    }
    sel.innerHTML = '<option value="">浏览器语音不可用</option>';
  }

  async function load() {
    try {
      state.channels = await gw.channels();
      computeCaps();
      state.loaded = true;
    } catch (e) {
      toast("渠道账本读取失败，能力下拉可能为空：" + (e.message || e), "err");
    }
    fillVoiceUI();
  }

  function save() {
    voiceCfg = {
      ...voiceCfg,
      ttsMode: $("#vc-tts-mode").value,
      sttMode: $("#vc-stt-mode").value,
      ttsVoice: $("#vc-browser-voice").value,
      ttsRate: Number($("#vc-rate").value) || 1.05,
      sttModel: $("#vc-stt-model").value,
      ttsModel: $("#vc-tts-model").value,
      visionModel: $("#vc-vision-model").value,
      videoModel: $("#vc-video-model").value,
      mmScope: $("#vc-mm-scope").value,
      customTts: {
        url: $("#vc-custom-url").value.trim(),
        key: $("#vc-custom-key").value.trim(),
        model: $("#vc-custom-model").value.trim(),
        voice: $("#vc-custom-voice").value.trim(),
      },
    };
    VoiceCfg.save(voiceCfg);
    BrowserTts.ready = false; // 音色/模式变化后下次重新加载与优选
    chainStatusText();
    const hint = $("#vcHint");
    if (hint) hint.textContent = "已保存 " + new Date().toLocaleTimeString("zh-CN");
    toast("语音与多媒体设置已保存", "ok");
  }

  /* ================= 测试：播报 / 转写 ================= */
  let ttsTesting = false;
  async function testTts() {
    if (ttsTesting) { ttsStopAll(); ttsTesting = false; return; }
    ttsTesting = true;
    const btn = $("#vcTestTts");
    if (btn) btn.textContent = "■ 停止";
    const sample = "这是 QuakeAI 语音播报测试。如果您能听到这句话，说明当前链路工作正常。";
    const layers = [];
    const t0 = Date.now();
    try {
      // 按当前模式逐层尝试，输出每一层的结果（与递降链同序）
      const mode = voiceCfg.ttsMode;
      const tryGateway = mode === "auto" || mode === "gateway";
      const tryCustom = mode === "auto" || mode === "custom";
      if (tryGateway) {
        try { await playBlob(await ttsBlob(sample)); layers.push("网关：成功（" + (Date.now() - t0) + "ms）"); }
        catch (e) { layers.push("网关：失败（" + (e.message || e) + "）"); }
      }
      if (layers.length && layers[layers.length - 1].startsWith("网关：成功")) { /* 已播 */ }
      else if (tryCustom) {
        try { await playBlob(await customTtsBlob(sample)); layers.push("自定义端点：成功（" + (Date.now() - t0) + "ms）"); }
        catch (e) { layers.push("自定义端点：失败（" + (e.message || e) + "）"); }
      }
      if ((!layers.length || layers.every(l => l.includes("失败"))) && mode !== "gateway") {
        try {
          await BrowserTts.init();
          if (BrowserTts.ready) { await BrowserTts.speak(sample); layers.push("浏览器语音：成功"); }
          else layers.push("浏览器语音：不可用");
        } catch (e) { layers.push("浏览器语音：失败（" + (e.message || e) + "）"); }
      }
    } finally {
      ttsTesting = false;
      if (btn) btn.textContent = "测试播报";
      const hint = $("#vcHint");
      if (hint) hint.textContent = "播报测试：" + layers.join(" → ");
    }
  }

  let sttTesting = false;
  async function testStt() {
    if (sttTesting) { VoiceInput.abort("vc-test"); sttTesting = false; return; }
    sttTesting = true;
    const btn = $("#vcTestStt");
    if (btn) btn.textContent = "■ 停止";
    const hint = $("#vcHint");
    try {
      const mode = await VoiceInput.start("vc-test", p => { if (hint) hint.textContent = "识别中… " + p; });
      if (hint) hint.textContent = mode === "browser" ? "浏览器识别中（说完自动停止）…" : "录音中（约 3 秒后自动停止并转写）…";
      if (mode === "recorder") await new Promise(r => setTimeout(r, 3000));
      const r = await VoiceInput.stop("vc-test");
      if (hint) hint.textContent = "转写结果（" + (r.source === "browser" ? "浏览器识别" : "网关转写") + "）：" + (r.text || "（空）");
      if (!r.text) toast("没有识别到内容，再试一次", "");
    } catch (e) {
      if (hint) hint.textContent = "转写测试失败：" + (e.message || e);
      toast("转写测试失败：" + (e.message || e), "err");
    } finally {
      sttTesting = false;
      if (btn) btn.textContent = "测试转写";
    }
  }

  /* ================= 装配 ================= */
  function init() {
    $("#vcSave").onclick = save;
    $("#vcRefresh").onclick = () => load();
    $("#vcTestTts").onclick = testTts;
    $("#vcTestStt").onclick = testStt;
    $("#vc-rate").addEventListener("input", e => { $("#vc-rate-v").textContent = Number(e.target.value).toFixed(2); });
    // 能力模型依赖渠道账本：装载一次（后续每次进入视图都刷新，拿最新账本）
    load();
  }

  global.QuakeVoice = {
    state, init, load, save,
    get cfg() { return voiceCfg; },
    BrowserTts, BrowserStt, MicHub, VoiceInput,
    ttsPlaySentence, ttsStopAll, transcribeBlob,
    modelsByCap, mmModelFor, chainStatusText,
  };
})(window);
