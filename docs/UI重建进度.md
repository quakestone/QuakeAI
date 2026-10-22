# UI 全量重建 · 施工进度

> 依据《前端 WASM 项目 UI 全量重建施工宪法》与 `docs/UI全量重建-升级与测试计划.md`。
> 开发分支：`feat/ui-rebuild`（线上 main / gh-pages 不动，过门禁后合入）。

## B0 脚手架 + 适配层 + 首屏真实对话闭环 ✅（门禁 G0 通过）

### 交付物

| 项 | 文件 | 说明 |
|---|---|---|
| 新 UI 壳 | `src/ui/index.html` | QuakeAI 品牌化外壳（sidebar + topbar + view-host + modal + toast + 语音浮层），仅相对路径 |
| 设计令牌 | `src/ui/assets/css/tokens.css` | Kit 令牌全量保留 + `--accent` 品牌化 + 管理面扩展令牌集中定义（健康三态色等） |
| 组件样式 | `src/ui/assets/css/app.css` | Clarity Kit 原样采用（零改动，保证可升级） |
| 业务扩展 | `src/ui/assets/css/gw.css` | 健康徽标 / 网关状态卡 / 模型选择器 / 流式消息 / 代码块 / `[hidden]` 修正 |
| 持久化 | `src/ui/assets/js/store.js` | localStorage `gw_*` + IndexedDB `gw_convos`（键名/库名/表结构与旧 UI 完全一致）+ `gw_convos_v1` 一次性迁移 |
| **WASM 边界** | `src/ui/assets/js/adapters.js` | `GatewayAdapter` 单例：`init()` 幂等装载、`health/admin/chat/channels/speech/transcribe/diag/events/usage`；SSE 解析、percent 解码、Abort、错误语义全部在此层消化；UI 不直接碰 `gw.*` |
| 组件 | `src/ui/assets/js/components.js` | escapeHTML / md（与旧 UI 同口径）/ message / streamMessage（增量句柄）/ toast / healthBadge |
| 应用 | `src/ui/assets/js/app.js` | 首页（真实网关状态）+ 对话（一问一答/停止/重生成/导出/模型选择）+ 语音助手浮层 + 主题 + 快捷键 |
| 构建 | `scripts/build.sh` | wasm 胶水输出 `build/assets/wasm/`，UI 壳组装 `build/`，版本号注入 |
| SW | `src/sw.js` | 预缓存改新路径（CACHE `quakeai-gw-v2`） |
| 测试 | `scripts/g0-verify.py` + `scripts/test-ui-g0.sh` | 一键：构建 → 子路径托管 → mock 上游 → Playwright 实测（28 断言） |

### 门禁 G0 实测结果（28/28 PASS）

- 子路径托管（`/QuakeAI/`）全资源 200；WASM 装载就绪（适配层单例，防竞态）。
- 真实数据：`health()` 特性清单、`channels()` 建渠道/掩码回显、可用模型口径（与旧 UI 一致）。
- **真实对话闭环**：UI 发送 → `gw.chat` → WASM → mock 上游 SSE → 流式增量渲染 → 首字耗时/渠道元信息 → IDB 落库（含渠道名、流式标记清除）。
- 旧数据无损：`gw_convos_v1` 迁移（键清除+标记）、`gw_theme` 继承、IDB 旧会话直读、中断流式消息标记「（生成中断）」。
- 控制台/页面零错误；流式停止按钮显隐正常。

### 过程中发现并修复的缺陷

1. **代理转发从未接通**（`src/lib.rs`）：WASM 读 `GW_UPSTREAM_PROXY`，UI/文档契约是 `gw_upstream_proxy`——用户在设置页填的代理从不生效。已按契约改为小写键（F25 的前置修复，B4 将补 UI 与测试）。
2. **`[hidden]` 被 Kit CSS 覆盖**：`.soft-btn{display:inline-flex}` 使 UA 的 `[hidden]{display:none}` 失效，流式停止按钮切不回来。gw.css 加 `[hidden]{display:none!important}`。
3. 测试基建：mock 上游加 CORS 响应头（浏览器 e2e 直连前置）；`pkill -f` 自匹配自杀防护（括号技巧）。

## B1 对话全量（F1–F7）✅（门禁 G1 通过 56/56）

### 交付物

| 项 | 文件 | 说明 |
|---|---|---|
| 积分账本 | `src/ui/assets/js/points.js` | `gw_model_points_v1`：sel +1 / use +1 / share +10 / degrade −20，key=模型名、`__auto__` 自动条目；`applyRoutePoints(meta, requestedModel)` 收适配层已解码 meta（不碰 Response） |
| 魔法指令 | `src/ui/assets/js/magic.js` | `/debug on\|off`、`/console: zsj\|off`（全角 `／` 兼容）；dbg 500 条环形日志 + 页面内浮层（iOS PWA 无 F12）；控制台面板经 `setStateProvider` 注入数据源，magic 不反向依赖 app |
| 附件管线 | `src/ui/assets/js/attachments.js` | LIMITS（text 1MB / skill 100KB / media 3MB / file 20MB）、KIND_CAPS（图5/音3/视1/文5）；图片 dataUrl 直连 vision parts、视频抽帧 poster、音频 Blob 持久化；气泡渲染缩略图/播放器/file pill/desc 理解行 |
| Skills 三流 | `src/ui/assets/js/skills.js` | 系统级（tier=system，**双通道**：system 消息 + x-gw-features）→ 默认流（≤3，`gw_skip_default` 暂停）→ 手选；去重保序，>100KB 单条跳过；自持字节计量，无跨模块加载序耦合 |
| 分享 | `src/ui/assets/js/share.js` + `qrcode.js` | deflate-raw → base64url 进 URL hash（服务端零存储），`/s#` 落地页；平台域（github.io 等）回落同源；二维码 canvas → data:image/gif |
| 消息组件 | `components.js` | divider 角色（`.ctx-div`）、附件气泡、assistant share / user edit+retry 操作组；**始终输出 `.message-text` 容器**（流式句柄依赖） |
| 应用接线 | `app.js` | `sendWith`：ctxStart 历史切片 → buildApiContent → 三流注入 → `Skills.api.use` → `gateway.chat(features)` → `applyRoutePoints` → usage 记账；修改流截断重发、重试/重新生成、模型选择（积分倒序）、分享落级链、导出 .md；**流式单节点原则**（live 节点参与重渲染，杜绝双气泡） |
| 样式 | `gw.css` | 附件条/气泡、skills 三流条、ctx divider、edit-chip、dialog、积分徽标、`.message.streaming .message-actions{visibility:hidden}`、760px 断点 |
| SW | `src/sw.js` | 预缓存补 points/magic/skills/attachments/share/qrcode 六模块 |

### 门禁 G1 实测结果（56/56 PASS）

- **A 基础闭环**：WASM 就绪 → 建渠道 → 首轮真实流式 → mock 上游收到 → `__auto__.use +1`。
- **B skills 三流**：系统级「深度思考」开 → system 消息注入提示词（全渠道保底通道）+ 智谱系渠道请求体注入 `thinking:{type:enabled}`（x-gw-features 网关内部消费、按渠道能力落地，E2E 验效果）。
- **C 附件**：图片入列（chip/计数）→ 请求体末条 `[text,image_url]` parts → 气泡缩略图 → 计数条清空。
- **D 上下文边界 + sel 积分**：切模型 → divider（含模型名）→ `ctxStart` 落库 → `sel +1`。
- **E ctxStart 切片**：边界后请求只带 1 条用户消息、不含边界前提问 → `glm-4-flash use +1`。
- **F 修改流**：回填 → 编辑 → 截断重发，IDB 无重复提问、新提问落库、修改态撤销。
- **G 重试/重新生成**：assistant 重新生成 = 回退对应提问截断重发，消息总数不变、内容落库。
- **H 魔法指令**：全角 `／debug on` / `/console: zsj` 生效（顶栏按钮+对话框+状态 JSON），dbg 写入页面内浮层，off 后清理。
- **I/J 导出+分享**：.md 下载；分享对话框（无 navigator.share 落级）→ `/s#` 压缩链接 → 二维码 data:image/gif → `share +10`。
- **K ctxreset 开关**：默认开、可关可开。
- **G0 回归**：旧 IDB 会话直读、中断流式消息标记「（生成中断）」；A–K 控制台/页面零错误。

### 过程中发现并修复的缺陷

1. **流式/静态双气泡**：`renderMessages()` 曾同时产出静态占位与 stream 节点（同消息两个 DOM），`.last` 命中流式节点导致重新生成按钮找不到。改为**单节点原则**：`state.stream` 持 live 节点，renderMessages 对流式消息只挂 live 节点（append 移动已连接节点，支持流式中重渲染/切会话重挂），`streamMessage({retry:true})` 补终态按钮，CSS 流式中隐身。
2. **skills 加载序耦合**：skills.js 先于 attachments.js 加载时顶部 `const Att = global.QuakeAttachments` 为 undefined，开系统级 skill 即崩。改为自持 `SKILL_MAX/skillByteLen`（双保险：调整 script 顺序 + 去跨模块依赖）。
3. **测试竞态误判**：`send_and_wait` 曾在流式文本首现即返回，而 `applyRoutePoints` 在流结束后才跑 → 积分断言假失败。用 `window.__probe` 打点证明代码路径正确后，给测试补 `#chatStop` 隐藏等待（流真正结束），未改产品代码。
4. **mock 中文乱码**：mock 以 latin1 累积 body，digest 直接 `JSON.parse` 致中文 mojibake。digest 解析改 `Buffer.from(body,'latin1').toString('utf8')`（bodyLength 仍按 latin1 二进制安全）。
5. **x-gw-features 口径**：该头由网关内部消费、不转发上游；效果按渠道能力落地（智谱系注入 thinking/web_search，其他渠道 system 提示词保底）。G1 段 B 断言改验**效果**（mock 渠道注册为 `mock-zhipu`，`is_zhipu_like` 命中注入路径；`bearer_token` 仅 provider 精确 `zhipu` 走 JWT，raw key 原样透传不受影响）。

## B2 服务商与模型管理（F8–F18）✅（门禁 G2 通过 45/45）

### 交付物

| 项 | 文件 | 说明 |
|---|---|---|
| 服务商模块 | `src/ui/assets/js/providers.js` | `QuakeProviders`：渠道 CRUD、Key 轮换、启停/优先级、连通测试、模型元数据行编辑、预设预填、seed/导入/导出、批量体检、用量三表——旧 UI（admin_ui.html 模型服务商标签）行为对齐，表现层换 Clarity Kit |
| 视图 | `src/ui/index.html` | 导航「服务商」+ `#providersView`（统计条/渠道卡/预设网格/用量区）+ 三对话框（渠道表单 / 换 Key / 批量体检） |
| 适配层扩展 | `adapters.js` | `listChannels`（含 summary+presets）、`seed`、`testModel`、`adminRaw`、`usageExportCsv`（CSV 原文，不经 JSON 化） |
| 样式 | `gw.css` | 渠道卡/健康点/档位/能力徽标/预设卡/用量卡与三表/模型行编辑器/表单对话框/窄屏塌陷 |
| 壳接线 | `app.js` | VIEW_NAMES + setView 进入服务商视图即 `Providers.load()`；`Providers.init(refreshGateway)` 渠道变更联动模型/顶栏 |
| 导入数据源 | `build.sh` | 仓库根 `gateway-channels.json` 复制进 `build/`（页面相对路径拉取，子路径托管可用） |
| SW | `src/sw.js` | 预缓存补 `providers.js` + `gateway-channels.json` |
| 测试 | `scripts/g2-verify.py` + `scripts/test-ui-g2.sh` | 一键：构建 → 托管 → mock → Playwright（45 断言） |

### 门禁 G2 实测结果（45/45 PASS）

- **A CRUD**：表单建渠道（provider/名称/Base/类型/档位/优先级/Key/模型行/备注）→ 列表卡片 → models 落库。
- **B Key 轮换**：掩码回显（前3后4）；`5bc8...4bRY` 掩码被前端拦截（对话框不关+错误提示）；真实轮换掩码更新；轮换回有效 Key。
- **C 启停/优先级**：停用卡片置灰 + 徽标「停用」；优先级 ▲▼ ±10 即时生效。
- **D 连通测试**：探测写回 `healthy`（mock 200），卡片徽标「健康」。
- **E 模型元数据**：编辑弹窗回填；加第二行模型 → `models_meta` 2 行落库。
- **F 预设**：筛选 → 「添加」预填表单（provider/模型行）→ 取消关闭。
- **G seed**：一键载入默认渠道（数量增加），重试幂等不重复。
- **H 一键导入**：内置 7 通道 id 与 seed 同命名空间 → 首导全跳过；UI 删一个 → 再导恰好补回（数量 +1）；重导再全跳过。
- **I 导出**：`gateway-channels.json` 下载，结构 `{exported_at, note, channels}`，无明文 Key。
- **J 批量体检**：串行探测，产出「可用 x/y」汇总，mock 渠道模型 ✅。
- **K 用量对账**：真实对话产生 usage → 6 张统计卡 → 三表（按源/按源×模型/按日）→ CSV 列名与旧 UI 一致（带 BOM）。
- **L 删除 + 零错误**：删除后卡片消失；A–L 控制台/页面零错误。回归：G0 28/28、G1 56/56 不回退。

### 过程中发现并修复的问题

1. **G2 段 D 超时（测试数据问题，非产品缺陷）**：连通测试探测带轮换后的 Key，mock 只认 `test-key-local` → 401 → 健康落 `degraded`（设计口径正确）。测试改为轮换后验完**轮换回有效 Key** 再探测。
2. **G2 段 H 断言口径错误**：原断言假设「导入 = 纯新增」，但 seed 默认渠道与内置文件同 id 命名空间（`seed-{provider}-{kind}`），首导本应全跳过。重构为「跳过路径 + 删后再导补回」双路径验证。
3. **后端口径确认（无需改动）**：`api_update` 对缺失/空/掩码 `api_key` 一律视为不修改，仅新明文才轮换——providers.js 编辑态不发 api_key 字段，与后端口径一致。

### 已知限制（后续批次处理）

- 语音 TTS/STT 递降、工作流参数 → B3；上游/接入/鉴权/本机调试 → B4。
- 旧 UI（`src/admin_ui.html`）仍在 main/gh-pages 线上运行，本分支验收后（B7）才删除——当前两套并存属计划内的中间态。

## B3 提示词 + 语音多媒体 + 工作流（F20–F24）✅（门禁 G3 通过 53/53）

### 交付物

| 项 | 文件 | 说明 |
|---|---|---|
| 提示词模块 | `src/ui/assets/js/prompts.js` | `QuakePrompts`：F20 全量——统计条（条数/已启用/默认 n/3/累计使用/系统级）、四分类 pill 栏、卡片行（标题/分类/停用徽标/已用 N 次/简介/正文/操作组）、每类独立分页（5 条/页）、新建编辑对话框（标题+正文必填、简介 ≤30 截断）、启停/设默认（≤3，联动对话页 chip）/删除（联动清理默认 id） |
| 语音多媒体模块 | `src/ui/assets/js/voice.js` | `QuakeVoice`：F21 TTS 三级递降链（网关 `/v1/audio/speech` → 自定义端点 → `speechSynthesis`，逐层输出+只弹一次降级提示）；F22 STT 两级链（`SpeechRecognition` → 网关 `/v1/audio/transcriptions`，MediaRecorder→16kHz 单声道 WAV 重编码）；F23 视觉/视频模型范围（`mmScope` attach/always，`mmModelFor` 选路）；MicHub 麦克风单所有者+强制释放、VoiceInput 单消费者 |
| 工作流模块 | `src/ui/assets/js/workflow.js` | `QuakeWorkflow`：F24 四开关全接真实行为——usage（消息 meta token 徽标）、title（LLM 总结标题，answers≥2 且非用户命名）、memory（token 超阈 LLM 压缩，`memfsm` 状态字段兼容旧数据）、format（CSS 变量 `--att-thumb/--att-video-w/--att-video-h` 实时下发）；阈值/规格数值 clamp 持久化 `gw_wf_cfg` |
| 视图 | `src/ui/index.html` | 导航加 prompts/voice/workflow；`#promptsView`（统计/分类栏/卡片/分页）、`#voiceView`（链路状态卡+音频/图片视频/TTS/STT 设置卡+测试按钮）、`#workflowView`（四开关卡+阈值+规格+模型+记忆卡）、`#skill-dlg` 提示词对话框 |
| 样式 | `gw.css` | sk-catbar/sk-row 卡片族、vc-grid/vc-card/vc-chain 链路图、wf-card/wf-toggle/wf-nums、对话框 textarea、760px 响应式；附件规格改 CSS 变量驱动（视频容器 ≥300px 放大） |
| 壳接线 | `app.js` | 三视图 setView 装配；`sendWith` 视觉模型选路（`Voice.mmModelFor`）+ token 账本后挂 `Workflow.runWorkflow`；Jarvis 浮层重写（文字入口+回答播报）；消息「播音」按钮走 TTS 递降链；`window.QuakeApp` 导出 `refreshGateway`（渠道账本外部变更后手动刷新模型清单） |
| 消息动作 | `components.js` | 助手消息动作组加「播音」 |
| SW | `src/sw.js` | 预缓存补 prompts.js / voice.js / workflow.js |
| 测试 | `scripts/g3-verify.py` + `scripts/test-ui-g3.sh` | 一键：构建 → 子路径托管 → mock → Playwright（53 断言，A–F 六段） |

### 门禁 G3 实测结果（53/53 PASS）

- **A 提示词 CRUD（F20，21 断言）**：对话框新建（存储端生成 id）→ 编辑回填改名 → 停用/启用徽标 → 设默认（徽标 1/3 + 对话页 chip 联动）→ 分类切换 + 每类独立分页（造 7 条翻页验证）→ 空分类空态 → 手选发送后 `used_count +1`（服务端计数）→ 删除卡片消失 + 默认徽标回 0/3 + chip 同步移除。
- **B TTS 三级递降链（F21，11 断言）**：链路状态卡文案；auto 第一级测试播报真实命中网关（mock 收到 `/audio/speech`、输入非空）；回答完成「播音」按钮触发合成；browser 模式持久化 + 链路文案切换；自定义端点（第二级）指向 mock 真实命中；三级全部走通。
- **C STT 两级链（F22，7 断言）**：headless 下浏览器识别不可用（链路如实标注）；gateway 模式持久化；fake mic 录 3s → WAV 重编码 → mock 收到 multipart 并返回 `mock-transcription`；Jarvis 浮层文字入口 → 回答 → 按 TTS 链播报 + 浮层显示文本。
- **D 视觉/视频模型范围（F23，6 断言）**：vision 下拉来自渠道账本；带图附件选路覆盖（`glm-4v-flash`）；`always` 范围 + 自动选路纯文本也走视觉模型；显式选模型优先于 always；`mmScope`/`visionModel` 持久化。
- **E 工作流参数（F24，7 断言）**：CSS 变量实时生效（thumb/vw/vh）；`gw_wf_cfg` 数值持久化；auto_title 真实跑（title_auto=true 且标题被 LLM 改写）；memory 压缩真实触发并产出长期记忆（超阈 token 驱动）；usage 关闭后无 token 徽标。
- **F 零错误**：A–F 控制台/页面零错误。**回归：G0 28/28、G1 56/56、G2 45/45 不回退。**

### 过程中发现并修复的问题

1. **skills 并发种子重复条目（真 bug）**：app.js/prompts.js/setView 并发调用 `Skills.load()`，双双见空列表 → 双双 POST 种子 → 存储端对已存在 id 加后缀产出 `sk-three-sages-2`。修法：`skills.js` `load()` 加 in-flight 去重（与 providers.js 同模式），修复后恰 3 条无重复。
2. **分页条永不显示（产品缺陷）**：`#prPager`/`#usagePager` 在 index.html 写死 `class="usage-pager hidden"`，JS 只切 `hidden` 属性，`.hidden` 类（display:none）使其永不可见。修法：改用 `hidden` 属性驱动（gw.css 已有 `[hidden]{display:none!important}`）；providers 视图同款潜在问题一并修复。
3. **冗余元素遮蔽选择器**：index.html 遗留空 `#prDefBadge` div，使 `#prDefBadge, .sk-def-badge` 选择器取到空元素。删除该冗余 div（徽标由 renderRows 在 `#prRows` 内产出）。
4. **G1 回归被 B3 工作流污染（测试口径）**：B3 引入的 autoTitle/memory 会在主聊天完成后补发 LLM 请求，落在 digest 末条，G1 原有「取末条 digest」断言全部误命中后续请求。修法：g1-verify.py 加 `main_digest()` 定位主请求（user 消息直接是提问、非「用户：」开头的转录）。
5. **G3 测试口径四处修正（非产品缺陷）**：① `used_count` 是服务端计数，客户端 `state.list` 不自增——发送后需重拉账本再读；② `glm-4-voice` 入库即被 `load_all_channels` 迁移为 `glm-asr-2512`（store.rs 设计行为），断言改验账本实际值；③ memoryUpdate 需消息数 > `memKeepRecent` 才有可折叠区域，E2 测试对话加厚到 10 条；④ 测试经管理面 API 建渠道绕过 UI 的 onChange 联动，需手动触发 `refreshGateway`（据此把该函数导出到 `window.QuakeApp`）。
6. **E2 分支辨析**：消息数 ≤ keep_recent 时 memoryUpdate 走「无新增可折叠内容」分支（计数减半不压缩）——这是防死循环的设计分支，测试数据需覆盖压缩主路径。

### 已知限制（后续批次处理）

- 旧架构 WASM 工作流内核（`/wf/kernel.wasm`，CF Worker 托管）在纯静态部署下不存在——FSM「何时压缩」决策以同等阈值的 JS 逻辑实现（`workflow.js` 头注已诚实说明），`memfsm` 状态字段与旧数据兼容。
- headless Chromium 无语音引擎：浏览器 TTS 实际播放与 `SpeechRecognition` 不可用，链路状态如实标注；网关级递降（自定义端点/mock）已真实命中验证。
- 上游/接入/鉴权/本机调试 → B4；PWA/性能/三轮审查/删旧 UI/部署 → B5–B8。
- 旧 UI（`src/admin_ui.html`）仍在 main/gh-pages 线上运行，本分支验收后（B7）才删除。

## B4 上游/接入/鉴权/本机调试（F25–F29 + F19）✅（门禁 G4 通过 49/49）

### 交付物

| 项 | 文件 | 说明 |
|---|---|---|
| 上游接入模块 | `src/ui/assets/js/upstream.js` | `QuakeUpstream`：F25 代理地址填写/校验（http(s) 前缀）/状态展示/`hintIfProxyMissing` 发送失败引导；F27 管理口令（`gw_admin_token`，掩码回显）；F19 本设备 Key 行（provider 来自渠道账本，掩码回显+掩码拦截，留空保存=清除）；F28 部署自检（`GET /diag` JSON 渲染） |
| 网络监视模块 | `src/ui/assets/js/netmon.js` | `QuakeNetMon`：F29 四态（ok ≤300ms / slow ≤800ms / bad >800ms / offline）固定底部胶囊条；插件链 trace（colo/loc/协议解析）→ health 计时兜底；首屏 3s boot + 60s 心跳（rIC 空闲调度两段式）+ online/offline 事件；1s 探测硬超时；`gw_net_last` 持久化缓存展示；bad/offline 停心跳；点击手动重探 |
| 视图 | `src/ui/index.html` | 导航加「上游与接入」；`#upstreamView`（自检按钮+代理/鉴权两卡+接入 curl 文档卡+设备 Key 行+diag JSON）；全局 `#net-bar`（四态 LED + 文本） |
| 样式 | `gw.css` | up-grid/up-card/up-row/up-hint/up-state/up-code/up-diag、dk-rows/dk-row 五列网格、`.net-bar` 固定底部胶囊（四态 LED：ok/slow/bad/offline）、760px 响应式 |
| 壳接线 | `app.js` | `VIEW_NAMES` 加 upstream；setView 装配 `Upstream.load()`；init 装配 `NetMon.start()`；首页网关能力卡三态灯（代理/设备 Key/口令）；`sendWith` catch 非 Abort 分支接 `hintIfProxyMissing`；Providers 回调合并防覆盖 |
| 适配层修复 | `adapters.js` | **x-device-keys 头格式修正**（见问题 1）：存储态扁平 `{provider:key}` 发送前包装为网关期望的 `{id,keys}`（`gw_device_id` 持久化随机 UUID） |
| SW | `src/sw.js` | 预缓存补 upstream.js / netmon.js |
| mock | `scripts/mock-zhipu.mjs` | `/proxy` 路由复用 chat 处理器（模拟用户自建转发服务）；日志记 `gwTarget`/`proxyReceived`；`/proxy` 请求同样摘要素引；CORS 放行四类自定义头 |
| 测试 | `scripts/g4-verify.py` + `scripts/test-ui-g4.sh` | 一键：构建 → 子路径托管 → mock → Playwright（49 断言，A–G 七段） |

### 门禁 G4 实测结果（49/49 PASS）

- **A 上游转发代理（F25，12 断言）**：代理地址保存 → `gw_upstream_proxy` 持久化 + 「已配置」状态 + x-gw-target 约定提示 + 首页能力卡联动；配代理后对话经 mock `/proxy` 真实转发、`x-gw-target` = 真实上游地址（`http://127.0.0.1:8788/chat/completions`）、请求体带对模型与提问、回答含 ANSWER_MARK（**海外渠道经代理可用**）；清空恢复直连态 + `hintIfProxyMissing` 引导返回 true。
- **B 接入方式（F26，5 断言）**：curl 示例含 `/v1/chat/completions` 与 `/v1/audio/transcriptions`；观测头 `x-gateway-channel`/`x-gateway-attempts` 与四类自定义头（x-gw-target/x-gw-features/x-device-keys/x-admin-token）约定齐全。
- **C 管理面鉴权软锁（F27，8 断言）**：口令持久化 + 掩码展示；**裸 WASM 实例端到端**——缺 `x-admin-token` → 401、错误口令 → 401、正确口令 → 200（软锁生效且放行）；适配层自动带头不影响 UI 侧管理请求；清空后无头请求恢复 200（软锁释放）。
- **D 本设备 Key（F19，11 断言）**：provider 行来自渠道账本；录入 → `gw_device_keys` JSON 持久化 + 「已登记」状态；掩码拦截（不写入 + toast）；**端到端**——无共享 Key 的音频渠道 + 本设备 Key → STT 转写成功且 mock 上游收到 `Bearer <设备 Key>`（证明 `x-device-keys` 被 WASM 真实消费）；清除后 JSON 与行状态同步回落。
- **E 部署自检（F28，3 断言）**：`/diag` JSON 渲染（version/store/expected_binding/env 四段齐全）；store 态 ok 且渠道数 ≥2；env 如实反映未配管理口令。
- **F 网络监视四态（F29，7 断言）**：net-bar 固定底部常驻；route 拦截 `/cdn-cgi/trace` → ok 态（延迟 ms + 协议 + 国家中文名）+ 手动探测结论 toast + `gw_net_last` 持久化；offline 事件 → offline 态 + 缓存快照展示（含国家）；online 事件恢复 ok。
- **G 零错误（3 断言）**：boot 探测链式兜底真实发生（trace 404 → health 兜底，设计内行为）；无非探测路径资源 404；控制台/页面零错误。**回归：G0 28/28、G1 56/56、G2 45/45、G3 53/53 不回退。**

### 过程中发现并修复的问题

1. **x-device-keys 头格式不匹配（真 bug，端到端失效）**：adapters.js 最初把 localStorage 里的扁平 `{provider: key}` 原样作头发给网关，而网关侧 `DeviceKeys` 结构体期望 `{id, keys}` 包装——Rust 解析出空 keys 被静默忽略，设备 Key 端到端从不生效。修法：移植旧 UI 的 `deviceKeysHeader()`（过滤空值 + `gw_device_id` 持久化随机 UUID 包装）；G4-D 端到端断言（mock 上游实收 `Bearer <设备 Key>`）即为该修复的守门测试。
2. **net-bar 点击重探缺失（产品缺陷）**：netmon.js 标题承诺「点击重新检测网络」但 start() 未绑定点击处理。补上 `probe("manual")`（手动探测才给结论 toast，与设计注释一致）。
3. **netmon 探测 404 污染零错误断言（测试口径）**：静态托管下 `/cdn-cgi/trace` 与 `/health` 必然 404——这是插件①→② 链式兜底的设计内行为（探测失败静默降级），但 Chromium 会记 console error，G1 首跑因此 55/56。修法：四个门禁统一按 `m.location.url` 精确过滤探测路径 404；G4 另加双轨 404 记账（探测路径 404 必须发生=兜底链真实跑了；其他任何 404 都算缺陷），过滤不放宽为白名单。

### 已知限制（后续批次处理）

- 网关侧聊天渠道仍须自带共享 Key 才进候选池（`load_channels` 先过滤后应用设备 Key）——设备 Key 的端到端生效面目前是音频渠道；这是既有 Rust 行为，B4 只修 UI 适配层。
- 探测延迟取页源同源（trace/health 都打当前站点），不含上游 API 真实延迟；四态反映的是「到本站边缘/主机的可达性」。
- PWA 壳体固化/性能/三轮审查/删旧 UI/部署交付 → B5–B8。

## 复现命令

```bash
bash scripts/build.sh          # 构建
bash scripts/test-ui-g0.sh     # G0 一键实测（脚手架 28 断言）
bash scripts/test-ui-g1.sh     # G1 一键实测（B1 对话全量 56 断言）
bash scripts/test-ui-g2.sh     # G2 一键实测（B2 服务商与模型 45 断言）
bash scripts/test-ui-g3.sh     # G3 一键实测（B3 提示词+语音多媒体+工作流 53 断言）
bash scripts/test-ui-g4.sh     # G4 一键实测（B4 上游/接入/鉴权/本机调试/网络监视 49 断言）
```
