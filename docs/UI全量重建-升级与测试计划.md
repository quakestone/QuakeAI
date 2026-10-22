# QuakeAI 网关 · UI 全量重建：升级计划与测试计划

> 依据《前端 WASM 项目 UI 全量重建施工宪法》编制。设计基座采用上传的 **Clarity AI Chat UI Kit**（零构建、移动端优先、`tokens.css` 设计令牌 + `adapters.js` 后端边界）。
> 本文档为**计划与测试方案**，尚未改动任何业务代码、WASM、构建或仓库提交。待确认后进入施工。

---

## 0. 最高原则（不可违背）

- **旧 UI 视觉与实现彻底归零**：不保留/复用任何旧布局、导航、组件、图标、配色、CSS、动画、设计语言；禁止"换色/换图标/改圆角"式伪装。
- **业务能力完整继承**：WASM 引擎、Rust 算法、数据契约、持久化格式、事件协议一律不破坏。
- **旧 UI 仅作审计材料**（识别功能/事件/字段/接口），不作为新 UI 的视觉或组件模板。
- **新 UI 不接假数据**：所有入口必须接真实业务接口，禁止占位按钮/模拟结果。
- 旧代码已备份（`backup/0.19.34-20261022` 分支 + `v0.19.34-backup` tag + `backup/gh-pages-*`），回退走版本控制，不把旧 UI 留在生产包。

---

## 1. 第一阶段：审计产物（施工前的基线）

### 1.1 能力地图（三层现状）

| 层 | 现状 | 处置 |
|---|---|---|
| 业务能力层 | `wasm32` 产物 `build/index_bg.wasm`，wasm-bindgen 0.2.129，导出 `chat / admin / audio_speech / audio_transcriptions / health`（均收 `Request`、返 `Response`） | **原样保留**，算法/签名/返回结构不动 |
| 集成缝（耦合在旧 UI 内） | `admin_ui.html` 内：`import init from './build/index.js'`、`window.fetch` 拦截（`/admin` `/v1` `/health`）、`window.__gwDispatch` 路由到 `gw.*`、`api()` 助手 | **抽离为独立 UI 适配层**（新 `adapters.js`） |
| 旧表现层 | 7855 行单文件 `admin_ui.html`（视觉 CSS + 组件树 + 状态 + 事件全耦合） | **验收后彻底删除** |

### 1.2 业务功能清单（迁移基线，逐项验收）

| # | 功能 | 入口 | 处理（WASM/存储） | 输出 | 状态/降级 | 依赖 |
|---|---|---|---|---|---|---|
| F1 | 对话发送（流式 SSE） | 输入框/Enter | `gw.chat(new Request('/v1/chat/completions'))` | 流式增量文本 | 加载/停止(Abort)/失败 | 渠道+Key、上游代理 |
| F2 | 停止 / 重新生成 | 消息操作 | AbortController / 重发上一条 | — | — | F1 |
| F3 | 模型选择 | 选择器 | 读 `usableChatChannels` + `effModels` | 可用模型清单 | 空态提示 | 渠道 |
| F4 | 附件（图/音/视/文） | ＋ 附件 | 读文件→随消息上传；IndexedDB 持久化 | 发送+缩略图 | 超限拦截(5/3/1/5, 媒体3MB) | IndexedDB |
| F5 | Skills（系统+自定义+默认流） | chips/管理页 | `gw.admin /skills*`、`gw_skip_default` | 注入/展示 | 默认流开关 | skills API |
| F6 | 会话管理 | 侧栏 | IndexedDB `convos` | 列表/搜索/切换/导出 MD/删除 | 空态 | IndexedDB |
| F7 | 魔法指令 `/console` `/debug` | 输入框 | 本地开关 `gw_console`/`gw_debug` | 面板/浮层 | — | localStorage |
| F8 | 服务商 CRUD | 服务商页 | `gw.admin /channels` POST/PUT/DELETE | 列表 | 加载/错误 | channels API |
| F9 | Key 轮换 | 渠道卡 | PUT `/channels/{id}` `api_key` | 掩码回显 | 掩码拦截 | F8 |
| F10 | 启停 / 优先级 | 渠道卡 | PUT `enabled`/`priority` | 即时 | — | F8 |
| F11 | 连通测试 | 渠道卡 | POST `/channels/{id}/test` | 健康状态回写 | 超时/失败 | 上游 |
| F12 | 模型元数据编辑 | 编辑弹窗 | PUT `models_meta` | 结构化清单 | — | F8 |
| F13 | 预设快速添加 | 预设卡片 | `presetFill` 预填→POST | 新渠道 | — | F8 |
| F14 | 一键载入默认 | 维护 | `seed()` | 默认渠道 | — | F8 |
| F15 | 一键导入内置通道 | 维护 | fetch `./gateway-channels.json`→逐条 POST | N 个渠道（已存在 id 跳过） | 文件缺失/空 | F8 + 静态文件 |
| F16 | 导出配置 | 维护 | 导出 `{exported_at,note,channels}` | 下载 JSON | — | F8 |
| F17 | 批量体检 | 服务商页 | `selftestAll()` 串行探测 | 可用模型清单 | 逐个失败不崩 | F3/F11 |
| F18 | 用量与对账 | 服务商页 | GET `/usage` + `/usage/export` | 三张表 + CSV | 加载/空 | usage API |
| F19 | 本设备 Key | 服务商页 | localStorage `gw_device_keys`，请求头 `x-device-keys` | 本机覆盖 | — | localStorage |
| F20 | 提示词 CRUD | 提示词页 | `gw.admin /skills*` | 列表 | — | skills API |
| F21 | TTS 递降链 | 语音页 | `gw.audio_speech`→自定义端点→`speechSynthesis` | 音频/朗读 | 逐级降级 | 渠道+Key |
| F22 | STT 递降链 | 语音/Jarvis | `SpeechRecognition`→`gw.audio_transcriptions` | 转写文本 | 无权限降级 | 麦克风/渠道 |
| F23 | 视觉/视频模型范围 | 语音页 | `mmScope`/`vg-mm-scope` | 配置 | — | localStorage |
| F24 | 工作流参数 | 工作流页 | `wfCfg`（格式/缩略图/视口/记忆） | 配置 | — | localStorage |
| F25 | 上游转发代理 | 上游页 | localStorage `gw_upstream_proxy`，`fetch_channel` 填 `x-gw-target` | 转发地址 | 未配→对话 503 提示 | localStorage |
| F26 | 接入方式 | 上游页 | 文档 + curl 示例 | 只读展示 | — | — |
| F27 | 管理面鉴权 | 鉴权页 | localStorage `gw_admin_token`→`x-admin-token` 本地软锁 | 口令 | 留空=不鉴权 | localStorage |
| F28 | 部署自检 | 维护 | GET `/diag` | JSON | 失败提示 | diag API |
| F29 | 网络监视 | 全局 | 心跳 + online/offline | 状态条 | 探测失败静默 | 网络 |
| F30 | PWA / 离线 | 全局 | `sw.js` 预缓存壳 + manifest | 离线开壳 | 注册失败静默 | SW/manifest |

> 魔法指令、控制台、调试浮层、网络监视、主题切换属**横切能力**，需在新 UI 保留并落到真实状态。

### 1.3 不可破坏边界（铁律对象）

1. WASM 二进制与 5 个导出函数的签名/参数/返回结构。
2. 持久化格式：localStorage `gw_*` 系列键 + IndexedDB 会话/附件库（**新 UI 必须无缝读取旧数据**）。
3. 事件/网络协议：`/admin` `/v1` `/health` 路由约定、`x-admin-token`/`x-device-keys`/`x-gw-target`/`x-gw-features` 头、SSE 流式格式。
4. 安全语义：掩码 Key 拦截、admin 软锁、上游代理转发语义。
5. 错误语义：不吞错、不伪造成功、`json_error` 结构透传。

### 1.4 需先行抽离的耦合点

- `window.fetch` 拦截 + `__gwDispatch` + `api()`：从 HTML 内联脚本抽到适配层。
- 附件大小/数量上限常量 `LIMITS`/`KIND_CAPS`：随适配层/配置模块迁移，值不变。
- IndexedDB 开库/迁移逻辑：抽为独立 `store` 模块，供新 UI 复用。

---

## 2. 第二阶段：新前端架构（三层）

```
┌─ C 表现层（全新，基于 Kit）────────────────────────┐
│ index.html · tokens.css · app.css · components.js  │
│ app.js(状态/视图) · 各业务页(真实接入)              │
│   只调用适配层，禁止直接碰 gw.* / localStorage 细节 │
├─ B UI 适配层（全新，唯一 WASM 边界）───────────────┤
│ adapters.js → GatewayAdapter                       │
│   · init() 一次装载 WASM，集中持有 gw 实例          │
│   · 统一 Request/Response 构建与解析               │
│   · 异步状态映射 / 取消(Abort) / 错误→可读提示       │
│   · store.js：localStorage gw_* + IndexedDB（键不变）│
├─ A 业务能力层（原样保留）──────────────────────────┤
│ build/index_bg.wasm + wasm-bindgen index.js         │
└────────────────────────────────────────────────────┘
```

### 2.1 GatewayAdapter 接口草案（唯一 WASM 入口）

- 生命周期：`await GatewayAdapter.init()`（幂等，防 WASM 初始化竞态）。
- 对话：`chat({messages, model, features, signal})` → 解析 `gw.chat` 的 SSE `Response`，向上吐增量；取消即 Abort。
- 渠道：`channels.list/create/update/remove/test/setKey`。
- 技能：`skills.list/create/update/remove/use`。
- 语音：`speech(payload)`、`transcribe(formData)`（浏览器 STT 走 Web Speech，不经 WASM）。
- 运维：`usage()`、`usageExportUrl()`、`diag()`、`selftest(models)`、`events()`、`health()`。
- 配置：`config.get/set(key)`（`gw_upstream_proxy`/`gw_admin_token`/`gw_device_keys`…，键名不变）。
- 契约：**所有** WASM 差异在此层消化，UI 不散落调用 `gw.*`，不私自改协议。

---

## 3. 第三阶段：设计系统（沿用 Kit 令牌 + 品牌化）

- 直接采用 `tokens.css`：`--bg/--surface/--surface-2/--surface-3/--text/--muted/--subtle/--line/--accent/--accent-soft/--green/--shadow/--radius/--radius-sm/--sidebar-width`，浅/深双主题（`[data-theme="dark"]`），`prefers-reduced-motion` 已内置。
- **品牌化**：品牌标记由 `clarity.` 改为 **QuakeAI**（保留克制的字标 + 方形 brand-mark 手法），主题色可从 `--accent` 派生品牌色（一处改、全局生效）。
- **管理面扩展令牌**（新增但集中定义，不散落）：数据表格行高/密度、状态色（healthy/degraded/unknown 三态，禁用红绿滥用）、代码块、Key 输入、表单密度、图表色板。
- 一致性规则：同语义操作同视觉、同状态同色、同层级同排版（遵循 Kit 现有按钮/卡片/浮层体系）。

---

## 4. 第四阶段：信息架构与页面重设计（从零，不仿旧页）

新外壳沿用 Kit：`sidebar（分组导航）+ topbar（面包屑/状态/Jarvis/头像）+ view-host + modal + toast + Jarvis 语音浮层`。移动端侧栏抽屉、安全区、键盘快捷键沿用。

| 新页面/视图 | 承载的旧业务（F#） | 关键设计问题回应 |
|---|---|---|
| **首页（工作台）** | F1 快捷提问、渠道健康概览、能力状态（代理/Key 是否就绪）、F6 最近对话、F29 网络状态 | 一屏看清"能不能用+从哪继续"；真实数据 |
| **对话** | F1/F2/F3/F4/F5/F6/F7 + 调试/控制台 | 模型选择、附件、skills 三流、流式/停止/重生成、导出、魔法指令 |
| **服务商与模型** | F8–F18 | 渠道卡/表、启停/优先级/测试、模型元数据、预设、一键导入/默认/导出、批量体检、用量对账 |
| **提示词** | F20 | skills CRUD、分类/层级/启用/使用次数 |
| **语音与多媒体** | F21/F22/F23 + Jarvis | TTS/STT 递降、视觉/视频范围、语音面板 |
| **工作流** | F24 | 参数（格式/缩略图/视口/记忆） |
| **上游与接入** | F25/F26/F19 | 上游转发（脱钩 CF 必填）、OpenAI 兼容端点+curl、本设备 Key |
| **管理面鉴权** | F27 | admin 口令（本地软锁） |
| **本机与调试** | F28/F29/F7 + 主题 + 清空本地数据 | 部署自检、调试浮层、控制台、网络监视、主题、数据清理 |
| **语音助手浮层（Jarvis→改名）** | F22 | Web Speech → 网关 STT；文字入口 |
| 决策点：Kit 的"资料库"无对应旧业务 | — | 改为「本地数据管理」（会话/附件导出、清空）或移除，**不留空壳** |

> 每个页面须回答宪法 8 问；所有旧入口（F1–F30）都要有新位置；不允许为简洁删能力、不允许占位。

---

## 5. 第五阶段：组件体系（复用 Kit + 新建业务组件）

**直接复用（Kit 已有）**：`message`、`toast`、`modal`、`switch`、`card`、`field`、`nav-item/section`、`soft-btn/primary-btn/send-btn/icon-btn/text-btn`、`empty-state`、`progress-track`、`topic-pills`、`quick-card`、`plugin-card`(→能力卡)、`conversation-item`、`sidebar/topbar/view-host` 骨架、`jarvis-*`(→语音浮层)。

**需新建（业务专用，集中在新组件目录，统一命名）**：
- 数据展示：渠道表/渠道卡、健康徽标（三态）、模型选择器、用量三表、迷你图表。
- 表单：Key 输入（掩码/可见切换）、代理地址表单、范围/优先级控件、skills 编辑表单。
- 反馈：流式消息骨架、批量体检进度、连接测试结果条、代码块（curl/接入）。
- 容器：设置分组页（registry/mm/wf/api/proxy/auth 的卡片化重组）、分栏布局。
- 移动端：侧栏抽屉（已有）、底部操作、安全区适配（沿用 Kit env() 处理）。

> 规则：职责清晰、I/O 明确、状态完整（加载/空/错/成功）、可独立验证、必要无障碍；不塞巨型组件、不过度碎片化。

---

## 6. 第六阶段：WASM 兼容铁律 → 本项目落地约束

1. 不为视觉问题改 WASM；2. 不改 5 个导出签名/参数/返回；3. 不改 `gw_*`/IndexedDB 持久化格式（**旧数据必须可读**）；4. 不调异步任务顺序（体检串行等）；5. 不改 `/admin` `/v1` `/health` 路由与 4 类自定义头、SSE 格式；6. 页面卸载不杀进行中的对话/转写任务；7. 防重复订阅/重复请求/内存泄漏（适配层单例 + 卸载清理）；8. 防 WASM 初始化竞态（`init()` 幂等 + 就绪门控）；9. 不吞错、不伪造成功；10. 不用模拟数据（`DemoAdapter`/演示文案全部替换为真实 gateway 数据）；11. 不引入重型依赖拖慢 WASM 首屏（保持零构建、零运行时框架，CSS/JS 体量对标 Kit）；12. 业务数据单一数据源（适配层/store 持有，UI 只读派生，禁多源不一致）。

---

## 7. 升级施工计划（分批 + 顺序 + 里程碑门禁）

> 宪法允许功能量大时"先建完整清单再分批"，但最终须全迁移。批次间设验收门禁，未过不进下一批。

- **B0 脚手架与适配层**：Kit 外壳落地（品牌化 tokens、导航骨架、modal/toast/语音浮层）；`store.js`（复用旧 `gw_*`+IndexedDB，验证旧数据可读）；`GatewayAdapter`（init + health + chat + channels.list）跑通"真实一问一答"。**门禁 G0**：首屏加载 WASM、真实对话闭环、旧 localStorage 数据无损读取。
- **B1 对话全量**：F1–F7（流式/停止/重生成/模型选择/附件/skills 三流/会话/导出/魔法指令/调试浮层/控制台）。**门禁 G1**：宪法 Round1 对话链路全过。
- **B2 服务商与模型**：F8–F18（CRUD/Key/启停/优先级/测试/元数据/预设/导入/默认/导出/体检/用量）。**门禁 G2**：渠道全生命周期 + 内置通道一键导入真实可用。
- **B3 提示词 + 语音多媒体 + 工作流**：F20–F24 + 语音浮层。**门禁 G3**。
- **B4 上游/接入/鉴权/本机调试**：F25–F29。**门禁 G4**：配代理后海外渠道可用、鉴权软锁生效。
- **B5 PWA/性能/打磨**：F30 + SW/manifest/相对路径子路径兼容 + 深色/减少动效 + 无障碍。**门禁 G5**。
- **B6 三轮审查 + 修复 + 回归**（见 §8）。
- **B7 强制删除旧 UI**（见 §9）+ 生产构建 + 部署验证。
- **B8 交付报告**（见 §10）。

执行严格遵循宪法第十一部分顺序：审计→清单/基线→IA/视觉→组件/适配→页面接入→三轮审查/修复/回归→删旧→构建/部署→报告。

---

## 8. 测试计划

> 环境具备浏览器/无头能力（Chromium headless + Playwright 曾用于冒烟），宪法要求**实际执行**而非仅推断；确无法执行的项如实记录。

### 8.1 测试策略与环境
- 自动化：Rust 单测（`cargo test`，验证 WASM 侧未回归）；无头浏览器 E2E（Playwright）跑页面加载/WASM 引导/真实接口/子路径；静态引用分析（删旧后无死链）。
- 手动：视觉/移动端/跨浏览器（Chromium + iOS Safari standalone）/真机 PWA 安装。
- 基线：以 §1.2 F1–F30 为功能回归基线；每个 F 有触发→处理→输出→状态的断言。

### 8.2 Round1 业务完整性（每功能真实接口验证）
- 逐 F1–F30 打表：对话发/流/停/重生成；渠道增删改查、Key 轮换（含掩码拦截）、启停、优先级、连通测试（成功/超时/失败三态）、模型元数据；skills 全 CRUD + 使用计数；TTS 三级递降、STT 两级递降、视觉/视频范围；工作流参数持久化；代理填写前后对话从 503→可用；接入文档渲染；admin 软锁开/关；自检 JSON；用量表 + CSV 导出（列名对齐）；一键导入（首导 N 个、重导跳过、文件缺失报错）；批量体检串行且单点失败不崩。
- WASM 冒烟：`health`/`admin(channels/events/diag/skills)`/`chat`/`audio_*` 在无头浏览器返回真实数据。

### 8.3 Round2 视觉与交互
- 旧 UI 零残留核查（无旧 class/旧配色/旧图标/旧布局代码）。
- 全局设计语言统一（同语义同视觉、同状态同色）。
- 状态完备：每个页面的加载/空/失败/成功；流式骨架；错误可读（不裸露堆栈）。
- 移动端：无溢出/遮挡/误触；安全区；侧栏抽屉；窄屏（≤370px）可用。
- 键盘/焦点/a11y：Tab 焦点可见、Esc 关浮层、aria 标签、对比度、`prefers-reduced-motion` 降级、深色主题。

### 8.4 Round3 工程质量
- 生产构建成功；WASM 正确加载（`application/wasm` + streaming/回退）。
- 控制台零错误/未处理异常；无资源泄漏、无重复订阅/请求、无竞态。
- 刷新/重进/取消/异常恢复正常；路由/GitHub Pages 子路径/缓存(SW)正确。
- 首屏性能与包体可接受（对标 Kit 零依赖，不引入重型库）。

### 8.5 WASM 兼容回归（12 铁律逐条用例）
持久化兼容（旧 `gw_*`+IndexedDB 无损读取）、签名未变、协议/头未变、任务不误杀、单例防重订阅、init 幂等、错误不吞、无模拟数据、无重型依赖、单一数据源——每条对应一个可执行断言。

### 8.6 数据迁移兼容
- 预置旧版 localStorage/IndexedDB 夹具 → 新 UI 启动后渠道/会话/附件/偏好完整可读可续用。

### 8.7 PWA / 移动端 / 跨浏览器
- 安装（桌面+iOS 主屏 standalone）、离线开壳、SW 更新、相对路径子路径（`/QuakeAI/`）全资源 200、iOS 无 F12 时调试浮层可用。

### 8.8 部署验证
- GitHub Pages（gh-pages）构建产物可跑；`gateway-channels.json` 可 fetch；回退流程（切备份分支）演练一次。

### 8.9 通过与退出标准
三轮各自"发现即修+复验"；§10 验收清单全勾方算完成；无法执行的测试注明原因与未验证范围。

---

## 9. 第八阶段：强制删除旧 UI（验收后）

删除：`src/admin_ui.html`（旧单文件）、其内联 CSS/JS、旧图标/装饰、失效路由与旧 UI 状态代码、绑定旧表现层的无用适配。**不删**：WASM 资源、Rust 业务、数据契约、测试夹具、`sw.js`/`manifest`/`icon.svg`（属新壳资产，保留并按新壳更新）。删除前用静态引用分析 + 构建验证确认安全；回退靠 git 备份分支，不把旧 UI 藏进生产包。

---

## 10. 验收标准与交付物

**验收清单**（对应宪法第十部分）：
- 视觉：无旧元素残留 / 全页面新设计系统重实现 / 组件不继承旧实现 / 语言统一 / 层级清晰 / 桌面+移动完整 / 交互状态完备。
- 业务：F1–F30 全部有新入口且接真实逻辑 / WASM 初始化调用错误处理正常 / 数据契约与持久化兼容 / 异步任务与生命周期正常 / 无假按钮/模拟/占位。
- 工程：生产构建成功 / 自动化测试通过或如实记录 / 关键链路实测 / 旧 UI 与无用依赖清除 / 无阻断问题 / 有部署与回退说明。

**交付物**：新 UI 全源码；功能迁移清单（F1–F30 映射）；设计系统与组件说明；新旧架构边界与适配说明；生产构建产物/命令；自动化测试与关键业务验证结果；删除的旧 UI 资产与依赖清单；部署/配置/回滚说明；未完成事项/已知限制/风险清单。

---

## 11. 风险与需你决策的点

1. **体量与分批**：功能面大（F1–F30），按 §7 分批推进；如需调整优先级（例如先只做"对话+服务商"最小可发布），请指定。
2. **Kit 的"资料库/用户画像"无旧业务对应**：改为「本地数据管理」或移除——需你定，避免留空壳（宪法禁占位）。
3. **品牌与主题色**：QuakeAI 字标 + 由 Kit `--accent` 派生品牌色；是否沿用 Kit 现紫色系 `#4b61d1` 还是指定品牌色？
4. **对话数据源**：会话/附件在 IndexedDB，新 UI 必须无缝读取旧数据（已列为硬约束与测试项）。
5. **单文件 → 多文件**：新 UI 拆为 `index.html + assets/css + assets/js`（含适配层/store/组件/各页），构建脚本从"拼 `admin_ui.html`"改为"组装备份壳 + wasm-bindgen 产物"。
6. **上游转发**：纯静态无服务端，对话出海仍依赖用户自配 `gw_upstream_proxy`（产品语义不变，仅换呈现）。

---

## 附：与宪法的阶段对应

| 宪法阶段 | 落在本计划 |
|---|---|
| 一 最高指令 | §0 |
| 二 审计 | §1（含能力清单/边界/耦合点） |
| 三 三层架构 | §2 |
| 四 设计宪法 | §3 |
| 五 页面重设计 | §4 |
| 六 组件重建 | §5 |
| 七 WASM 铁律 | §6 |
| （质量审查） | §8 三轮 |
| 八 删旧 UI | §9 |
| 九~十一 执行/验收/交付 | §7 / §10 / §11 |
