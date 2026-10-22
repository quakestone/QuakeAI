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

### 已知限制（后续批次处理）

- 服务商/模型管理页、用量对账、一键导入内置通道 → B2。
- 语音 TTS/STT 递降、工作流参数 → B3；上游/接入/鉴权/本机调试 → B4。
- 旧 UI（`src/admin_ui.html`）仍在 main/gh-pages 线上运行，本分支验收后（B7）才删除——当前两套并存属计划内的中间态。

## 复现命令

```bash
bash scripts/build.sh          # 构建
bash scripts/test-ui-g0.sh     # G0 一键实测（构建+托管+mock+Playwright 28 断言）
bash scripts/test-ui-g1.sh     # G1 一键实测（B1 对话全量 56 断言）
```
