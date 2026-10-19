# CHANGES

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
