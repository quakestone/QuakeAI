# zhipu-wasm-gateway（v0.19.23）

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
| v0.19.21 | **发送流水线：文本/图片/音频/视频全进状态机 + 工作流小引擎串行流**：① 媒体状态机六态（就绪→排队→处理中→完成/失败/跳过），附件 chip 徽标 + 输入区 `pipe-bar` 五阶段进度条双反馈；② 新工作流种子 `send_pipeline`（stt→video→image 三步 DAG，depends_on 串行，复用既有 WASM 内核零 Rust 改动）——音频走 STT 转写、视频抽首/中/尾 3 帧过视觉模型、图片按「智能混合」路由（主模型有视觉能力直连 vision parts，否则降级为预描述文字）；③ 理解产出写 `a.desc`：请求里怎么发给模型、气泡里就怎么展示，且随附件持久化；④ 部分失败降级不卡死（假视频→文件名占位），音频全失败才中止并回填输入框；⑤ 另修复 skill 对话框取消/回退无反应（skillDlg 漏定义，见 v0.19.20） | ✅ e2e **456/435→456/456** + 单测 **31/31** + 内核串行行为测试 **23/23** + 浏览器实测 **27/27** |
| v0.19.23（本版） | **SKILL 管理改造**：① 管理页 skill 标签改名「SKILL 管理」，标签下新增四个二级分类（日常应用/专业场景/笔记整理/模型整理），各分类独立页码 + 每页 5 条分页条（与模型列表同范式）；② Skill 数据模型新增 category（二级分类）与 tier（台：系统级/非系统级）两字段，旧账本零迁移（缺省「日常应用/非系统级」），创建/更新 API 校验非法值 400；③ 系统级 skill（tier=system，系统硬编码）不罗列在前端列表流（管理页与选择器同口径）；④ 选择器 SKILL 子页同步二级分类 + 独立页码（UI 趋同管理页）；⑤ 管理页「所有模型」标签去掉「回到对话/模型服务商」两按钮（v0.19.22 常驻决策反转，导航仍有页头 ‹ 回退键与标签栏）；⑥ skill 对话框新增分类/台下拉，提交带白名单校验 | ✅ e2e **481→507/507** + 单测 **31/31** + 行为测试 **41→47/47** |
| v0.19.22 | **UI 优化三连**：① 模型选择器 SKILL 页——对话框标题跟随小标签名（选 SKILL 即「选择SKILL」）、UI 一律大写 SKILL、🧩 换成刀形几何匕首 logo（力量增强意象）、skill 列表与模型列表同范式（每页 5 条 + 分页条 + 搜索过滤）、模型专属控件（可用性检测/重新检测/上下文重置）在 SKILL 标签隐藏；② 模型管理页——「模型」标签改名「所有模型」，「回到对话」「模型服务商」两按钮在全部分支常驻，页头从 bare（标题伪装返回）改为 slim 范式（左上角 ‹ 回退键，与设置页同构）；③ 附件缩略图移入文本框内左上部（图片 42×42 小方片、音频/文件小 pill，名称收进 title），× 可删、可继续补发，「未发送」一眼可辨，消除已发送误会；流水线期间 chip 持续在框内且禁删 | ✅ e2e **456→481/481** + 单测 **31/31** + 行为测试 **23→41/41** + 浏览器实测 **27+36/36** |
| v0.19.20 | **热修复：skill 对话框「取消 / ←回退」无反应**：`skillDlg` 变量漏定义导致点击抛 ReferenceError——补上常量声明并统一对话框取用方式 | ✅ e2e **435/435** + 单测 **31/31** |
| v0.19.19 | **模型选择「诚实化」（方案 A）+ skill 简介 + 管理面板重构**：① 「选了 A 却用 B 回答」根治——`pick_model_attempts` 加 `explicit` 参数，显式选择的模型池中不可用/全部渠道失败时**明确拒绝**（503 `requested_model_unavailable` / 502 `requested_model_unreachable`），绝不静默回退自动选路；自动模式容错语义保留；② skill 加**简介字段**（≤30 字，选择器三行展示，留空自动从提示词首行截取），「＋ 扩展 skill」直通新增对话框，提示词 tab 改名「skill 管理」并去掉管理页新增按钮；③ 管理面板**撤销概览标签**，其信息区（当前模型条/统计 banner/渠道健康速览/最近事件）并入「模型」标签头部；④ **「资源」全量改名「模型服务商」**（tab/按钮/抽屉/预设卡/各页面文案）；⑤ 顺手修复 e2e 遗留 17 个失败（grep 把 HTML 内容当文件名等真 bug + 远程改版未同步断言） | ✅ e2e **435/435** + 单测 **31/31** + 浏览器实测 **16/16** |
| v0.19.3 | **修「选模型不生效」+ 增强模式整块下线（并入 skill）+ 用户消息左对齐 · 移除单文件部署版**：① 服务端 `pick_model_attempts` 对用户显式指定的模型改为从仅排除禁用/冷却的 `all_usable` 池取主选、**绕过 kind 过滤**，选不上才回退自动选路并透传 `x-gateway-model-unavailable` 头 + 前端 toast（不再静默走第一项）；多媒体覆盖收敛为仅本轮真带附件时生效；② **增强模式（⚡ 三段式接力）整块下线**，能力并入 skill（DRILL_PROMPT/三个诸葛亮，提示词保留复用），删除 banner / 双环动效 / 步骤时间线 / 徽章及相关前后端逻辑；③ 用户消息改**左对齐**；④ 移除单文件部署版（`single-file-worker.js` v0.6.0 停更 + `make-single-file.sh`），统一走 wrangler 构建 | ✅ `cargo check` 通过 |
| v0.19.2 | **模型选择逻辑线理顺 + 增强模式修复**：① 增强模式（⚡ 三模型接力 / 自动切换）此前在 skill/音频/图视标签下被隐藏，用户以为「没了」——现已在**文本 + skill 标签**下可见（两者都是纯文本场景）；② 增强模式**尊重用户手选的模型**——显式选了具体模型时用作最终作答模型，不再强制积分「第 1 名」（修复「还是第一模型去提问」）；③ 模型选择器顶部加**常驻「当前选择」条**，并排显示「当前模型」与「表达线（skill）」，厘清两条独立轴线（选 skill ≠ 换模型） | ✅ e2e + 单测 + 浏览器实测 |
| v0.19.1 | **「启用」真正生效**：此前管理面板的「启用/停用」只是展示标签，点了对话里也不会带，导致「skill 启用了却驱动不起来」。修复：① 引入**默认对话 skill**——管理面板「设为默认对话」后持久化到本浏览器，新对话自动带上（composer 显示🧩芯片、发送时注入 system 消息），即「启用就驱动」的语义；② 「启用/停用」改为真开关——停用后 skill 在模型选择器里灰显且不可点；③ 选择器里默认 skill 自动高亮并标注「默认对话」徽章；④ 内置「三个诸葛亮」种子化默认 enabled | ✅ e2e + 单测 + 浏览器实测（默认自动注入 / 停用不可选 / 设为默认后自动带） | **「启用」真正生效**：此前管理面板的「启用/停用」只是展示标签，点了对话里也不会带，导致「skill 启用了却驱动不起来」。修复：① 引入**默认对话 skill**——管理面板「设为默认对话」后持久化到本浏览器，新对话自动带上（composer 显示🧩芯片、发送时注入 system 消息），即「启用就驱动」的语义；② 「启用/停用」改为真开关——停用后 skill 在模型选择器里灰显且不可点；③ 选择器里默认 skill 自动高亮并标注「默认对话」徽章；④ 内置「三个诸葛亮」种子化默认 enabled | ✅ e2e + 单测 + 浏览器实测（默认自动注入 / 停用不可选 / 设为默认后自动带） |
| v0.19.0 | **skill 子系统本体上线**（独立于模型对象的「表达线」）：模型登记「名字 + 能力」，skill 登记「标题 + 提示词正文」，二者共用选择器与管理页同一份列表。① 后端：`Skill` 结构 + DO 存储 `/skills`（GET/PUT 全量）+ admin API（GET/POST/PUT/DELETE + `POST /{id}/use` 累加 used_count）；② 选择器「skill 模型」标签：只显示标题一行，按使用频次取**前 6 条**，第 7 行是「扩展 skill」入口；③ 管理页 skill 面板完整 CRUD（新增/编辑/删除）；④ **首次进入自动种子化内置「三个诸葛亮」**（PRE-ANSWER DRILL PROTOCOL v1.0，与增强模式共用同一段 `DRILL_PROMPT` 单一事实源）；⑤ **聊天接入 = 独立表达线**：选中 skill 后 composer 显示可取消 chip，发送时其提示词作为 `system` 消息注入本轮对话（与选哪个模型无关），并 fire-and-forget 累加 used_count；⑥ `#/skills` 直达路由。踩坑：skill id 生成原用 `char::is_alphanumeric()`，而 Rust 该谓词对**中文返回 true**，导致中文标题原样进 id、URL 路径百分号编码后与存储对不上 → `not_found`，改用 `is_ascii_alphanumeric()` 保证 id 全 ASCII | ✅ e2e **393/393** + 单测 **22/22** + 积分 **12/12** + 浏览器实测 **9/9 零 JS 错误**（种子化/选中 chip/管理 CRUD/真实请求体验证 system 注入） |
| v0.18.2 | **后台 HTML 禁缓存 + 缓存自检**（"改了没生效"的真凶）：用户反馈「模型选择页面的黑块还没处理掉 skill 标签还是没有增加回来」，排查发现代码与构建产物都正确（wasm 里 `MP_TABS = ["text","skill","audio","mm"]`、服务返回页面含 2 处 skill 标签），**根因是后台 HTML 从来没发过 `Cache-Control` 头**（只有流式 API 有 `no-cache`）——后台是「HTML 骨架 + 内嵌 wasm」的 SPA，浏览器缓存后一直跑旧代码，反复普通刷新无效。修复：① `html_response` 补 `Cache-Control: no-store, no-cache, must-revalidate, max-age=0` + `Pragma: no-cache` + `X-Gateway-Version`；② 构建时用 `split+join` 把版本号注入 HTML 的 `__BUILD_VERSION__` 占位（no_std 无 `replace_all`）；③ 前端 `BUILD_MARK` 与 `/health` 版本比对，不一致时侧栏亮红字提示硬刷新 | ✅ e2e **375/375** + 单测 **22/22** + 积分 **12/12** + 浏览器真实回归 |
| v0.18.1 | **恢复 skill 标签 + 修掉增强模式行的「空框」**：上一轮把「去掉圈中的空白」误读成「去掉 skill 标签」，本版恢复：① **skill 是独立于模型对象的表达线**（模型登记「名字 + 能力」，skill 登记「标题 + 提示词正文」），模型选择器回到 **4 标签**、管理页回到 **5 标签**，`#pane-skill` 独立面板恢复，且 `mpItems()` 里 skill 标签明确不从模型池取数；② **真正的 bug**：`.enh-row` 是 flex 容器但checkbox 没写 `flex: none`，`min-width: auto` 把它撑到 **290px 宽**，把 ⚡ 与「增强模式」文字全顶到右侧，整行看起来就是一个空大框 —— 加 `flex: none; width: 15px` 后实测回到 **15px**，行高 73px → 46px；③ 顺带**增强模式只在「文本模型」标签下出现**（纯文本接力在音频/图视/skill 标签下无意义），切标签时 `enh-row.hidden`同步 | ✅ e2e **367/367** + 单测 **22/22** + 积分 **12/12** + 浏览器真实回归 |
| v0.18.0 | **去掉 skill 标签 / 管理页列表两标签 / 会话列表分页 / 屏蔽 error**：① 用户指令「1.模型选择页 这块不是说好去掉吗 skill有自己的表达线 2.模型管理页的刷新按钮屏蔽掉 把模型登记入口放到刷新位置 还有那个多媒体设置按钮去掉 3.模型管理列表分两标签：文本主流.多媒体模型（不要再瀑布了 列表深度只要超过5个全部改成分页）4.那个error也屏蔽掉 6.历史会话页面列表也是 该翻页一定翻页 不要一拉到底」：模型选择器回到 **3 标签**（文本/音频/图视）、管理页回到 **4 标签**（文本模型/多媒体模型/模型用量/模型积分榜），skill 占位全部删除；② 管理页头 **删掉刷新按钮**、原位换 **「＋ 登记资源」**，同时**删掉 🎛 多媒体设置图标**（卡片内仍有内联配置）；③ 模型列表收敛为 **文本 / 多媒体** 两标签（多媒体合并 stt+tts+vision+video），`MG_PAGE_SIZE` 降到 **5**，超 5 条必分页；④ store 账本异常时顶栏徽章由红色 `store: error` 改为中性 `store: —`（详情仍在store-banner）；⑤ **历史会话列表分页**（每页 8 条，翻页只切内存不重读 IndexedDB，打开会话自动跳到所在页、新建回第 1 页） | ✅ e2e **363/363** + 单测 **22/22** + 积分 **12/12** + 浏览器真实回归 |
| v0.17.0 | **模型选择页 4 标签 + 模型管理 6 标签 + 全量翻页 + 首页品牌 logo**：① 用户指令「模型选择页做多标签：一个标签：文本模型.skill模型.音频模型.图视模型 瀑布装不下做翻页 首页左上角加logo 并列出产品名称：QUAKEAI网关，模型管理页面除了以上四个标签 加模型用量 模型积分榜（不要渲染出界外）该翻页全部翻页」：模型选择器由单一列表改为 **4 标签**（文本/skill/音频/图视），按**主/次要能力**严格分类（旧版会把 tts/vision 混进文本列表），**各标签页码互相独立**；② 模型管理页扩为 **6 标签**（上述四个 + 模型用量 + 模型积分榜），用量与积分榜由独立卡片搬进标签面板，标题去重；③ **该翻页全部翻页**：模型列表 8/页、积分榜 10/页、用量三表 8 行/页（切页走前端缓存，不重复请求），排名用全局序号；④ **防出界三层断链**：卡片/面板体 `min-width:0` + `.tbl-scroll` 横向滚动壳，浏览器实测 5 个标签 `scrollWidth === clientWidth`、出血元素 0；⑤ 首页左上角加 **渐变 SVG logo + 产品名「QUAKEAI网关」**，顶栏标题改可收缩 | ✅ e2e **355/355** + 单测 **22/22** + 积分 **12/12** + 浏览器真实回归 |
| v0.16.0 | **模型选择器分页 + 增强模式三段接力**：① 用户指令「模型选择页面 模型列表每页最多六个 做分页管理 / 模型管理的入口放在这个小页面的右上角 / 支持模型名称模糊查询 / 加个模型增强的按钮：增强模式 就是问题让积分第三的模型自我 drill 3 遍后 带上上下文 给积分第二的模型总结 把问题说清楚来 最后给模型第一的问题回答 / 问题发送完 发送按钮的动销要增强显示 非增强提问 也要动效区别开来 同时内容区的模型回答时不能死等 借鉴 agent 把模型轮换问答的内容也展示出来按步骤顺序 / 首页右上角的槽位给新会话按钮 与月亮按钮对调」：模型选择器新增 **每页 6 条分页**（「自动」钉在首页不参与切割）+ **模型名/渠道名/供应商三维模糊搜索**（换词回第 1 页）+ 右上角 **⚡ 模型管理入口**；② **增强模式三段式接力**：`DRILL_PROMPT` 内置 **PRE-ANSWER DRILL PROTOCOL v1.0** 全文，积分第三名自我 drill 3 遍（每轮带上轮产出形成自我迭代）→ 第二名总结最终任务定义 → 第一名给用户作答，共 5 步；生效条件严格按要求：**非音频问答的纯文本 且 文本 > 20 字 且至少 3 个可用模型**，不满足时开关置灰并给出原因；③ **步骤时间线**（借鉴 agent step chain）：正文之上按步骤显示每阶段承担模型与状态（●待执行/◐流式/✓完成/✕失败），可折叠展开看全文，回答区不再死等；④ **发送按钮双态动效**：普通生成 `■`+青蓝呼吸光晕，增强生成 `⚡`+紫青双环反向旋转+3 段进度点，并尊重 `prefers-reduced-motion`；⑤ 顶栏右侧顺序改为 主题→通话→新建会话；⑥ 模型管理页头部修复（标题块 `flex:1 1 auto; min-width:0` 此前被压成「每行一个字」的竖排）+ 模型列表从渠道卡内的瀑布抽成 **三标签页平铺**（文本/音频/图片·视频），徽章溢出修复 | ✅ e2e **333/333** + 浏览器真实回归 |
| v0.15.0 | **UI 体验刷版 + 模型积分体系**：① **模型积分账本**（用户指令「模型管理是大模块 每次模型选择+1 每次模型降权-20 模型每使用一次+1 每分享一次+10 建立各大模型的个人使用数据系统 选择模型时以积分多少倒序排列 自动模型也以此为例」）—— 4 个计分点全部接入，模型选择器与能力下拉**按积分倒序**，「自动」以 `__auto__` 独立计分并参与同一排序；② **新诊断头 `x-gateway-degraded`**：原 `x-gateway-attempts` 只有渠道名而降权是**模型级**的，前端要按模型扣分必须知道具体哪些模型被降权，故新增逗号分隔的被降权模型名列表并贯通 chat/audio/tts 三链路与两条失败路径；③ **UI 七项改造**：所有子页/弹层统一左侧边缘右滑关闭（分层：对话框 > 通话页 > 侧栏）+ 左上角回退按钮（6 个 dialog + 通话页 + 模型管理页 + 分享落地页）、首页模型设置按钮去掉、模型选择按钮移到输入区 `+` 号旁（右上角腾给新建会话）、语音设置拆成「🎧 音频模型」「🖼 图片视频模型」两块（**三个模型选择真正接入请求**：TTS→`body.model`、STT→FormData、多媒体→聊天 `body.model`）、通话页 TTS 播放修复（上游 WAV 常回 `application/octet-stream` 被浏览器拒解码 → 按字节头嗅探 MIME，叠加 iOS 自动播放拦截 → Web Audio 兜底）、AI 精灵**拟人化且零动效**（删除全部 `@keyframes orb-*`，改为静态五官）；④ 积分存 localStorage **刻意不落 DO**（DO 是多人共享账本，积分是个人偏好，混进去会污染他人排序） | ✅ e2e **287/287** + 单测 **22/22** + 积分执行级测试 **12/12** + 浏览器真实交互回归 |
| v0.14.0 | **DeepSeek 渠道对接 + 用量成本账本**，含**路由可达性重大修正**（用户反馈「你没把ds的模型对接进来吗」后定位：`deepseek-flash` 被标 `kind=vision` 导致**纯文本请求整条过滤**它、DeepSeek 流量全落更贵的 `deepseek-v4-pro`；第一版「vision⊃text 一律放宽」修法又让高优先级视觉模型抢走纯文本流量（§14.1 回归失败）；**正解是新增 `ModelMeta.alt_kind` 次要能力** —— 候选判定 `serves()` 主或次命中即进池，排序 `primary_rank()` 作最高优先级键保证主能力优先、alt 只兜底，两处能力过滤同步改）。用户指令：「这个是DeepSeek的 你也对接下」+ 纠偏「我给你是空白的文件 你也在做」）：① **DeepSeek 种子落库** `seed-deepseek-chat`（`api.deepseek.com/v1`、priority 70 夹在国内 80~100 与海外 40/35/30 之间、默认 disabled、caps text+vision）；现行模型实况化——只保留 `deepseek-flash`（V4.1-Flash、1M 上下文、**原生支持 image 输入**）与 `deepseek-v4-pro`（纯文本旗舰），移除已弃用的 `deepseek-chat`/`deepseek-reasoner` 与已下线的 `deepseek-v4-flash-vision-exp`；② **单价档案** `ModelMeta` 新增 `price_in_hit`/`price_in_miss`/`price_out`/`currency`，实现 DeepSeek 式两段输入计费（缓存命中/未命中分别计价）+ 输出单独计价（Flash 0.02/1/2、Pro 0.025/3/6 CNY 每百万 token）；③ **逐请求用量账本** `UsageRecord` + DO `usage` 键与 `GET/POST/DELETE /usage`，`Response::clone()` tee 旁路采集上游 `usage`（不改动流重放，零回归），兼容顶层与 SSE 末帧；④ `GET /admin/api/usage` 聚合（totals/按源/按 源×模型/按自然日，含 success_rate、cache_hit、mixed_currency 标记、avg_latency）与 `GET /admin/api/usage/export` 对账 CSV（**列名严格对齐官方 `cost-*.csv`** + UTF-8 BOM）；⑤ 管理页新增「用量与成本」面板（6 指标卡 + 三张分组表 + 导出按钮）；⑥ 方案纠偏：用户上传的官方 `usage_data_*.zip` 两个 CSV **只有表头零数据行**，故**放弃 CSV 导入对账**（空转功能），改为网关自身记账 + 导出对齐官方 schema | ✅ e2e **242/242** + 单测 **22/22** |
| v0.13.0 | **海外视觉全量对接**（用户指令「把海外视觉模型全部对接补上」）：① OpenRouter 种子补全全部免费视觉/多模态模型——`google/gemma-4-26b-a4b-it:free`、`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`、`thinkingmachines/inkling:free`、`thinkingmachines/inkling-small:free`，共 7 个免费视觉模型可轮换防 429；② 修正两处误标——`qwen/qwen3.8-27b:free` 与 `openrouter/free`（自动路由，含视觉）由 `text` 改为 `vision`；③ **视觉甄别增强**——`payload_has_image` 现识别 `video_url`（不仅 `image_url`），视频附件同样路由到视觉/多模态渠道（适配 omni 模型）；④ 视觉/文本共用**同一**选择逻辑（`payload_has_image`→`want_kind`→`pick_model_attempts` 单一引擎处理 text/vision/stt/tts，无双分支）；⑤ 单测新增 `payload_vision_detection` + `overseas_seed_channels_well_formed` 扩展校验 | ✅ e2e **208/208** + 单测 **13/13** |
| v0.12.0 | **海外模型融合**（用户指令「把国外的那几个模型都融和进来」）：① 三个海外预设（OpenRouter / Groq / NVIDIA NIM）从「仅登记模板」升级为**网关种子资源**——一键载入即落库 6 条（国内 3 + 海外 3）；② 海外种子 `enabled=false`、优先级 40/35/30 低于国内（100/90/80），作「国内渠道全挂时的兜底」；③ 各家现行免费模型清单实况化（OpenRouter 17 个 `:free`，含 `openrouter/free` 自动路由 + `gemma-4-31b-it` 视觉；Groq 14400 RPD/30 RPM 免卡含 `llama-3.2-11b-vision-preview`；NVIDIA NIM ~40 RPM 免卡含 `nemotron-3.5-super-vl-120b-a12b` 视觉）；④ 海外种子均声明 `text+vision` 能力（各视觉模型入 `models_meta`），国内视觉全挂时 `image_url` 自动落到海外视觉模型兜底；⑤ 预设 vision 推断正则扩展覆盖 gemma/vl；⑥ 单测新增 `overseas_seed_channels_well_formed` | ✅ e2e **202/202** + 单测 **12/12** |
| v0.11.0 | **测试反馈三修**（用户实测三条）：① **呼叫页播放修复**——iOS/Android 媒体自动播放策略下「按键→识别→流式→合成」链路已离手势数秒，`Audio.play()` 被拒/`speechSynthesis` 不发声；首个 pointerdown 手势内 `unlockMedia()` 预热（AudioContext resume + 0.1s 静音 WAV 换 sticky activation + 空 utterance 唤醒），`openCall()` 显式再调；② **交流模式**——`callMode`（duplex 同时问答默认 / half 一问一答禁抢话），呼叫页 ⇄/↔ 按钮切换 + VoiceCfg 持久化 + 状态文案分化 + TTS 降级 toast 可见提示；③ **分享直通系统面板**（修「没成功」）——`shareMsg()` 优先 `navigator.share()` 不弹二级页，AbortError 安静退出，失败才落级对话框；顺手修 s. 子域 404 实测根因（workers.dev 等 22 个平台域无权配 s. 子域 → PLATFORM_SUFFIXES 回落同源 /s，node 单测 6 用例）；④ **多媒体容器播放**——音频/视频存 Blob 进 IDB，消息渲染 `<audio controls>` / `<video controls playsinline>`，blob: URL WeakMap 缓存，视频 canvas 抽帧首帧 poster 缩略图（输入区 chip + 消息双呈现），刷新后容器仍可播放 | ✅ e2e **183/183** + 单测 **11/11** + Playwright **23 断言三场景**零 JS 错误 |
| v0.10.0 | **分享微信功能线 / 二级子页分发裂变**（用户三条优化指令之分享线）：① 新增 `GET /s` 独立分享页（share_ui.html include_str! 进 WASM，随网关部署零基础设施）；② **零存储架构**：回答全文 deflate-raw 压缩 → base64url 塞 URL hash（`z.` 前缀，老浏览器降级 `p.` 明文）——fragment 浏览器不发给服务器，无存储无日志天然「阅后即焚」；③ `shareOrigin()` 二级子域推断（www.x.com → s.x.com，生产 CNAME 即可；localhost/IP 回落同源）；④ 分享页：迷你 Markdown 渲染 + 二维码 + 微信 UA 检测（内置浏览器引导「⋯→发送给朋友」，外部浏览器引导「复制链接/扫一扫」）+ 明暗主题 + 「打开应用继续对话」裂变回流 CTA；⑤ 管理页分享对话框（复制/系统分享/二维码/打开分享页，clipboard 失败降级 execCommand）；⑥ 6000 字上限截断保护；⑦ 顺手修 p. 明文路径 base64url 字母表 bug（实测挖出：中文必含 -/_ 直接炸解码） | ✅ e2e **164/164** + 单测 **11/11** + Playwright 六场景实测 |
| v0.9.0 | **聊天记录迁 IndexedDB**（用户三条优化指令之聊天记录线）：① convos/msgs 双 store（消息复合主键，重发截断同位置覆盖不重复）；② 附件随消息持久化——图片 dataUrl 刷新后缩略图不丢、音频 Blob 刷新后重发可用；③ **可删**：会话级/消息级（长按菜单删除）/一键清空；④ **可查**：侧栏搜索标题即过滤 + 消息内容跨会话命中（片段高亮）；⑤ **联动视图**：搜索/删除/打开同一渲染链路；⑥ **流式持久化**：SSE 节流落库 + pagehide/visibility 双兜底冲刷，流式中刷新不丢已生成部分；⑦ **新会话修复**：旧流抢占（streamGen）——否则发送键是旧流停止键，新对话发不出去；⑧ 顺手修 toast 拦截底部点击（悬浮 4.6s 输入区点不动） | ✅ e2e **146/146** + 单测 **11/11** + Playwright 九场景实测 |
| v0.8.0 | **语音全链路重构**（用户三条优化指令之语音线）：① 麦克风单所有者 `MicHub` + `VoiceInput` 单消费者（识别与录音绝不并存——「第二次就挂/STT 跑不通」的软件层根治）；② 通话状态机 `CallFsm` 六态，**barge-in 播报中抢话打断是一等公民**（speaking→listening 立即停播+断流+释放麦克风）；③ TTS 三层递降链：网关 → 自定义端点（OpenAI 兼容，客户自有资源替换点）→ 浏览器语音（GitHub 抓 Speakit-JS 融合改造，离线兜底）；④ STT 两层链：浏览器实时识别（零上传）→ 网关转写；⑤ 🎛 语音设置对话框（模式/端点/音色/语速可配，localStorage 持久化） | ✅ e2e **124/124** + 单测 **11/11** + Playwright 六场景实测（打断停播/浏览器兜底/端点降级） |
| v0.7.5 | **修复呼叫页「第二次就挂」**：① `callBusy` 加锁提前到录音启动（曾在 `await rec.stop()` 之后——松手连按导致两个 MediaStream 并发，iOS 直接拒）；② `pumpTts` 未被 await 导致 callBusy 早释、两轮播报声音叠加——新增 `ttsQueueDrained()` 等队列播空才松锁；③ 阶段性 STT 每次 `new AudioContext` 累积触顶 iOS 并发上限——改全局单例复用；另加 60s 超时兜底/防重入/入口残留清理/挂断中断 SSE 流 | ✅ e2e **107/107** + 单测 **11/11** + Playwright 连按复测 |
| v0.7.4 | **修复侧栏版本号显示过期值**（「前端版本变成 6.0」）：HTML 初始值去硬编码 + `refreshVersion()` /health 兜底；顺带挖出 `admin.rs` 独立 VERSION 常量停更的深因——全部收敛到 `env!("CARGO_PKG_VERSION")` 单一事实源（以后升级只改 Cargo.toml） | ✅ e2e **95/95** + 单测 **11/11** + Playwright 三场景实测 |
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
bash scripts/test-e2e.sh  # 自动起 mock + workerd，跑 183 项断言
```

手动跑：

```bash
node scripts/mock-zhipu.mjs &                    # 本地 mock 上游（127.0.0.1:8788）
workerd serve workerd.config.capnp               # 网关（127.0.0.1:8787）
open http://127.0.0.1:8787/admin                 # 资源管理页面
open http://127.0.0.1:8787/s                     # 分享落地页（载荷在 URL hash，服务端零存储）
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
- **用量与成本面板（v0.14）**：每次转发成功后从上游响应的 `usage` 字段取 token，按模型
  内置**单价档案**（`price_in_hit` / `price_in_miss` / `price_out`，DeepSeek 式两段输入计费）
  折算成本并落 DO 环形账本（保留 30 天）。面板展示请求数 / 成功率 / 总 token / 缓存命中 /
  估算成本 / 平均延迟 6 项指标，以及「按源 / 按 源×模型 / 按自然日」三张分组表。
  采集走 `Response::clone()` tee 旁路，**不改动流式重放逻辑**。
- **对账 CSV 导出**：`GET /admin/api/usage/export` 导出网关账单，列名严格对齐 DeepSeek 官方
  `usage_data` 的 `cost-*.csv`（`user_id,start_time_iso,end_time_iso,model,name,wallet_type,cost,currency`），
  带 UTF-8 BOM（Excel 双击不乱码）。将来官方有数据时，把官方 CSV 按 model+日期透视后与本表
  并排 diff 即可对账。（注：不做官方 CSV **导入**——网关自己记的账才是第一手数据。）
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

## 验证结论（mock e2e 242/242 + 单测 22/22 + 分版本浏览器交互实测通过）

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

v0.14.0 新增验证（e2e §20，35 项断言全绿）：

| # | 验证点 | 结果 |
| --- | --- | --- |
| 37 | DeepSeek 种子落库：`api.deepseek.com/v1`、`deepseek-flash` + `deepseek-v4-pro`、已下线模型已移除 | ✅ |
| 38 | Flash 单价档案 0.02 / 1 / 2 CNY 每百万 token 正确落库 | ✅ |
| 39 | `deepseek-flash` 双能力（主 text + 次 vision）、`v4-pro` 纯 text、caps 含 vision | ✅ |
| 40 | 种子默认 disabled、priority 70 夹在国内(80/90)与海外(40)之间 | ✅ |
| 41 | **非流式 + 流式各记 1 条用量**落 DO（tee 旁路采集，SSE 末帧 usage 同样解析） | ✅ |
| 42 | token 解析正确（prompt / completion / total / cache_hit 逐项累加） | ✅ |
| 43 | 聚合三维度齐全（按源 / 按 源×模型 / 按自然日 YYYY-MM-DD） | ✅ |
| 44 | 成本按单价档案折算 > 0、`priced_requests` 与币种 CNY 正确 | ✅ |
| 45 | 对账 CSV：`text/csv` + 附件下载 + **UTF-8 BOM**（Excel 不乱码） | ✅ |
| 46 | CSV 列名**严格等于**官方 `cost-*.csv` schema，数据行以 `gateway` 标识 | ✅ |
| 47 | **纯文本命中 `deepseek-flash`** 且未误用更贵的 `v4-pro`（alt_kind 兜底生效） | ✅ |
| 48 | **视觉请求同样命中 `deepseek-flash`**（原生支持 image 输入） | ✅ |
| 49 | **§14.1 视觉/文本隔离无回归**：prio 80 的 `glm-4v-flash` 不抢 prio 50 的 `glm-4-flash` 纯文本流量 | ✅ |
| 50 | 前端 `alt_kind`：次要能力下拉 + 保存读取 + 双徽章（虚线样式）+ 筛选命中 + API 往返持久化 | ✅ |
| 51 | 管理页「用量与成本」面板容器 + 导出按钮 + 拉取 `/admin/api/usage` | ✅ |

唯一未测项：**有效 Key 的真实对话/转写响应**（需要你自己的 Key，管理页登记即用）。
DeepSeek Key 已实测可读模型清单，但 `chat/completions` 返回 `HTTP 402 Insufficient Balance`
（账户零余额）——这也正是官方 `usage_data` 导出为空的原因，故真实 usage 抓取未能联调，
usage 字段按官方文档实现并用 mock 验证。

## 目录结构

```
├── src/
│   ├── lib.rs            # 入口路由 + 多通道重试 + 首帧拼流 + multipart 重建
│   ├── channels.rs       # Channel 模型 / 预设目录（含 DeepSeek）/ 掩码 / 冷却判定 / seed / 单价档案
│   ├── store.rs          # DO 账本 RPC（load/save/events/usage）
│   ├── admin.rs          # /admin API：CRUD、探测、seed、events、usage 聚合与 CSV 导出、健康回写
│   ├── admin_ui.html     # 类 ChatGPT 单页：对话首页 + 模型管理二级子页 + 用量成本面板（include_str! 内嵌）
│   └── share_ui.html     # 分享落地页 /s（include_str! 内嵌，URL hash 载荷零存储渲染）
├── scripts/
│   ├── build.sh          # cargo + worker-build + 追加 ResourceStore DO 类
│   ├── make-single-file.sh  # 合成 deploy/ 单文件（wasm base64 内联，Dashboard 粘贴用）
│   ├── storage-do.js     # JS Durable Object 类（追加进 build/index.js）
│   ├── mock-zhipu.mjs    # 本地 mock：流式/非流式/429/502/断流/multipart 全行为
│   └── test-e2e.sh       # 一键端到端（233 项断言，多 workerd 配置切换）
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
