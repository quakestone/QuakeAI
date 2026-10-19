# CHANGES

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
