# CHANGES

## v0.13.0（2026-10-20）—— 海外视觉全量对接（免费视觉模型补列 / video_url 甄别 / 统一选择逻辑）

用户指令：「把海外视觉模型全部对接补上」+「视觉模型管理要对接聊天的视觉甄别自动切换视觉模型 与文本模型的选择一个逻辑的」。

- **OpenRouter 免费视觉模型全量补列**：此前海外融合只列了 `gemma-4-31b-it:free` 一个视觉 `:free`，漏了 OpenRouter 免费层其余视觉/多模态模型。本次在 `seed-openrouter-chat` 补齐 `google/gemma-4-26b-a4b-it:free`（MoE 3.8B 激活、图+文+视频）、`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`（图+视频+音频 omni）、`thinkingmachines/inkling:free` 与 `thinkingmachines/inkling-small:free`（1M 上下文、图+音频），共 **7 个免费视觉模型**可轮换防 429。
- **修正两处误标**：`qwen/qwen3.8-27b:free`（图+文+视频）与 `openrouter/free`（自动路由，含视觉免费模型）由 `text` 改为 `vision`。`openrouter/free` 优先级置顶（100），image_url 直接走自动路由、由 OpenRouter 服务端再选最优免费视觉模型。
- **视觉甄别增强**：`payload_has_image()` 现同时识别 `video_url`（原仅 `image_url`），视频附件也路由到视觉/多模态渠道，适配 omni 模型。
- **统一选择逻辑确认**：视觉与文本共用同一引擎——`payload_has_image()` 判定 `want_kind`（vision/text），`pick_model_attempts()` 单一函数处理 text/vision/stt/tts 四类候选排序/兜底/冷却，不存在两套分支。
- 验证：e2e **208/208**（§19 含海外视觉兜底路由 + 新视觉模型清单 + video_url 甄别实测）+ 单测 **13/13**（`payload_vision_detection` 新增 + `overseas_seed_channels_well_formed` 扩展）。

## v0.12.0（2026-10-20）—— 海外模型融合（OpenRouter / Groq / NVIDIA NIM 一键载入兜底）

用户指令：「把国外的那几个模型都融和进来」。此前三个海外预设（OpenRouter /
Groq / NVIDIA NIM）仅作「登记模板」——`PRESETS` 里有、但 `/seed` 不载入、路由
也不参与。本次把它们升级为**网关种子资源**，与国内渠道同走「一键载入默认」。

### ① 海外预设模型清单实况化（2026-10 调研）

| 预设 | 旧模型 | 新模型（现行免费层） |
| --- | --- | --- |
| OpenRouter | `:free` | `openrouter/free`（自动路由）+ `nvidia/nemotron-3-super-120b-a12b:free` + `qwen/qwen3.8-27b:free` + `google/gemma-4-31b-it:free`（视觉）+ `cohere/north-mini-code:free`；17 个 `:free` 多数 50/天、部分 1000/天，免卡 |
| Groq | `llama-3.1-8b-instant` | `llama-3.3-70b-versatile` + `llama-3.1-8b-instant` + `llama-3.2-11b-vision-preview`（视觉）；免费层 14400 RPD / 30 RPM 免卡 |
| NVIDIA NIM | `nemotron` | `nvidia/nemotron-3-ultra-550b-a55b` + `nvidia/nemotron-3.5-super-vl-120b-a12b`（视觉）+ `deepseek-ai/deepseek-v4.1-flash` + `meta/llama-3.3-70b-instruct`；约 40 RPM 免卡，100+ 模型 |

> 刻意避开 `poolside` 系 `:free`（2026-10-31 到期）；免费额度随时变动，以控制台为准。

### ② 新增 `default_overseas_channels(now_iso)`

复用 `default_cn_channels` 的 `mk`/`mm` 闭包模式，生成 3 条种子：

- `id`：`seed-openrouter-chat` / `seed-groq-chat` / `seed-nvidia-chat`；
- `enabled = false`：Key 留空，登记 Key 后才参与路由（避免空 Key 打爆上游）；
- `priority`：40 / 35 / 30，**低于国内**（zhipu 100 / siliconflow 90 / zhipu-audio 80）
  —— 国内优先、海外兜底；
- `caps`：`["text", "vision"]`，每家视觉模型入 `models_meta`（gemma-4-31b /
  llama-3.2-11b-vision / nemotron-3.5-super-vl），`image_url` 才能在国内视觉全挂时
  落到海外视觉模型兜底；
- `note`：注明「海外补充、国内全挂时兜底、需 Key、可能需科学上网」。

`api_seed` 在 CN 种子之后合并海外种子，按 `id` 幂等（已存在不重复添加）。

### ③ 前端联动

- 「一键载入国内默认」按钮 →「一键载入默认（国内+海外）」，toast 文案同步；
- `presetFill` 的 vision 推断正则从 `/glm-4v|glm-4\.1v|-vl|vision/i` 扩展为
  加 `gemma`，覆盖 gemma-4 / 各 `vl` / `vision` 系海外视觉模型；
- `seed()` 现返回 6 条（国内 3 + 海外 3），e2e §12.2 / §19 同步断言。

### 验证

- 单测新增 `overseas_seed_channels_well_formed`：3 条、id 唯一、全 disabled、
  优先级 <80 且 40>35>30、各含 vision 能力、时间戳填充。
- e2e §19（v0.12.0）：seed 6 条 + 海外全 disabled + 优先级有序 + 各含 vision；
  **海外视觉兜底路由实测**——启用 openrouter 种子指向 mock，`image_url` 请求
  命中 `google/gemma-4-31b-it:free`，纯文本命中海外 text 模型；预设清单含三家
  现行免费模型。
- 全量：e2e **202/202**（原 183 + §19 新增 19，§12.2 由 3 扩到 6 的 3 条断言计入
  §12）+ 单测 **12/12**（原 11 + 1）。

## v0.11.0（2026-10-23）—— 呼叫交流模式 / 分享直通系统面板 / 多媒体容器播放

按用户测试反馈三条：「① 呼叫页面无法播放，该页面增加交流模式设置：一问一答 /
同时问答；② 分享不要弹二级页中断，直接走系统分享但没成功；③ 图片.音频视频等
多媒体支持缩略图，音视频引入容器播放」。

### ① 呼叫页播放修复 + 交流模式

**播放修复根因**（iOS/Android 媒体自动播放策略）：通话链路「按键 → 识别 →
流式 → 合成」距用户手势已数秒，`Audio.play()` 不在手势栈内被系统拒
（NotAllowedError），`speechSynthesis` 在部分 iOS/微信版本不发声。修复：
首个 `pointerdown` 手势内 `unlockMedia()` 同步预热——恢复 AudioContext、
播 0.1s 静音 WAV 换取 iOS sticky media activation、发空 utterance 唤醒
语音引擎；`openCall()` 里也显式调一次（点电话图标即手势）。

**交流模式** `callMode`（`VoiceCfg` 持久化，默认 duplex）：

- **duplex 同时问答**：播报/思考中按住说话 = barge-in 打断（v0.8.0 一等公民）；
- **half 一问一答**：thinking/speaking 期间禁抢话（toast「等 AI 说完再说话」），
  状态文案同步分化。呼叫页顶部 ⇄/↔ 按钮切换，localStorage 持久化。

**TTS 降级可见提示**：网关播音失败落浏览器语音时 toast 一次
（`ttsDegradeNotified` 防刷屏）——此前静默降级，用户侧表现就是「没坏但没声」。

### ② 分享直通系统面板（修「没成功」）

- **原流程**：点分享先弹二级对话框（链接框 + 二维码），用户再点对话框里的
  「系统分享」——多一跳，且系统面板被二级页挡住，用户感知「分享没成功」。
- **新流程**：`shareMsg()` 优先 `navigator.share()` 直通系统面板；
  `AbortError`（用户取消）安静退出；其余失败（微信内 share 常直接 reject）
  才落级原对话框；无 `navigator.share`（微信 WebView）也走原对话框。
- **顺手修 s. 子域 404（截图实测根因）**：`quakestone987.workers.dev` 是
  Cloudflare 平台域，用户无权配 `s.` 子域——旧推断逻辑必然产出不存在的子域
  （截图里 Page not found 的由来）。新增 `PLATFORM_SUFFIXES` 平台域识别
  （workers.dev / vercel.app / github.io 等 22 个公共后缀），命中即回落同源
  `/s`；自有域（www.x.com → s.x.com，生产 CNAME）推断逻辑保留。
  `shareOrigin()` 有 node 单测（location stub，6 用例）防回归。

### ③ 多媒体容器播放

- 附件 `kind` 增加 `video`；`slimAtt` 音频 + 视频存 Blob 进 IDB
  （结构化克隆，刷新不丢），图片 dataUrl 保留；
- 消息渲染：图片 `img.att-thumb` 缩略图；音频
  `<audio controls preload="metadata">`；视频
  `<video controls preload="metadata" playsinline>`；
  blob: URL 按附件对象 WeakMap 缓存（多次渲染不重复 createObjectURL）；
- **视频首帧缩略图**：`videoPoster()` canvas 抽帧（muted + metadata 预载 +
  seek 0.1s），成功存 poster 并持久化 IDB，失败/无轨降级为无 poster 播放器；
  输入区 chip 视频显示首帧缩略（抽帧未完成时 🎬 图标）；
- 刷新后音视频容器仍可播放（IDB Blob 恢复 + blob: 重建）——Playwright 实测覆盖。

### 验证

e2e **183/183**（§18 新增 20 条断言：模式定义/切换/拦截/手势预热/降级提示/
分享直通顺序/AbortError/平台域识别/多媒体四件套/shareOrigin 6 用例 node 单测）
+ 单测 **11/11** + Playwright **23 断言三场景**（分享直通三种降级路径、half 双态
禁抢话 + duplex barge-in + idle 起录、多媒体缩略/容器/poster/刷新恢复）零 JS 错误。

## v0.10.0（2026-10-22）—— 分享微信功能线：/s 二级子页 + 零存储链接分享

按用户三条优化指令之分享线：「分享微信功能线是应用分发裂变的很重要途径，
要实现，做域名二级子页实现」。

### 为什么是「URL hash 里塞正文」而不是服务端存储

微信的分享卡片**只能带一个 URL**（标题/描述由 og 标签决定，正文带不走），
且微信内置浏览器对 Web Share API 支持残缺。所以架构选择：

- 回答全文 `JSON → CompressionStream("deflate-raw") → base64url`，
  塞进 `#` 后面的 fragment（`z.` 前缀；老浏览器降级 `p.` 前缀明文）；
- `#` 片段**浏览器不会发给服务器**——服务端零存储、零数据库、零日志，
  天然「阅后即焚」，也没有任何内容合规负担；
- 压缩后一条典型回答约 150–250 字符，二维码 M 级纠错轻松扫得动；
  超过 6000 字自动截断并提示「打开应用查看全文」。

### 二级子域 s.<域名>

`shareOrigin()` 推断：`www.x.com / app.x.com → s.x.com`（生产把
`s.<域名>` CNAME 到本 Worker 即可）；`localhost` / IP 回落同源 `/s`。
新增路由 `GET /s`（与 `/s/`）返回独立分享页 `share_ui.html`
（`include_str!` 编进 WASM，随网关一起部署，不增任何基础设施）。

### 分享页能力

- 解码渲染：`z.` 解压 / `p.` 明文两条路；迷你 Markdown
  （代码块/行内代码/加粗/链接/列表/段落）；
- 微信分支：UA 检测 `MicroMessenger` → 引导「点右上角 ⋯ → 发送给朋友」；
  外部浏览器 → 「复制链接粘贴微信」+「二维码长按保存扫一扫」；
- 二维码：qrcode-generator（MIT）同库注入管理页与分享页两侧；
- 明/暗主题（data-theme 循环 + localStorage，跟随系统兜底）；
- 底部 CTA「打开 QuakeAI 应用继续对话」——裂变回流入口；
- 错误态友好提示（浏览器不支持解压 / payload 损坏）。

### 管理页分享面板

消息操作栏「分享」→ 对话框：只读链接框（点击全选）/ 复制链接
（clipboard 失败降级 `execCommand`）/ 系统分享（`navigator.share`，
不支持时按钮隐藏）/ 二维码 / 打开分享页。

### 顺手修复：p. 明文路径字母表 bug（Playwright 实测挖出）

管理页 `b64urlUnicode` 产出 base64**URL** 字母表（`-`/`_`），分享页
p. 分支却直接 `atob`（只认 `+`/`/`）——中文内容几乎必含 `-`/`_`，
无 CompressionStream 的老设备上降级路径**必炸**。已修为先归一化
再 atob，并加跨文件互逆断言防回归（管理页编码器 → 分享页解码器，
z./p. 两条路都跑真实往返）。

### 验证

- e2e **164/164**（§17 新增 18 条断言：路由 include/404 文案/分享页
  解码器/微信引导/QR 注入/上限保护/子域推断/降级链/真实 HTTP 200/
  Content-Type/字母表归一化/**编码-解码跨文件互逆**）；
- 单测 **11/11**；
- Playwright 六场景实测：生成分享链接（同源 /s#z.）、二维码生成加载、
  新页打开渲染、微信 UA 转发引导、复制链接剪贴板、p. 明文降级渲染
  （加粗/行内代码）——零 JS 错误。

## v0.9.0（2026-10-22）—— 聊天记录迁 IndexedDB：可删可查 / 联动视图 / 流式持久化 / 新会话修复

按用户三条优化指令之聊天记录线：「这些进浏览器 indexdb 的 可删可查
可联动视图 会话流要去适应 新会话目前没做好」。

### 为什么离开 localStorage

① **5MB 字符串配额**——图片 dataUrl（base64 数百 KB）只能剥离、会话 60 条
封顶；② `setItem` 同步序列化大 JSON 卡主线程（流式期间每次存都掉帧）；
③ 整坨读整坨写，做不了消息级增删与跨会话搜索。

### IDB 结构（gw_convos / v1）

- `convos`（keyPath id，updated 索引）：会话元信息；
- `msgs`（**复合主键 [convoId, idx]**，by_convo 索引）：消息级存储——
  重发截断后同位置重新入列 = 覆盖，不会重复；
- 附件随消息持久化（`slimAtt`）：图片留 dataUrl（刷新后缩略图不丢）、
  音频留 Blob（刷新后重发转写仍可用）、普通文件只存元信息；
- 启动迁移：localStorage `gw_convos_v1` → IDB，迁完删旧 key 留标记，
  无双数据源；IndexedDB 不可用（隐私模式等）降级纯内存 + 明确提示。

### 可删

- 会话级：侧栏 ✕ → `deleteConvo`（convos + 该会话全部 msgs）；
- 消息级：长按菜单新增「删除」→ idx 前移整段重写 + 旧末位游离记录
  精确清理（按复合主键删，不会误伤）；
- 全部：侧栏「🗑 清空全部会话」（confirm 防误触）。

### 可查

- 侧栏搜索框：标题即过滤（同步呈现）+ 消息内容跨会话命中
  （`allMsgs` 扫描，按会话聚合并显示片段，「内容命中 N 个会话」）；
- Esc 清空 / 回车打开第一个命中。

### 可联动视图 + 会话流适应

- 搜索态 / 删除 / 打开全部走同一 `renderConvos/renderMessages` 链路，
  侧栏高亮、首页最近会话 chips、顶部标题三处联动；
- **流式持久化**：SSE 循环内 `markMsgDirty` 节流落库（800ms），终态
  立即写；`pagehide` + `visibilitychange` 双兜底冲刷挂起写——
  流式中刷新/崩溃，已生成部分不丢，重开标记「（生成中断）」；
- 异常退出的流式消息重开自动标记中断，不再带光标。

### 新会话修复（「新会话目前没做好」的实锤）

根因：旧流还在跑时点「＋ 新对话」，发送键还是旧流的**停止键**——
新对话里打字发送 = 中止旧流，什么都发不出去。
修复：`newConvo()` 先用 `streamGen` 代号抢占旧流（abort + 收尾），
旧流的 finally 检测到代号过期只落库不碰 UI，新会话立即可发送。

### 顺手修复：toast 拦截点击（实测挖出）

`.toast` 单独开了 `pointer-events: auto`，悬浮的 4.6s 内底部输入区
（含发送按钮）**点不动**——用户视角就是「界面卡了」。toast 是纯通知，
改为继承容器的 `pointer-events: none`。Playwright 实测复现 + 修复验证
（清空会话后 500ms 内点发送即生效），e2e 加防回归断言。

### 验证

- e2e **146/146**（§16 新增 30 条断言：IDB 结构/迁移/流式持久化/
  冲刷/新会话抢占/三级删除/搜索/附件持久化/中断恢复/toast 不拦截）；
- 单测 **11/11**；
- Playwright 九场景实测（无头 Chromium）：旧数据迁移、发送-刷新恢复、
  流式中刷新、新会话抢占旧流、标题/内容搜索、删消息/删会话/清空、
  图片附件刷新后缩略图加载——零 JS 错误。

## v0.8.0（2026-10-22）—— 语音全链路重构：状态机 + 单消费者 + TTS/STT 双兜底

v0.7.5 修完「第二次就挂」后，按用户三条优化指令把语音这条功能线补齐：
TTS 去 GitHub 抓精简项目改造融合做兜底、STT 加兜底模型、双向交互引入
状态机（中断是高频事件）。核心思路：**把「哪个组件此刻独占麦克风/扬声器」
变成显式状态，从结构上消灭竞争**。

### ① 麦克风单所有者 MicHub（「第二次就挂 / STT 跑不通」的根治）

所有取麦统一走 `MicHub.acquire(owner) / release(owner)`：新 owner 抢占前
**先彻底释放旧所有者的流**再要新的。浏览器语音识别与 MediaRecorder
绝不并存——`VoiceInput` 一次输入只用一个消费者：识别可用（Chrome/Edge）
走 `SpeechRecognition`（continuous+interimResults，零上传、真实时文字流），
不可用（Firefox/Safari）或用户强制网关模式才 `MediaRecorder` + 网关转写。
输入框长按语音与通话页共用同一套（owner 分别为 input / call），
互相抢占时旧会话被 `abort()` 同步取消。

### ② 通话状态机 CallFsm（barge-in 打断是一等公民）

六态 `closed/idle/listening/transcribing/thinking/speaking`，事件驱动
显式迁移，非法迁移直接拒绝（忙态按钮点不动）：

- **打断（barge-in）**：`speaking/thinking` 态 press → `listening`，
  立即停播（`ttsStopAll`）+ 中断在途 SSE 流 + 清空 TTS 队列 + 释放麦克风，
  不等当前句播完——双向语音里打断是高频操作，不允许「等 AI 说完」；
- 每个 UI 动作都是「状态 + 事件 → 新状态」，从结构上消灭两轮交错/声音叠加；
- 状态文案/精灵球四态/按钮 busy 全部由 `callSyncUi()` 按状态派生，
  不再各处手工同步。

### ③ TTS 三层递降链（Speakit-JS 融合体做浏览器兜底）

去 GitHub 抓了 **Speakit-JS**（MIT，2KB，对 `speechSynthesis` 的 promise
化封装）改造融合，并吸收 Easy Speech 的语音列表加载策略
（`getVoices` → `onvoiceschanged` → 1.5s 轮询兜底）：

- **递降链**：网关 `/v1/audio/speech` → **自定义端点**（OpenAI 兼容，
  客户自有 TTS 接口资源的替换点，填 URL/Key/模型/音色即换）→
  浏览器 `speechSynthesis`（离线可用的最后兜底）；
- 仅网关模式不降级（错误明示）；auto/custom 任一层失败自动落下一层；
- 浏览器 TTS 长文本按句切片 + 120 字硬切（Chrome 超长 utterance
  静默截断）；`cancel → onerror → reject("stopped")` 作为控制流信号
  被静默消费（打断/挂断不误报错）；
- 新增 **🎛 语音设置对话框**：TTS/STT 模式、自定义端点、浏览器音色、
  语速，配置存 localStorage（`gw_voice_cfg`），实时预览递降链。

### ④ STT 两层链

浏览器实时识别（零上传）优先 → 网关 `/v1/audio/transcriptions`
（MediaRecorder 250ms 时间片 + 每 2s 阶段性全量转写做实时文字流）。
「一直跑不通」的软件层根因就是双消费者互踩，单消费者原则已根治。

### 验证

- e2e **124/124**（§15 重写为 27 条 v0.8.0 架构断言：MicHub 抢占释放/
  VoiceInput 单消费者/FSM 六态/barge-in 迁移/早锁/递降链/切片/停止语义/
  防重入/超时兜底/挂断中断/配置持久化）；
- 单测 **11/11**；
- Playwright 六场景实测（无头 Chromium）：输入框语音两次无锁死、
  通话全流程 FSM 闭环、**播报中抢话打断**（speaking→listening +
  pause 调用证据）、浏览器 TTS 实际播音、custom 端点坏自动降级
  浏览器兜底、设置对话框保存持久化——零 JS 错误。

## v0.7.5（2026-10-21）—— 修复呼叫页「第二次就挂」（麦克风竞争 + 播报叠加）

用户实测「呼叫 第一次识别可以 第二次就挂了」，并精准怀疑「麦克风资源占用了
没释放」。前端实测复现后实锤三个根因，全部修复：

### 根因① callBusy 加锁太晚 → 两个 MediaStream 并发（主因）

`finishCallVoice` 的 `callBusy = true` 加在 `await rec.stop()` **之后**：
松手瞬间连按第二次按钮，第一轮的收尾还在 await 里（MediaStream 还活着），
第二轮已经拿到锁又去 `getUserMedia`——**iOS 对硬件占用直接拒绝**，
表现就是「第二次就挂」。

- 修复：锁提前到 **录音启动即持锁**（`callRec.start(250)` 后立即
  `callBusy = true`），整个「录音→识别→思考→播报」全程禁第二轮；
  `finishCallVoice` 同样在首个 await 之前加锁；
- 兜底：入口先回收可能残留的定时器/录音器/流；`pointerup` 丢失
  （切后台/来电）时 60s 自动收尾；两个 finish 函数加防重入标志
  （超时兜底与 pointerup 可能同时触发）。

### 根因② pumpTts 未被 await → callBusy 早释、两轮播报声音叠加

`chatStream` 结束只代表**文字生成完**，分句 TTS 的句子还在队列里播。
原来 `callAskStreaming` 不等队列排空就返回，`callBusy` 提前松掉，
第二轮说话与第一轮播报**声音叠加**、UI 状态与实际播放不符。

- 修复：新增 `ttsQueueDrained()`/`flushTtsWaiters()`，
  `callAskStreaming` 在 `chatStream` 后 `await` 播报排空；
- `pumpTts` 播空时唤醒等待者；`stopCallAudio` 手动唤醒被 `pause()`
  卡住的当前播放（pause 不触发 onended，否则 pumpTts 永挂）；
- drain 期间用户挂断 → 旧链路干净收尾，不再向已重置的历史入账。

### 根因③ 每次阶段性 STT 都 new AudioContext → iOS 并发上限触顶

实时文字流每 2s 一次阶段转写，每次 `toWavBlob` 都 `new AudioContext()`
再 `close()`——**iOS 对实时 AudioContext 并发数有上限**，累积触顶后
麦克风/音频全面失灵，同样表现为「第二次挂」。

- 修复：全局单例 `_wavCtx` 复用（解码失败才丢弃重建）；
  OfflineAudioContext 本就是轻量一次性对象，不受影响。

### 附带改进

- 挂断时 `AbortController` 中断进行中的 chat SSE 流——不必等上游把话说完；
- 输入框长按语音同步同款策略：录音启动即持锁 `voiceBusy`，
  所有出口（取消/空录音/识别失败/自动发送）统一走 finally 松锁；
- e2e §15 新增 11 条前端防回归断言（早锁位置/drain/AudioContext 单例/
  防重入/超时兜底/入口清理/流式中断）。

## v0.7.4（2026-10-20）—— 修复侧栏版本号显示过期值（「前端版本变成 6.0」）

### 根因

管理页侧栏版本号 `<span id="side-version">` 的 HTML 初始值是 **v0.6.0 时代
的硬编码**，之后 v0.7.0~v0.7.3 四个版本都没改。只有 `loadAll()` 成功拿到
`/channels` 数据时才会被真实版本覆盖——**接口一失败（store 报错/网络抖动），
侧栏就一直显示 v0.6.0**，用户误以为部署退回了旧版本。

### 修复

- HTML 初始值 `v0.6.0` → `…`（永不再显示假版本）；
- 新增 `refreshVersion()`：`loadAll` 的 catch 分支也尝试从 `/health`
  取真实版本兜底——`/channels` 挂了口径也不丢；
- e2e 新增防回归断言：`GET /admin` 的 HTML 禁止出现硬编码版本号
  （`id="side-version">v<数字>`），且必须含 refreshVersion 兜底。

### 更深根因（实测顺带挖出）

侧栏版本取自 `/admin/api/channels` 的 `version` 字段，而 `admin.rs` 里有个
**独立的 `const VERSION = "0.7.2"`**——从 v0.7.2 起每次升级只改了 Cargo.toml
和 lib.rs 的三处字面量，**从来没改过这个常量**，所以接口一直报旧版本。
- 根治：`admin.rs` 的 VERSION 与 lib.rs 三处版本字面量全部改为
  `env!("CARGO_PKG_VERSION")`（编译期从 Cargo.toml 注入）——**版本号
  单一事实源，以后升级只改 Cargo.toml 一处**；
- 单测断言 `admin::VERSION == CARGO_PKG_VERSION`；e2e 断言
  health / channels / Cargo.toml **三处版本一致**；
- Playwright 三场景实测：正常加载 → v0.7.4；channels 接口 500 →
  v0.7.4（/health 兜底）；全部接口挂 → `…`（绝不显示假版本）。

## v0.7.3（2026-10-20）—— 实时语音通话：STT 文字流 / 分句实时播报 / 拟人化呼叫页

用户实测「播音失败: messages 参数非法」+「呼叫页要改」后的根修与升级：

### 1. 播音「messages 参数非法」根因（1214）

- 实锤链路：老账本音频渠道登记的 tts 模型是 `glm-4-voice`——它是**端到端
  语音对话模型**，打到 `/audio/speech` 会被上游按 chat 参数校验，报 1214
  「messages 参数非法。请检查文档。」v0.7.2 只改了请求时改写表，**已部署
  的旧实例仍在跑旧代码**，且账本里的旧模型名原样存在。
- 根治（双保险）：
  - `resolve_stt/tts_model()` 对智谱系渠道**一律收敛**到端点唯一正确模型
    （glm-asr-2512 / glm-tts）——转写/合成端点目前就只有这两个模型，
    任何误配（glm-4-voice、glm-asr、手滑填的 chat 模型）都在请求前被改写；
  - `store::load_all_channels()` 出口做**账本零迁移**：audio 渠道 models_meta
    里的 glm-4-voice(stt)→glm-asr-2512、glm-4-voice(tts)→glm-tts，
    只改内存态不回写 DO，路由层从此干净。
- 前端播音/播报失败时给出针对性提示（识别 1214 → 引导升级/检查渠道模型）。

### 2. STT 实时文字流（通话页 + 长按发语音）

- 录音过程中每 2s 把「到目前为止的音频」整体转写一次（智谱转写无状态，
  全量重发即可），实时文字流进识别区；busy 防慢请求叠加，失败静默
  （松手后的最终转写兜底）。录音超 45s 自动停阶段转写只做最终。

### 3. 分句实时 TTS 播报（模型边想边播）

- 新增 `chatStream()`：chat SSE 增量按句末标点（。！？；!?\n + 英文句点
  后跟空白，不切开 3.14 这类小数）切句，凑满一句立即 `ttsEnqueue()`；
  无标点超长片段 120 字强制切，避免播报迟迟开始。
- TTS 播放队列：合成一句播一句、顺序播放；文字同步滚动；挂断全中断。

### 4. 呼叫页拟人化 UI + 模型切换

- 中央「AI 精灵球」四态动画：idle 呼吸 / listening 双波纹外扩 /
  thinking 快速微颤 / speaking 球内声浪柱——UI 状态对应模型的听/想/说。
- 顶部模型切换按钮（通话模型独立记忆 `gw_call_model`，空 = 网关自动选路），
  切换后下一句生效。
- 输入框长按发语音同样支持实时识别（录音浮层内文字流）。

### 验证

- 单测 10/10（新增 resolve 收敛 3 组）；WASM 0 告警；
- e2e **92/92**（新增 TTS 收敛断言：上游必收 glm-tts + tongtong）；
- 前端 JS 过 node --check；句子切分逻辑 node 单测 8/8（中文/英文/小数/
  换行/超长无标点）。

## v0.7.2（2026-10-20）—— 智谱语音端点模型修正（STT/TTS 各归其位）

用户实测「语音识别失败: API 调用参数有误…message should not be empty」后的根因修复：

### 根因（查官方文档确认）

- 智谱 `/audio/transcriptions`（转写）只认 **ASR 模型 glm-asr-2512**；
- 智谱 `/audio/speech`（合成）只认 **TTS 模型 glm-tts**（音色 tongtong 等）；
- 我们此前两处都用 `glm-4-voice`——它是**端到端语音对话模型**（走
  chat.completions + input_audio），拿到转写/合成端点会被上游按 chat 参数校验
  拒绝，即 "message should not be empty"；TTS 默认音色还是阿里的 `tongyi-qwen`。

### 修复

- `resolve_stt_model()` / `resolve_tts_model()`：智谱系渠道自动把旧模型名
  （glm-4-voice 等）改写为正确端点模型——**老账本零迁移，用户零感知**。
- 转写请求补 `stream=false`（官方基础调用必带，否则流式处理拿不到 text）。
- TTS 默认音色 `tongtong`、显式 `response_format=wav`、content-type 回退改 wav。
- 默认模型/种子渠道/预设全部更新为 glm-asr-2512（stt）+ glm-tts（tts）；
  音频渠道按模型名推导能力（`*tts*` → tts，其余 → stt）。
- mock multipart 日志补 authorization 字段；e2e 90/90（含旧模型名改写断言）。

## v0.7.1（2026-10-20）—— 输入法布局 / STT 格式 / 播音模型 三修复

用户实测反馈的三连修：

### 1. 输入法弹起后 App 内容消失（iOS）

- 根因：旧代码只用 `visualViewport.height` 驱动 `--app-h`，没处理 iOS 键盘弹起时
  `visualViewport.offsetTop > 0`（可视区整体上移）——App 仍贴在布局视口顶部，
  被顶出可视区，屏幕只剩输入条。
- 修复：新增 `--app-top` 变量，`body { height: var(--app-h); transform: translateY(var(--app-top)) }`，
  App 与可视区精确对齐；固定定位元素（侧栏/浮层/Toast）随 body 变换，全部落在键盘上方。

### 2. 语音识别报「transcriptions不支持当前文件格式」

- 根因：iOS Safari 的 MediaRecorder 产出 `audio/mp4`，智谱 glm-4-voice 只认 wav/mp3。
- 修复：前端新增 `toWavBlob()` / `encodeWav()`——录音/音频附件统一走 WebAudio 解码 →
  OfflineAudioContext 重采样 16kHz → 16bit PCM WAV 再上传（wav/mp3 原样透传，
  解码失败原样兜底）。麦克风长按、附件语音、通话浮层三条链路同时生效。

### 3. 播音提示无可用模型

- 根因一：`handle_audio_speech` 的候选池只加载 chat 渠道，而 tts 模型登记在音频
  渠道上 → 永远选不到。修复：候选池 = chat + audio 双池合并。
- 根因二：`pick_model_attempts` 能力匹配过严，音频渠道的 stt 模型（glm-4-voice
  兼具转写与合成）被排除。修复：tts 允许回落 `kind=="stt" && 渠道 kind=="audio"` 的模型。
- 加固：Key 借用后仍为空的候选直接剔除并返回明确 503（不再带着空 Bearer 吃上游 401）。

## v0.7.0（2026-10-20）—— 模型选择引擎 + 设备级隔离 + 通话/播音

围绕用户三条核心诉求落地：**「模型要精细化管理、出错自动换模型」**、**「key 要绑定设备、
别因升级丢失」**、**「回答可复制分享播音、能一键双向通话」**。

### 容器云部署（Render / Railway / Fly，Docker Runtime）

- 新增根 `Dockerfile` + `deploy/start.sh` + `.dockerignore`：镜像内无 Rust 工具链
  （`build/` 两产物已随仓库提交），启动时按 `PORT` / `ZHIPU_API_KEY` / `ADMIN_TOKEN` /
  `ZHIPU_BASE_URL` 生成 workerd 配置并 `exec workerd serve`；DO 持久化落 `STORAGE_DIR`
  （默认 `/data`，对应 Render Persistent Disk）。
- **不能用 Rust Runtime**：本项目是 cdylib 无 bin target，`cargo run` 报
  `a bin target must be available`（Exit 101）——服务类型必须切 Docker。
- 实测通过：镜像构建、`/health` 0.7.0、`/admin` 200、登记渠道后容器重启 Key/渠道仍在。

### 模型选择引擎（后端 `src/lib.rs` / `src/channels.rs`）

- **四级能力模型**：每个模型标注 `text / vision / stt / tts` 之一（旧账本按模型名
  推导：`-vl`/`vision`→视觉、`glm-4v` 系列→视觉、audio 渠道→stt、其余→text）。
  请求按能力选模型：`payload_has_image()` 检测 `image_url` parts → 走 vision 模型。
- **模型级路由 `pick_model_attempts()`**：请求指定模型时，把所有**提供该模型**的渠道
  排在最前（跨渠道按优先级）；任一个上游失败即换下一个渠道；主选全部失败再回退到
  **同类其他模型**（实现模型级自动更换）。未指定模型则按优先级用全部同能力模型。
- **模型级健康与冷却**：`ModelMeta.health`（继承 `Channel.Health` 的
  `fail_count` / `cooldown_until_ms`）独立记账；429 时冷却
  `COOLDOWN_MS × min(fail_count,5)`，`ensure_models_meta()` 把旧账本推导结果固化为
  落点，首写健康即落地。冷却中的模型/渠道自动跳过。
- **Key 复用（跨渠道）**：`borrow_key_if_absent()` 让暂缺 Key 的音频渠道借同
  `provider`（或 `base_url`）聊天渠道的 Key——用户只在智谱主线登记一次即可（修复
  「语音识别报错」）。音频/stt 渠道允许暂缺 Key 进入候选池，借到后再发请求。
- **TTS 播音端点**：`POST /v1/audio/speech`（`{model,input,voice}`）→ 上游
  `/audio/speech`，回退 `audio/mpeg`；供「播音按钮」与「双向通话」复用。
- **无候选 503**：某能力无任何可用模型时明确回 `no_candidate_model`（中文能力说明），
  不再静默 500。

### 设备级 Key 隔离（前端 + 后端）

- 浏览器生成设备序列号 `gw_device_id`（localStorage），按 provider 缓存一份本机
  Key 到 `gw_device_keys`；所有 fetch 带 `x-device-keys` 头（Rust 侧
  `apply_device_keys()` 仅内存并入、不写共享账本）。
- 语义：A 设备不想用 B 登记的共享 Key——登记页新增「本设备 Key」卡片，按 provider
  录入/编辑/清除；账本已登记 Key 的渠道优先级更高，设备 Key 只在账本为空时补位。
- 配套修复「升级后 Key 全丢」：DO 改 `localDisk`（`.wd-storage/`，已 gitignore），
  重启进程后渠道与掩码仍在（e2e 12.10 断言）。

### 前端体验

- **回答操作按钮组**：每条助手消息尾部固定「复制 / 分享（navigator.share 降级
  clipboard）/ 🔊 播音（走 `/v1/audio/speech` → Audio 播放，再点停止）」。
- **历史进本机缓存**：会话落 `gw_convos_v1`，欢迎页新增「最近会话（本机缓存）」chips
  行（最多 5 个），点击即开。
- **右上角双向通话**：📞 按钮唤出全屏通话浮层——按住说话（复用录音链路）→ STT 转写
  → chat 回复 → TTS 自动播报 → 挂断结束（`openCall`/`endCall`）。
- **消息长按菜单**：长按 500ms / 右键消息弹出「重发 / 复制」；`sendOrStop` 重构为
  可注入 `text/attachments` 的 `sendWith`，重发走同一链路。
- **输入法弹起布局**：`visualViewport` 监听驱动 `--app-h`（替代桌面/移动不一致的
  `100dvh`），输入框 auto-grow 上限跟随视口高度。
- **明暗主题**：🌙 切换 `data-theme`，偏好存 localStorage。
- **模型管理升级**：渠道卡片内模型行——能力徽章（文本/视觉/STT/播音四色）、
  模型级状态点（健康/冷却 Ns）、优先级、启用/禁用、单独探测；卡片顶部能力筛选
  chips（全部/文本/视觉/STT/播音）。

### 验证

| 套件 | 结果 |
| --- | --- |
| `cargo test --release`（lib） | ✅ 7/7 |
| `cargo check --target wasm32-unknown-unknown` | ✅ 0 警告 |
| `bash scripts/test-e2e.sh`（mock + workerd，89 断言） | ✅ **89/89** |
| 前端内嵌 JS 语法 + DOM/函数引用静态校验 | ✅ 通过 |

e2e 新增断言：vision 路由（image_url→glm-4v-flash）、纯文本→text 模型、跨渠道 Key
复用（音频渠道无 Key 借主线 Key）、TTS 返回 `audio/mpeg`、模型级 429 冷却窗口 +
自动换模型、DO 持久化（重启不丢渠道/Key）。`mock-zhipu.mjs` 新增 `/audio/speech` 端点。

---

## v0.6.0（2026-10-19）—— 输入区增强：语音长按 + 本地资源提交

对接下一个模型之前，先把移动端输入体验补齐全（用户需求：「优化输入框加语音按钮
（长按发语音），还有加个 + 号可提交本地资源 照片.视频.文件等」）。

### 语音（🎙 长按说话）

- **交互**：按住麦克风说话，松开发送；上滑超过 60px 进入取消态（浮层变红、
  提示「松开取消」），松手放弃本次录音。`pointerdown/up` + `setPointerCapture`
  保证手指滑出按钮也不丢事件；`contextmenu` 已阻止，长按不弹系统菜单。
- **链路**：MediaRecorder（250ms timeslice）→ Blob → `POST /v1/audio/transcriptions`
  （复用网关 multipart 上传 + audio 渠道）→ 转写文本填入输入框并自动发送。
  与「选音频文件发送」同一条转写链路，双入口统一。
- **反馈**：录音浮层含声波动画、计时器（0:00 起）、取消态；按钮录音脉冲动画。

### 本地资源（+ 号）

- **图片**：读为 data URL，以 OpenAI vision parts
  （`{type:"image_url", image_url:{url:dataURL}}`）随 messages 发送——
  需渠道模型支持视觉（GLM-4V / Qwen-VL 等）；用户气泡同步渲染 76px 缩略图。
- **音频**：走上面的转写链路，识别结果拼入正文。
- **视频/PDF/其他文件**：网关暂无文档解析端点，降级为
  `[附件: 名称（大小）]` 文字说明附在正文前，模型至少知道有这东西。
- **持久化配额保护**：附件 dataUrl/File 只在内存存活，`saveConvos()` 深拷贝
  剥离——图片 base64 动辄数百 KB，不剥离会瞬间撑爆 localStorage 5MB 配额
  连坐全部会话。刷新后图片附件降级为 `📎 文件名` 文字标签。

### 顺手修复的真实缺陷

- **刷新后当前会话丢失**：`currentId` 初始恒为 `null`，刷新/重开后会话列表
  还在但消息区空白，必须手动点开。现在恢复 `updated` 最新的会话（聊天应用
  常态行为），侧栏高亮同步。
- **e2e 脚本进程管理竞态**：`stop_workerd` 用 `$!` 记录的是 setsid 的 pid
  而非 workerd 本身（setsid 可能 fork），杀不掉真实进程；下一段 start 的
  新实例绑定 8787 失败，而 readiness 循环却打到尚未退出的旧实例上——
  导致后续段实际跑的是上一段配置（表现为 ch-blank/flaky 段随机失败）。
  改为按端口 `ss` 反查真实 pid + 等端口真正释放。

### 验证

| 套件 | 结果 |
| --- | --- |
| `cargo test --lib`（含 header_safe 编码/往返） | ✅ 7/7 |
| `cargo check --target wasm32-unknown-unknown` | ✅ 0 警告 |
| `bash scripts/test-e2e.sh`（mock + workerd，70 断言） | ✅ 70/70 |
| puppeteer 浏览器交互回归（+ 语音/附件链路，24 断言） | ✅ 24/24 |

浏览器交互回归覆盖：附件选择/chip 渲染/移除、图片+文字一起发送（气泡含缩略图）、
mock 透传回复、徽章无乱码、长按录音→浮层+计时→松手转写→自动发送全链路、
上滑取消（浮层收起、不产生消息）、localStorage 无 base64、刷新后附件降级
为文件说明。截图：`docs-shots/手机端-输入区增强.png`（420×860 手机视口）。

---

## v0.5.1（2026-10-19）—— 诊断头中文乱码修复

**现象**：线上聊天回复的渠道徽章显示 `via Ã¦ÂÂç GLM ä¸­ç«`——中文渠道名变乱码。

**根因**：HTTP 响应头不允许非 ASCII 字节。浏览器按 Latin-1 解响应头，
网关此前把中文渠道名（`智谱-JWT`）的 UTF-8 字节直接写进
`x-gateway-channel` 头，前端读出来就是 Mojibake。响应体（JSON）不受影响，
所以只有徽章乱，聊天内容正常。

**修复**：所有可能含中文的诊断头值（渠道名、attempts 里的渠道 ID）
在 Rust 侧经 `header_safe()` 做 percent-encoding（RFC 3986 非保留字符
原样保留，非 ASCII 字节 → `%XX` 大写十六进制），上线响应头保证纯 ASCII；
前端读取处用 `decodeHeader()`（`decodeURIComponent` + 失败兜底）还原。
`x-gateway-version` / `provider` / `base-url` / `model` 等 ASCII 值不受影响。

**验证**：单测 +2（共 7/7：编码正确性 + 往返无损，含 `%`/`&`/emoji 边界）；
e2e +1（共 70/70：头内 ASCII 编码断言 + 解码还原「智谱-JWT」往返断言）；
前端 JS 语法校验通过。

---

## v0.5（2026-10-19）—— 智谱 JWT 自动鉴权

背景：用户最初提供的智谱 Key 是控制台显示界面里的**掩码值**（`5bc8...4bRY`），
直调 `/api/paas/v4/*` 返回 401「令牌已过期或验证不正确」。拿到完整两段式 Key
后的真实联调证实：完整原始 Key 与 JWT 两种方式均可 200，JWT 是官方 develop
文档推荐的鉴权方式（免手工管理、payload 自带过期时间）。本版让网关自动处理：

- **自动换 JWT**：`provider=zhipu` 且 Key 为 `{id}.{secret}` 两段式时，
  上游请求头改为现场生成的短期 JWT——header `{"alg":"HS256","sign_type":"SIGN"}`、
  payload `{"api_key":id,"exp":now+1h,"timestamp":now}`（毫秒 epoch）、
  HMAC-SHA256(secret) 签名。其余 provider（硅基流动等）仍原样 Bearer。
- **三条路径统一出口**：chat、audio、连通性探测全部走 `bearer_token()`
  （探测也走同一逻辑，智谱渠道测试行为与真实请求一致）。
- **零依赖实现**：`src/zhipu_jwt.rs` 手写 SHA-256 + HMAC-SHA256 + base64url，
  不为百行算法拉入 ring/hmac 重型依赖；正确性由标准测试向量背书
  （`cargo test --lib` 5/5：FIPS 180-4 向量、RFC 4231 HMAC 向量、
  base64url 边界、JWT 结构可复算、坏 key 拒绝）。
- **前端提示**：登记/预填智谱渠道时显示 Key 格式说明（两段式、自动换 JWT）。
- **e2e 覆盖**（+5 项，共 69）：mock 验证 JWT 三段式结构与
  alg/sign_type/api_key/1h-TTL，上游实收 `Bearer eyJ…` 而非原始 Key。

### 验证

| 套件 | 结果 |
| --- | --- |
| `cargo test --lib`（密码学标准向量） | ✅ 5/5 |
| `cargo check --target wasm32-unknown-unknown` | ✅ 0 警告 |
| `bash scripts/test-e2e.sh`（mock + workerd） | ✅ **69/69** |
| puppeteer 浏览器交互回归 | ✅ 17/17 |
| 真实 Key 联调 | ✅ 硅基流动 200 通（直连 + 经网关）；智谱 200 通（直连原始 Key / 直连 JWT / 经网关 JWT 三条路径均验证） |
| 智谱真实端点经网关 e2e（13 断言） | ✅ 13/13：非流式回复、渠道命中、诊断头（version 0.5.0 / provider=zhipu）、Key 掩码 `9f1***qVph` 无明文、连通性探测上游 200、SSE 流式、测后渠道删除 |

---

## v0.4（2026-10-19）—— 登记 500 修复 + ChatGPT 式前端 + 模型管理二级子页

本轮修复三个用户反馈问题，全部经本地 workerd e2e（64/64）与浏览器交互回归
（puppeteer 17/17）验证。

### 1. 智谱登记报「保存失败: HTTP 500」—— 已修复

**根因**（两处，都在管理面写入路径）：

| 位置 | 问题 |
| --- | --- |
| `src/admin.rs` `api_update` | `save_all_channels(env, &list).await?` 裸 `?`：DO 不可用时错误直接冒泡 |
| 旧部署版 create 路径 | 同款裸 `?`（本次一并硬化） |
| `handle_admin` | 无外层兜底，任何未预期 Err 都变成 Workers 默认 500 纯文本页 |

另有一个隐藏坑：截图里 Key 显示为 `5bc8...4bRY`（带省略号）——这是**掩码值**
（前 3 后 4 格式应为 `5bc***4bRY`）。旧逻辑会把掩码当明文写进账本，之后路由
全部 401。

**修复**：

- `save_all_channels` 失败统一走 `store_unavailable()`：**503** +
  `store_unavailable` 错误码 + Durable Object 绑定指引
  （变量名 `RESOURCE_STORE` / 类名 `ResourceStore`，或打开 `/admin/api/diag` 自检）；
- `handle_admin` 拆成内外两层：外层兜底把未预期 Err 转 **JSON 500**
  （`internal_error` + 排障提示），不再吐 Workers 默认纯文本 500；
- `api_create` / `api_update` 请求体解析改 `req.text().await.unwrap_or_default()`，
  空 body 视为 `{}`；
- 掩码值（含 `***` / `...` / `…`）在**后端**直接 400 `masked_key` 拒绝，
  **前端** `submitChannel` 也有预检，根本不会发出去。

### 2. 手机端 UI 变形 —— 已重写为类 ChatGPT 布局

**根因**：`src/admin_ui.html` 的 JS 在 `updateChatTop` 处被硬截断（543 行），
`loadAll` / `submitChannel` / `sendOrStop` 等后半部分全部缺失——页面能渲染但
所有交互都是死的，窄屏下残缺的 CSS 就表现为"变形"。

**修复**：完整重写（约 1000 行），布局对齐 ChatGPT：

- **对话首页 `#/`**：消息列表 + 底部输入框（auto-grow textarea）+ 发送/停止
  同一按钮；支持 SSE 流式逐帧上屏、`[停止]`（AbortController 中断 fetch）、
  reasoning 折叠块、markdown-lite 渲染；
- **模型管理 `#/models`**：二级子页，左上角 ← 返回；侧栏底部「⚙ 模型管理」
  全宽按钮、顶栏入口按钮（移动端仅图标）、无渠道时欢迎区大按钮，三个入口；
- **渠道徽章**：每次回复显示命中渠道（`via 智谱 BigModel`），发生自动切换时
  显示「⤻ 已自动切换」并列出 attempts；
- **移动端**：`100dvh` + `env(safe-area-inset-*)` 适配刘海/小白条；侧栏改抽屉
  （遮罩点击关闭、`body.nav-open` 锁滚）；所有对话框改底部弹层
  （drag handle + 上滑动画）；渠道网格移动端单列；顶栏无横向溢出。

### 3. 模型管理入口（首页二级子页）—— 已实现

hash 路由：`#/` 对话首页，`#/models` 模型管理；`/admin` 无 hash 时兼容落
`#/models`（旧链接不破）。模型管理页内容：store 不可用红色横幅（DO 未绑定
指引）、5 项统计、预设目录（搜索过滤 + 「登记」预填）、渠道卡片（健康点/
冷却倒计时/优先级▲▼/测试/换 Key/编辑/删除/启停）、事件账本、diag 弹窗。

### 其他

- **hash 路由修正**（截图验证时发现）：原 `currentView()` 把 `#/` 和空 hash
  一起按路径名兜底，导致从 `/admin#/models` 点「← 返回对话」设了 `#/`
  却仍停在 models 页（旧链接访客的返回键坏死）。改为显式 hash 优先：
  `#/models` → models、`#/` → chat，**只有空 hash** 才按路径名兜底
  （`/admin` 无 hash 落 models 的兼容规则不变）。
- `scripts/make-single-file.sh` 适配新版 worker-build 压缩 glue
  （`import X from"./index_bg.wasm"` 无空格写法；改为删 import + 注入同名
  `const X = new WebAssembly.Module(__wasmBytes())`，对所有引用点生效）；
- Rust 侧清理全部编译警告（dead_code / unused mut / 未使用常量归位），
  `COOLDOWN_MS` 提升为 `pub(crate)` 供 admin.rs 两处冷却写入共用；
- `scripts/test-e2e.sh` 的 workerd 路径改为自动探测（`.tools/workerd` →
  `$WORKERD_BIN` → 旧路径）；
- 版本号 0.3.0 → 0.4.0（health / 诊断头 / Cargo.toml / 文档）。

### 验证

| 套件 | 结果 |
| --- | --- |
| `cargo check --target wasm32-unknown-unknown` | ✅ 0 警告 |
| `bash scripts/test-e2e.sh`（mock + workerd） | ✅ **64/64** |
| puppeteer 浏览器交互回归（手机/桌面视口） | ✅ **17/17** |
| 单文件部署版 smoke（health/chat/admin/seed） | ✅ |

17 项交互回归含：手机端顶栏无横向溢出（实测 `entry-btn right=378 < 390`）、
抽屉开关、模型选择器、SSE 流式内容/徽章/推理折叠、停止按钮、二级页进入与
← 返回（含 `/admin` 旧链接场景）、登记弹层、掩码 Key 拦截、桌面无横向滚动。
