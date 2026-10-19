# zhipu-wasm-gateway（v0.7.4）

**Rust → WASM 的免费资源 AI 聚合网关：资源管理 + 智能路由单例。**

网关主体是 Rust 代码，编译为 `wasm32-unknown-unknown`，跑在 **Cloudflare Workers** 运行时上
（本地用官方 `workerd` 二进制运行，与线上同一运行时）。对外暴露 OpenAI 兼容端点
（`/v1/chat/completions`、`/v1/audio/transcriptions`、`/v1/audio/speech`），对内把请求路由到登记过的国内优先
资源（智谱、硅基流动……）。

版本演进：

| 版本 | 能力 | 验证 |
| --- | --- | --- |
| v0.1 | WASM 网关打通智谱 OpenAI 兼容 API | ✅ mock + 真端点（401 透传） |
| v0.2 | **流式首包前重试**（首包前失败可重试、已吐字不切换）+ **multipart 音视频上传** | ✅ e2e 实测 |
| v0.3 | **资源管理前端页面**（登记/更新/轮换 Key/探测/事件账本）+ **Durable Object 账本** + **429 冷却跳过** + 接入**硅基流动**（下一个国内资源） | ✅ e2e **64/64** |
| v0.4 | **登记 500 修复**（DO 不可用 → 503 + 绑定指引；未预期错误 → JSON 500 兜底）+ **类 ChatGPT 前端**（对话首页 + 模型管理二级子页，hash 路由，流式聊天/停止/渠道徽章）+ 移动端适配 + 掩码 Key 前后双预检 | ✅ e2e **64/64** + 浏览器交互回归 **17/17** |
| v0.5.1 | **诊断头中文乱码修复**：中文渠道名经 `header_safe()` percent-encoding 后上线（HTTP 头不允许非 ASCII，浏览器按 Latin-1 解会导致 `智谱` → `Ã¦ÂÂç`），前端 `decodeHeader()` 还原 | ✅ e2e **70/70** + 单测 **7/7** |
| v0.6.0 | **输入区增强**：🎙 长按语音（录音→`/v1/audio/transcriptions` 转写→自动发送，上滑取消）+ `+` 号提交本地资源（图片走 vision parts / 音频转写 / 其他文件降级文字说明；dataUrl 不落 localStorage）；顺手修复刷新后当前会话丢失 | ✅ e2e **70/70** + 单测 **7/7** + 浏览器交互回归 **24/24** |
| v0.7.0 | **模型选择引擎**：四级能力（text/vision/stt/tts）+ 模型级路由/健康/冷却/自动换模型 + TTS 播音端点（`/v1/audio/speech`）+ 跨渠道 Key 复用（音频渠道借同 provider 主线 Key）；**设备级隔离**：`gw_device_keys` 头 + 本机 Key 卡片 + DO localDisk 持久化（重启不丢 Key）；**前端**：回答复制/分享/播音、最近会话缓存、右上角双向通话、消息长按重发、明暗主题、模型管理模型行/能力徽章/筛选 | ✅ e2e **89/89** + 单测 **7/7** + WASM 编译 0 警告 |
| v0.7.1 | **三连修**：iOS 键盘弹起 App 消失（visualViewport offsetTop）、STT「不支持当前文件格式」（iOS mp4 → WebAudio 重采样 16kHz WAV）、播音无模型（tts 候选池 chat+audio 双池合并） | ✅ e2e **89/89** + 单测 **7/7** |
| v0.7.2 | **智谱语音端点模型修正**：STT=glm-asr-2512（+stream=false）、TTS=glm-tts（音色 tongtong）——glm-4-voice 是端到端语音模型，放到转写/合成端点会被上游按 chat 校验（1210/1214）；旧模型名请求时自动改写，老账本零迁移 | ✅ e2e **90/90** + 单测 **7/7** |
| v0.7.4（本版） | **修复侧栏版本号显示过期值**（「前端版本变成 6.0」）：HTML 初始值去硬编码 + `refreshVersion()` /health 兜底；顺带挖出 `admin.rs` 独立 VERSION 常量停更的深因——全部收敛到 `env!("CARGO_PKG_VERSION")` 单一事实源（以后升级只改 Cargo.toml） | ✅ e2e **95/95** + 单测 **11/11** + Playwright 三场景实测 |
| v0.7.3 | **实时语音通话**：① 根因根治「messages 参数非法」（1214）——智谱系渠道一律收敛到端点唯一模型 + 账本零迁移（audio 渠道旧名 glm-4-voice → glm-asr-2512/glm-tts）；② **STT 实时文字流**（录音中每 2s 阶段性全量转写 + diff 显示，通话页与长按发语音均支持）；③ **分句实时 TTS 播报**（chat SSE 按句切分，凑满一句立即合成入队顺序播放，文字同步滚动）；④ **呼叫页拟人化 UI**（AI 精灵球四态：呼吸/波纹/脉冲/声浪）+ 通话模型独立切换 | ✅ e2e **92/92** + 单测 **10/10** + 前端 JS 语法/切句逻辑单测 + Playwright 全链路实测（假麦克风录音→实时转写→流式对话→WAV 播报） |
| v0.5 | **智谱 JWT 自动鉴权**：provider=zhipu 且 Key 为 `{id}.{secret}` → 现场生成 HS256 短期 JWT（sign_type=SIGN，TTL 1h，官方推荐方式）；真实联调证实：完整 Key 直调 200 / JWT 直调 200 / 经网关 JWT 200（此前 401 纯因掩码 Key 无效） | ✅ e2e **69/69** + 密码学标准向量单测 **5/5** + 真实端点联调 **13/13**（智谱 + 硅基双通道，经网关含流式/探测/掩码） |

> 各版本的详细根因与改法见 **[CHANGES.md](CHANGES.md)**。

## 快速开始

```bash
cargo install worker-build --version "^0.8"
rustup target add wasm32-unknown-unknown
# workerd 二进制：npm @cloudflare/workerd-linux-64（或 GitHub releases，linux-64）
# 放到 .tools/workerd，或设 WORKERD_BIN；test-e2e.sh 自动探测，也可改脚本

bash scripts/build.sh     # cargo + worker-build + 追加 ResourceStore DO 类
bash scripts/test-e2e.sh  # 自动起 mock + workerd，跑 64 项断言
```

手动跑：

```bash
node scripts/mock-zhipu.mjs &                    # 本地 mock 上游（127.0.0.1:8788）
workerd serve workerd.config.capnp               # 网关（127.0.0.1:8787）
open http://127.0.0.1:8787/admin                 # 资源管理页面
```

## 资源管理（v0.3 核心）

打开 `/`（或 `/admin`）即进入管理界面——**类 ChatGPT 布局的单页应用**，无外部依赖：

- **对话首页（`#/`）**：消息列表 + 底部输入框；发送后 SSE 流式逐帧上屏，
  发送键变为**停止键**（AbortController 中断）；每次回复带渠道徽章
  （`via 智谱 BigModel`，自动切换时显示「⤻ 已自动切换」+ attempts 明细）；
  支持 reasoning 折叠块与 markdown-lite 渲染。
- **输入区增强（v0.6.0）**：`+` 号提交本地资源——图片以 vision parts 发送
  （需 GLM-4V / Qwen-VL 等视觉模型，气泡渲染缩略图），音频经
  `/v1/audio/transcriptions` 转写后入正文，视频/PDF 等其他文件降级为
  `[附件: 名称]` 文字说明（dataUrl 只在内存，不落 localStorage 防爆配额）；
  🎙 长按说话（松手发送、上滑 60px 取消），与选音频文件同一条转写链路。
  刷新后自动恢复上次正在看的会话。
- **模型管理（`#/models`，首页二级子页）**：左上角 ← 返回；三个入口——
  侧栏底部「⚙ 模型管理」全宽按钮、顶栏入口按钮（移动端仅图标）、
  无渠道时欢迎区大按钮。hash 路由，`/admin` 无 hash 时兼容落 `#/models`
  （旧链接不破）。
- **移动端**：`100dvh` + 安全区适配；侧栏为抽屉（遮罩关闭、锁滚）；
  对话框为底部弹层（drag handle + 上滑动画）；渠道网格单列。

模型管理页内容：

- **国内预设目录**（左侧栏）：智谱 BigModel、硅基流动 SiliconFlow、Moonshot Kimi、阿里百炼、
  DeepSeek、MiniMax、腾讯混元、百度千帆、火山方舟豆包、讯飞星火、智谱音频（chat/audio），
  以及海外补充（OpenRouter / Groq / NVIDIA NIM）。每个卡片带免费额度说明和文档链接，
  点「登记」自动预填 Base URL / 模型 / 档位。
- **一键载入国内默认**：写入智谱主线、智谱音频、硅基流动三条渠道（disabled + Key 待登记，
  避免空 Key 打上游）。
- **API Key 登记 / 轮换**：行内「登记 Key / 换 Key」→ 粘贴 → 保存。**Key 只以掩码
  （前 3 后 4）回显，明文只写入 Durable Object，任何 API 响应都不含明文**；轮换时回传
  掩码值或留空 = 不改变原 Key（防误覆盖）。掩码值（含 `***` / `...` / `…`）在前端
  预检阶段直接拦截，后端同样 400 `masked_key` 拒绝（v0.4 双保险）。
- **智谱 Key 特殊格式**（v0.5）：智谱 Key 是 `{id}.{secret}` 两段式，网关会自动换成
  短期 JWT 调 `/api/paas/v4`（官方推荐的 JWT 鉴权方式；掩码值如 `5bc8...4bRY` 直调会
  401「令牌已过期或验证不正确」，前后端均有掩码预检拦截）——登记时整串粘贴即可，无需手动生成 JWT。
- **连通性测试**：chat 渠道发 `{model, max_tokens:1}` 最小请求；audio 渠道探 `GET /models`。
  判定 2xx=healthy / 401/403=链路通但 Key 无效 / 429=限流并写入 60s 冷却 / 其余=失败，
  结果回写渠道健康状态（延迟、最近检测时间、错误摘要）。
- **优先级与开关**：priority 大者优先，行内 ▲▼ 调整；可随时停用/启用（启用即清冷却，
  给一次立即恢复机会）。
- **降权/切换事件面板**：最近 50 条环形账本（ring buffer），形如
  `ch-a=HTTP429;ch-b=ok`，结果分 ok / degraded / failed。
- **store 不可用横幅**：DO 未绑定/不可用时，页面顶部红色横幅给出绑定指引
  （v0.4：登记接口此时返回 503 `store_unavailable` 而非裸 500）。
- **管理面鉴权（可选）**：配置 `ADMIN_TOKEN`（var/secret）后，所有 `/admin/api/*` 要求
  `x-admin-token` 头；页面右上角填入即存 localStorage。未配置时仅面向本地/内网。

存储：`RESOURCE_STORE` Durable Object 单例（`scripts/storage-do.js` 实现的 JS 类，
workerd/Workers 静态注册）。渠道、Key、健康状态、事件账本都在 DO 的 `state.storage` 里；
Rust 主线通过 `stub.fetch_with_request` 走 JSON 协议读写（`src/store.rs`）。DO 天然串行化，
无锁安全；本地 workerd 是 inMemory 存储 + 5s alarm 保活（详见下「诚实清单」）。

## 智能路由（"先选能力强的，被降权后再换一个"）

1. **候选排序**：账本中 enabled + 已登记 Key + 不在冷却的渠道，按 `priority` 降序
   （登记时按 T0/T1/T2 档位和免费额度人工设定；PRD 的六因子自动评分是后续）。
2. **逐渠道尝试**（v0.2 语义不变）：
   - 非 2xx（429/5xx/4xx）→ 记事件 + 回写降权状态 → **立即换下一渠道**；
   - 429 额外写入 60s 冷却窗口，期间该渠道被候选过滤跳过；
   - 连接失败 / 首帧前断连 / 空流 → 同样换渠道；
   - **首帧成功到达客户端后锁定，任何中断只干净收尾**（已吐字不切换，防内容撕裂）。
3. **全部失败**：最后一个上游错误（状态码+错误体）原样透传——降权信号不灭；
   只有全部连接级失败才返回 502 汇总（含 attempts 明细）。
4. **回退**：账本不可用/无可用动态渠道时，回退 `ZHIPU_CHANNELS` 静态通道 +
   `ZHIPU_API_KEY`（老配置零改动）。

诊断响应头：`x-gateway-channel` / `x-gateway-provider` / `x-gateway-attempts` /
`x-gateway-model` / `x-gateway-version`（兼容保留旧 `x-zhipu-*` 系列）。

## 下一个资源：硅基流动 SiliconFlow

按"国内优先"顺序，智谱之后接 **硅基流动**（注册实名送 16 元，≤10B 模型 0 元永久免费，
OpenAI 兼容 `https://api.siliconflow.cn/v1`）：

- 已在预设目录 + seed 默认内置（`channels.rs` 的 `default_cn_channels`）；
- e2e 第 13 节实测：登记 `provider=siliconflow` 渠道 → chat 请求命中并透传响应，
  诊断头 `x-gateway-provider: siliconflow`；
- 其他国内源同理：管理页「+ 登记新资源」或从预设目录点「登记」，粘 Key、启用即可，
  **无需改代码**（OpenAI 兼容管线复用；audio 类走 `/audio/transcriptions` 同路径）。

## 验证结论（mock e2e 64/64 + 浏览器交互回归 17/17 实测通过）

第 1–11 项（前 32 断言，v0.1/v0.2 既有能力）不变：

| # | 验证点 | 结果 |
| --- | --- | --- |
| 1 | Rust → `wasm32-unknown-unknown`（worker crate 0.8.7 + worker-build） | ✅ |
| 2 | workerd 加载 WASM、处理 fetch 事件 | ✅ |
| 3 | 非流式 JSON 透传（统一走"首帧+拼流"路径） | ✅ |
| 4 | 流式 SSE 透传（逐帧 + `[DONE]`） | ✅ |
| 5 | 上游 429 状态码与错误体原样回传（降权信号） | ✅ |
| 6 | 多通道：首包前 502 → 同请求自动换通道 | ✅ attempts 记录 |
| 7 | 首帧前断连（200+0帧）→ 换通道拿到完整流 | ✅ |
| 8 | 已吐字后断流：只收尾，上游仅请求 1 次 | ✅ |
| 9 | multipart 音频（512B）/视频（64B）上传、字段透传 | ✅ |
| 10 | 真实端点：WASM → open.bigmodel.cn TLS + 401 透传 | ✅ |

v0.3 新增（第 12–13 节）：

| # | 验证点 | 结果 |
| --- | --- | --- |
| 11 | 管理页 UI 渲染（标题/预设目录/渠道表/seed 按钮） | ✅ |
| 12 | seed 载入 3 个国内默认渠道 | ✅ |
| 13 | 登记渠道 + 列表 + DO 持久化（进程内） | ✅ |
| 14 | **API Key 掩码**：响应无明文、格式前3后4 | ✅ |
| 15 | 动态双通道：dyn-a(502)→dyn-b(ok)，attempts/provider 头正确 | ✅ |
| 16 | 事件账本记录降权（failed + attempts 串） | ✅ |
| 17 | Key 轮换：明文替换、掩码占位不改变、响应无明文 | ✅ |
| 18 | 删除渠道生效 | ✅ |
| 19 | **429 透传 + 冷却窗口写入 + 冷却期跳过（回退静态仍成功）** | ✅ |
| 20 | **硅基流动渠道命中并透传**（provider 诊断头） | ✅ |
| 21 | 动态渠道清空后回退静态 default 通道 | ✅ |

v0.4 新增验证（浏览器交互回归，puppeteer + 手机/桌面双视口）：

| # | 验证点 | 结果 |
| --- | --- | --- |
| 22 | 手机端顶栏无横向溢出（实测 entry-btn right=378 < 390） | ✅ |
| 23 | 抽屉侧栏开合 + 遮罩关闭 + 锁滚 | ✅ |
| 24 | 模型选择器（5 项：打开/过滤/选择/取消/键盘） | ✅ |
| 25 | SSE 流式上屏 + 渠道徽章 +「⤻ 已自动切换」+ reasoning 折叠 | ✅ |
| 26 | 停止按钮中断流（AbortController） | ✅ |
| 27 | 二级子页进入（#/models）与 ← 返回 | ✅ |
| 28 | 登记弹层 + 掩码 Key 前端拦截 | ✅ |
| 29 | 桌面视口无横向滚动 | ✅ |

v0.6.0 新增验证（浏览器交互回归，puppeteer + Chromium 假音频设备，420×860 手机视口）：

| # | 验证点 | 结果 |
| --- | --- | --- |
| 30 | `+` / 🎙 按钮、隐藏 file input、录音浮层初始隐藏 | ✅ |
| 31 | 附件 chip 渲染（data URL 缩略图）+ 移除清空 | ✅ |
| 32 | 图片 + 文字一起发送：气泡含缩略图与正文，发送后输入框/附件清空 | ✅ |
| 33 | 长按录音：浮层显示 + 计时器走动 → 松手转写 → 文本入消息 → 自动发送 → 助手回复 | ✅ |
| 34 | 上滑 >60px 取消态（浮层变红/文案「取消」/松手收起/不产生消息） | ✅ |
| 35 | localStorage 无 base64（dataUrl 不落盘）+ 刷新后附件降级为 `📎 文件名` | ✅ |
| 36 | 刷新后恢复上次会话（消息不空白）+ 徽章无 Mojibake | ✅ |

唯一未测项：**有效 Key 的真实对话/转写响应**（需要你自己的 Key，管理页登记即用）。

## 目录结构

```
├── src/
│   ├── lib.rs            # 入口路由 + 多通道重试 + 首帧拼流 + multipart 重建
│   ├── channels.rs       # Channel 模型 / 国内预设目录 / 掩码 / 冷却判定 / seed
│   ├── store.rs          # DO 账本 RPC（load/save/events）
│   ├── admin.rs          # /admin API：CRUD、探测、seed、events、健康回写（503/500 硬化）
│   └── admin_ui.html     # 类 ChatGPT 单页：对话首页 + 模型管理二级子页（include_str! 内嵌）
├── scripts/
│   ├── build.sh          # cargo + worker-build + 追加 ResourceStore DO 类
│   ├── make-single-file.sh  # 合成 deploy/ 单文件（wasm base64 内联，Dashboard 粘贴用）
│   ├── storage-do.js     # JS Durable Object 类（追加进 build/index.js）
│   ├── mock-zhipu.mjs    # 本地 mock：流式/非流式/429/502/断流/multipart 全行为
│   └── test-e2e.sh       # 一键端到端（64 项断言，多 workerd 配置切换）
├── build/                # worker-build 产物（index.js 含 DO 类注册 + index_bg.wasm）
├── deploy/
│   └── single-file-worker.js   # 单文件部署版（路线 B，598KB，已本地实测）
├── .github/workflows/deploy.yml # 路线 C：push 即云端部署
├── workerd.config.capnp      # 本地配置（BASE_URL → mock，DO binding）
├── workerd.real-test.capnp   # 真端点配置（填你的 Key 即可真实对话）
├── wrangler.toml             # 线上部署（含 DO migration）
├── CHANGES.md                # 版本变更记录（v0.4 三项修复根因与改法）
├── LINUX.md                  # Linux 自托管方案（workerd + systemd）
├── README.md / DEPLOY.md     # 使用说明 / 部署完整指南（三条路线 + 无 Rust 说明）
└── .tools/workerd            # 本地运行时二进制（npm @cloudflare/workerd-linux-64，.gitignore 忽略）
```

## 两个"限制"是如何被解决的

### 1. 流式重试（oxide 说"未实现"的）

根因不是平台做不到，而是高层封装没给选项。Workers/WASM 三块原语都齐：
`fetch().body` 是流、`ReadableStreamDefaultReader.read()` 可 await、可新建流回吐。

```
for 渠道 in 候选顺序(priority 降序，跳冷却):
    fetch 该渠道
    非 2xx(429/5xx/4xx) ──► 记事件+回写健康，continue      ← 首包前失败，换
    连接失败(Promise reject) ──► continue                  ← 首包前失败，换
    读首帧 read_chunk():
        失败/空流 ──► continue                              ← 首包前失败，换
        成功 ──► 构造新 ReadableStream：先 enqueue 首帧，再续读剩余帧
                  → 此渠道锁定；后续任何中断只 close() 收尾  ← 已吐字不切换
```

### 2. multipart 音视频上传（oxide 说"不可用"的）

- inbound：`worker::Request::form_data()` 原生解析 multipart；
- 裸字节：`File::bytes()` = `Blob.arrayBuffer()` → `Vec<u8>`；
- outbound：**手工拼 multipart 字节体**（boundary + CRLF + 裸 bytes），不依赖
  FormData 序列化。音频/视频/任意二进制通用。

## 生产部署（没有本地 Rust 也能上线）

> 完整指南见 **[DEPLOY.md](DEPLOY.md)**。一句话版：**部署从来不需要 Rust**
> （Rust 只在改 `src/*.rs` 后重新编译才需要），`build/` 已是编译好的产物；
> 「Dashboard 只能单文件」也只对网页编辑器成立。三条路线按推荐度排序：

| 路线 | 需要什么 | 操作 |
| --- | --- | --- |
| **A. wrangler CLI（推荐）** | Node（不用 Rust） | `npx wrangler login` → `secret put ZHIPU_API_KEY` → `npx wrangler deploy`。多模块上传，无单文件限制；`wrangler.toml` 已含 DO migration |
| **B. Dashboard 网页粘贴** | 什么都不用装 | 全选粘贴 `deploy/single-file-worker.js`（598KB，wasm 已 base64 内联，本地 workerd 单模块实测通过）→ Deploy → 加 `RESOURCE_STORE` DO 绑定 + 变量 |
| **C. GitHub Actions** | 什么都不用装 | 推仓库到 GitHub 即自动部署（`.github/workflows/deploy.yml` 已就绪，配两个 repo secret） |

```bash
# 路线 A 最小命令集
npx --yes wrangler@latest login
npx --yes wrangler@latest secret put ZHIPU_API_KEY     # 静态回退 Key（可选）
npx --yes wrangler@latest secret put ADMIN_TOKEN       # 管理面鉴权（强烈建议）
npx --yes wrangler@latest deploy                       # wrangler.toml 已含 DO migration
```

之后所有资源 Key 都在 `/admin` 页面登记，无需再碰 secret；静态 fallback 保留。

重新编译产物（仅改了 Rust 源码时需要）：`bash scripts/build.sh` 后
`bash scripts/make-single-file.sh`（刷新路线 B 的单文件）。产物中的
`build/index.js` + `build/index_bg.wasm` 是部署必需的两个模块，`.gitignore`
已放行提交（云端 CI 部署免 Rust 工具链的关键）。

## 标准交付模式：GitHub commit → Cloudflare Worker 自动发布

交付不止于文档——仓库已把「commit 即上线」链路配好（`.github/workflows/deploy.yml`）：

```
git push origin main
   └─▶ GitHub Actions（ubuntu-latest）
         ├─ checkout（含预编译产物 build/index.js + index_bg.wasm）
         ├─ preflight：node scripts/test-storage-do.mjs（DO 协议自测）
         └─ npx wrangler deploy  ──▶ Cloudflare Worker「quakeai-gateway」
                                      （DO migration v1 自动应用）
```

一次性配置（仓库建好后）：

1. **GitHub 仓库**：`Settings → Secrets and variables → Actions` 加两个 secret：
   `CLOUDFLARE_API_TOKEN`（权限模板 **Edit Cloudflare Workers**）、
   `CLOUDFLARE_ACCOUNT_ID`（Cloudflare 面板右侧栏）。
2. **密钥**（二选一）：
   - 推前执行 `npx wrangler secret put ZHIPU_API_KEY` / `ADMIN_TOKEN`；
   - 或部署后全部在 `/admin` 页面登记各资源 Key（静态 fallback 可留空）。
3. 之后每次 `git push`（或 Actions 页手动 Run workflow）即自动部署，
   本地不需要 Node 也不需要 Rust。

> worker 名取 `wrangler.toml` 的 `name = "quakeai-gateway"`；要部署到已有
> worker 就把它改成那个名字。改动只碰 `src/` 时才需要重新
> `bash scripts/build.sh` 编译（CI 直接部署仓库里的预编译产物）。
>
> Linux 自托管（workerd + systemd）方案见 **[LINUX.md](LINUX.md)**。

## 仍存在的真实限制（诚实清单）

- **本地 DO 存储不跨重启**：`durableObjectStorage = (inMemory = void)` 仅进程内存，
  且 workerd 实测在实例 idle 逐出后会丢 storage（与 schema 文档表述不符）——本仓库用
  5s alarm 保活规避；**生产 Cloudflare 上 storage 本就跨实例持久**，重启不丢，
  alarm 保活只是无害空唤醒。要本地跨重启持久，可改 `localDisk`（workerd 标为实验性）。
- **无 timer 原语**：冷却窗口用 `Date.now()` 比较而非 sleep；PRD 的 DO  alarms
  调度设计不变（本仓库的 alarm 保活即最小用例）。
- **请求体大小**：Workers 免费层 body 上限 100MB；更大文件需流式 pipe（P2）。
- **掩码≠加密**：DO 内 Key 为明文存储（本地 workerd / 生产 DO storage 同）；
  更高安全要求请将 Key 放 `wrangler secret` 并由代码按渠道名映射读取，或接 KMS。
