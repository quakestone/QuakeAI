# 开发说明：模型可用性扫描与受限态呈现（v0.19.10）

> 状态：**待评审，未开发**
> 目标版本：v0.19.10
> 前置版本：v0.19.8（`POST /admin/api/models/test` 单模型自检）、v0.19.9（换模型上下文边界）

---

## 0. 需求复述（确认理解）

| 你的原话 | 我理解的需求 |
|---|---|
| 不能用的模型显示效果与能用的要区别开来 | 模型列表里，**受限/不可用**的行在视觉上与可用行明显区分（不只是灰一点） |
| 在前端选择模型页面的时候，工作流引擎逐个去查下本页模型的可用性 | 打开模型选择弹窗时，由 WASM 工作流引擎驱动，**对当前页可见的模型逐个**做可用性探测 |
| 把受限原因显示在模型名称的括号里 | 行标题变成 `模型名（原因）`，如 `cohere/north-mini-code:free（渠道未启用）` |
| 同时改变效果 | 括号出现的同时，行样式随之变化（色条/图标/透明度） |
| 并记录在状态机 | 探测结论落在 WASM 状态机里，不是散落的布尔值/局部变量 |
| 再次进入页面在两小时后 就再来一次可用状态扫描 | 缓存 TTL = **2 小时**；超过则重新扫描 |
| 两小时内再进入模型页面不做扫描 | TTL 内命中缓存，**零网络请求**，直接渲染缓存结论 |

一句话：**打开模型页 → 引擎扫本页 → 结果进状态机 + 本地缓存 2 小时 → 用括号和样式把"能不能用、为什么不能用"直接摆在名字旁边。**

---

## 1. 现状与差距

| 现状 | 问题 |
|---|---|
| `usableChatChannels()` 只判断 `enabled` + `api_key_masked !== "未登记"` + 未在冷却 | 这是**配置级**可用，不是**实测级**。Key 填了但无效、模型已下架、被 429 限流，都显示为"可用" |
| 模型列表按积分倒序排列，无任何可用性标记 | 用户选完才发现不能用，且不知道原因 |
| v0.19.8 的 `/models/test` 只能手动、单个、无缓存 | 没有批量、没有 TTL、没有状态机，无法支撑"进页面自动扫" |

---

## 2. 状态定义

### 2.1 主状态（状态机枚举）

| 码 | 名称 | 含义 | 是否"可用" |
|---|---|---|---|
| 0 | `Unknown` | 未探测 | — |
| 1 | `Probing` | 正在探测 | — |
| 2 | `Ok` | 实测通过，且上游自报模型 == 所选模型 | ✅ |
| 3 | `Substituted` | 可用，但上游换成了别的模型 | ✅（有警示） |
| 4 | `Blocked` | 受限，但**用户可修复**（未启用 / 未配 Key / Key 无效 / 限流 / 冷却） | ❌ |
| 5 | `Gone` | 不可用且**用户无法自行修复**（无渠道登记 / 上游 404 模型不存在） | ❌ |

### 2.2 受限原因（括号文案）

括号里只放**一句话、可行动**的原因。同一模型可能挂在多个渠道，取**最优状态**；全部不可用时，按"修复代价从低到高"取第一条：

| 优先级 | reason code | 括号文案 | 触发条件 | 用户该做什么 |
|---|---|---|---|---|
| 1 | `disabled` | `（渠道未启用）` | 所有提供该模型的渠道都 `enabled=false` | 去模型管理启用渠道 |
| 2 | `no-key` | `（未配置 Key）` | 渠道已启用但 `api_key` 为空 | 登记 Key |
| 3 | `auth` | `（Key 无效）` | 上游 401 / 403 | 换 Key |
| 4 | `rate-limited` | `（限流中）` | 上游 429 | 等冷却 / 换模型 |
| 5 | `upstream-5xx` | `（上游异常）` | 5xx | 稍后重试 |
| 6 | `timeout` | `（超时）` | 超过单次探测超时 | 稍后重试 |
| 7 | `network` | `（网络不通）` | fetch 抛错 | 检查网络 |
| 8 | `not-found` | `（模型不存在）` | 上游 404 | 从渠道里删掉或改名 |

特殊情况：

- `Substituted` → 括号文案 `（上游实为 xxx）`，**不算受限**，行仍为可用样式，但加一个浅色警示角标。
- `Probing` → 括号文案 `（检测中…）`，右侧小 spinner。
- `Ok` → **不加括号**，保持现状（避免满屏噪音）。
- 缓存过期但尚未重扫 → 保留旧括号 + 名称前置 `~`（表示结论可能过期）。

---

## 3. 视觉规范

复用现有 CSS 变量，不新增主题色。

| 状态 | 行样式 | 名称 | 右侧信息 |
|---|---|---|---|
| `Ok` | 现状不变 | `模型名` | 渠道 · 供应商 · 档位 · 积分 |
| `Substituted` | 现状 + 右上角小警示点 | `模型名（上游实为 xxx）` | 同上 |
| `Blocked` | 整体 `opacity: .55` + 左侧 3px 暖色竖条 + 名称前 `⏸` | `模型名（渠道未启用）` | 原因之外仍保留渠道信息 |
| `Gone` | `opacity: .4` + 左侧 3px 冷色竖条 + 名称前 `✕`，**不可点选** | `模型名（模型不存在）` | 同上 |
| `Probing` | 现状 + 右侧 spinner | `模型名（检测中…）` | 同上 |

补充：

- **`Blocked` 与 `Gone` 的行一律不可点选**（已确认：不允许选择受限模型），`cursor: not-allowed`，点击时 toast 一次原因，不做任何选择动作。
- **已选中的模型在扫描后变为受限**：保留用户的选择，**不静默清除**（用户的选择权优先于一次快照结论），但在弹窗顶部显示一条警示条：`当前所选 xxx 受限：渠道未启用`，并在该行加 `current` 标记。用户可自行改选。
- 括号文字用 `var(--dim)`，比模型名小一号，不抢主信息。
- 移动端（当前弹窗宽度）：括号必须能换行，不撑破布局；用 `white-space: normal` 而非 `nowrap`。

---

## 4. 扫描机制

### 4.1 触发

| 时机 | 行为 |
|---|---|
| 打开模型选择弹窗（聊天页 `openModel()`） | 扫**当前页**可见模型 |
| 翻页 / 切换 tab / 搜索词变化 | 对**新出现的这一页**扫（未命中缓存才发请求） |
| 2 小时内再次打开 | **不扫**，直接渲染缓存 |
| 超过 2 小时 | 重新扫当前页 |
| 用户点「重新检测本页」 | 强制扫，绕过 TTL |

> 触发方式：打开/翻页时派发工作流事件 `models.page_open`，由引擎起一条 run 跑完本页（详见第 6 节）。

### 4.2 范围

- **只扫当前页**：`MP_PAGE_SIZE = 6`，即一次最多 6 个模型（「自动」项不扫，它不是具体模型）。
- 串行执行，不并发：避免触发上游 RPM 限制，也让进度可读。
- 单模型超时 **8 秒**，整页超时 **45 秒**；超时按 `timeout` 记。

### 4.3 跳过条件

| 条件 | 原因 |
|---|---|
| 正在流式生成（`streaming === true`） | 扫描会抢额度并干扰流式体验 |
| tab 为 `skill` | skill 不是模型对象 |
| 页面不可见（`document.hidden`） | 移动端后台不浪费流量与电量 |
| 同一页已有 `Probing` 在跑 | 防重入 |

### 4.4 缓存

- 存储：`localStorage` 单键 `gw_avail_v1`，值形如
  ```json
  { "cohere/north-mini-code:free": { "st": 4, "rs": "disabled", "ts": 1792525095000, "http": 0, "ms": 0, "served": null } }
  ```
- **TTL = 2 小时 = 7_200_000 ms**，常量 `AVAIL_TTL_MS`，写在状态机里而非散落前端。
- 容量保护：只保留最近 **200** 条，超了按 `ts` 淘汰最旧。
- 清理：渲染前按当前模型列表过滤，模型被删掉的条目自然丢弃。
- 不进 IDB：这是短生命周期数据，不值得一次 schema 迁移。

### 4.5 探测请求

复用 v0.19.8 的判定口径（单轮、空历史、`temperature: 0`、`max_tokens: 64`），保证"自检"和"自动扫描"结论一致——**同一件事不能有两套判定**。

---

## 5. 状态机设计（WASM 引擎）

### 5.1 为什么放引擎

与 v0.19.5 `MemoryFSM`、v0.19.6 `ModelSelFSM` 一致：

1. 判定逻辑单点收敛，不在前端散落 `if/else`；
2. 可写原生单测（`cargo test`），不依赖浏览器；
3. 通话页、未来的其它入口复用同一套判定。

新增模块 `ModelAvailFSM`，与 `modelsel_abi` 并列，互不影响。

### 5.2 状态与事件

```
状态：Unknown(0) → Probing(1) → Ok(2) | Substituted(3) | Blocked(4) | Gone(5)
                                          ↑ 任意状态可因 TTL 过期回到 Probing
```

| 事件 | 迁移 |
|---|---|
| `page_enter(now, last_ts)` | `now - last_ts >= TTL` → `Probing`；否则 → 保持原状态（`CacheHit`） |
| `probe_begin` | `→ Probing` |
| `probe_ok(served, requested)` | `served == requested` → `Ok`；否则 → `Substituted` |
| `probe_http(status)` | 401/403→`Blocked(auth)`；429→`Blocked(rate-limited)`；404→`Gone(not-found)`；5xx→`Blocked(upstream-5xx)` |
| `probe_error(kind)` | `timeout`→`Blocked(timeout)`；`network`→`Blocked(network)` |
| `cache_expire` | `→ Unknown`（下次 `page_enter` 必扫） |
| `mark_disabled` / `mark_no_key` | 不发请求，直接 `Blocked(disabled / no-key)`（省额度） |

> 注意 `mark_disabled` / `mark_no_key`：这两种情况**不需要网络**就能判定，扫描时直接短路，既省额度又快。这是当前 v0.19.8 接口已经在做的（`skipped-disabled` / `skipped-no-key`），状态机里固化成显式迁移。

### 5.3 ABI（命名沿用现有规范）

```
avail_new() -> i32                         // 句柄
avail_drop(h)
avail_begin(h)                             // 进入探测
avail_result(h, state: i32, reason: i32, http: u32, latency: u32, served_ptr, served_len)
avail_state(h) -> i32
avail_reason(h) -> i32
avail_reason_label(h) -> ptr/len           // 括号文案（含 Substituted 的"上游实为 X"）
avail_should_scan(h, now_ms: f64, ttl_ms: f64) -> i32   // 2 小时规则的唯一实现
avail_touch(h, now_ms: f64)                // 写回 ts
```

### 5.4 单测清单（`cargo test`）

1. `avail_ttl_blocks_rescan` —— 1 小时内 `should_scan == 0`
2. `avail_ttl_allows_rescan` —— 2 小时零 1 秒后 `should_scan == 1`
3. `avail_ok_when_served_matches`
4. `avail_substituted_when_served_differs` —— 含 `a/b:free` 形态
5. `avail_blocked_on_401_403_429`
6. `avail_gone_on_404`
7. `avail_reason_priority` —— 多渠道全挂时取"最可行动"的那条
8. `avail_label_no_parentheses_when_ok` —— 可用态不加括号（防噪音回归）

---

## 6. 扫描由工作流引擎任务驱动（已确认方案）

**不新增后端接口**。扫描是一条**工作流任务**，由现有 WASM 工作流引擎编排，每个模型是一个步骤，步骤里调已有的 `POST /admin/api/models/test`。

### 6.1 复用现有引擎机制

现有引擎的调用约定（`wfDrive`）：

```
engine_start(handle, eventJson) → rid
  ↳ engine_ready(rid) 取就绪步（scratch 里是 steps[]）
  ↳ 逐个执行插件 → engine_complete(rid, stepId, outJson)
  ↳ 失败 → engine_fail(rid, stepId, errMsg)，返回可否重试
  ↳ steps 为空 = run 终态
```

### 6.2 动态 seed：一次页面扫描 = 一条 run

页面上有几个模型，就生成几个步骤——seed **在运行时按当前页模型列表生成**，不写死：

```json
{
  "id": "model_probe",
  "version": 1,
  "trigger": { "event": "models.page_open" },
  "steps": [
    { "id": "p0", "plugin": "wf.model_probe", "input": { "model": "cohere/north-mini-code:free" } },
    { "id": "p1", "plugin": "wf.model_probe", "input": { "model": "google/gemma-4-31b-it:free" } }
  ]
}
```

- 每个 run 用独立句柄：`engine_new()` + `engine_load(h, seed)`，扫完 `engine_drop`。
- 步骤天然串行（`wfDrive` 里是 `await`），正好满足"不并发、不触发限流"。
- `wfDrive` 现有步数上限 `guard > 16`：一页 6 个模型足够；若将来页大小上调，需同步放宽。

### 6.3 插件 `wf.model_probe`

```js
"wf.model_probe": async ({ model }) => {
  // 1) 先查本地缓存：TTL 内直接返回，不发请求（对应"2 小时内不扫"）
  // 2) 配置级短路：渠道未启用 / 未配 Key → 不发请求，直接出结论（省额度）
  // 3) 否则调 POST /admin/api/models/test，把结果喂给 ModelAvailFSM
  // 4) 写回 localStorage 缓存（含 ts）
}
```

要点：

- 插件的返回值同时用于**状态机记录**与**渲染**，两处口径一致。
- 插件是纯"执行者"，**判定逻辑仍在 WASM 状态机里**（`avail_result` 决定落到 Ok/Blocked/Gone）。
- 整条 run 的终态由引擎给出，前端只监听终态后统一重绘一次（避免逐行抖动）。

### 6.4 为什么不用后端批量接口

| 方案 | 结论 |
|---|---|
| 新增 `POST /models/probe` 批量接口 | 不需要。工作流引擎已经承担了编排职责，再加一层是重复 |
| 前端裸 `for` 循环 | 也不用。循环、重试、终态判定都在引擎里，前端只做插件 |

后端零改动，只复用 v0.19.8 的 `POST /admin/api/models/test`。

---

## 7. 接入点（改哪里）

| 文件 | 位置 | 改动 |
|---|---|---|
| `engine/src/lib.rs` | 新增 `mod avail` + `mod avail_abi` | `ModelAvailFSM` + 上述 ABI |
| `engine/src/lib.rs` | `mod fsm_tests` | 8 条单测 |
| `src/engine_wasm.bin` | — | `scripts/build-engine.sh` 重编 |
| `src/admin_ui.html` | `WF_SEED_PROBE` + `wfDrive` 调用 | 运行时按当前页模型生成动态 seed |
| `src/admin_ui.html` | `WF_PLUGINS` | 新增 `wf.model_probe` 插件 |
| `src/admin_ui.html` | `openModel()` / `renderModelList()` | 触发扫描 + 括号渲染 + 样式 + 禁用点击 |
| `src/admin_ui.html` | `WF` 封装区 | `wfAvail*` 系列包装（读状态/原因/标签） |
| `src/admin_ui.html` | CSS | `.model-opt.blocked` / `.gone` / `.probing` / `.current` |
| `src/admin_ui.html` | 模型弹窗底部 | 「重新检测本页」按钮 + 上次检测时间 |
| `src/admin.rs` | — | **不改**（复用 `/models/test`） |

---

## 8. 边界与风险

| 风险 | 处理 |
|---|---|
| **额度消耗** | 每页最多 6 次单轮请求（`max_tokens: 64`），TTL 2 小时。以每天开 10 次页面算 ≈ 60 次探测/天，可接受 |
| **扫描引发 429 污染真实健康度** | 扫描结果**不写回**渠道 `health` / 降权账本，避免"自己把自己测挂" |
| **探测内容泄露用户数据** | 探测请求是固定文案，不含任何会话内容 |
| **模型被删但缓存还在** | 渲染前按当前模型列表过滤，孤儿条目自动丢弃 |
| **用户点正在探测的模型** | 不阻塞，正常发送；状态回填后仅更新行样式 |
| **多标签页同时扫** | 各自独立，`localStorage` 后写覆盖，无害 |
| **移动端后台切换** | `document.hidden` 时不扫；回到前台若已过期则补扫 |
| **老缓存格式变更** | 键名带版本 `gw_avail_v1`，升版即整体失效，不做迁移 |

---

## 9. 验收标准

1. 打开模型页，当前页 6 个模型**逐个**出现 `（检测中…）`，随后各自变成结论。
2. 渠道未启用的模型显示 `（渠道未启用）`，行变暗 + 左侧竖条，**不发起网络请求**。
3. 未配 Key 的显示 `（未配置 Key）`，同样不发请求。
4. 可用的模型**不加任何括号**，样式与现在完全一致。
5. 关闭弹窗后 1 小时内再打开：**零网络请求**（Network 面板可验证），结论立即呈现。
6. 把系统时间调后 2 小时再打开：重新扫描。
7. 上游换家的模型显示 `（上游实为 xxx）`，但行仍为可用样式。
8. **受限行点击无任何选择动作**，仅弹一次原因 toast；`cursor: not-allowed`。
9. **已选中的模型扫描后变受限**：选择不被清除，顶部出现警示条。
10. 扫描是**一条工作流 run**，步骤数 == 本页模型数（Network/Console 可核对），不并发。
11. `cargo test` 全绿（含 8 条新增单测）。
12. 扫描期间正在流式回答时，不触发扫描。

---

## 10. 决策记录（已确认）

| # | 问题 | 结论 |
|---|---|---|
| 1 | 受限（`Blocked`）的模型还允许点选吗？ | **不允许**。`Blocked` / `Gone` 均不可点选；已选中的模型变受限时保留选择但顶部警示 |
| 2 | 扫描结果要不要回写渠道健康账本？ | **不要**。扫描是诊断，不影响真实路由与降权 |
| 3 | 扫描怎么编排？ | **走工作流引擎任务**：动态 seed + 每模型一步 + `wf.model_probe` 插件，复用 `/models/test`，后端零改动 |
| 4 | 2 小时 TTL 要不要做成可配置？ | 固定常量（本说明第 4.4 节） |
| 5 | 「自动」这一项要扫吗？ | 不扫，它不是具体模型 |
| 6 | 通话页的模型选择要不要也扫？ | v1 不做，先只做聊天页 |
| 7 | 是否加「重新检测本页」手动按钮？ | 加，绕过 TTL |
| 8 | 括号里要不要显示延迟？ | 不显示，括号只放"为什么不能用" |

---

## 11. 工作量拆解（建议顺序）

| 阶段 | 内容 | 产出 |
|---|---|---|
| 1 | 引擎 `ModelAvailFSM` + ABI + 8 条单测 | `cargo test` 全绿，`engine_wasm.bin` 重编 |
| 2 | 工作流任务：`WF_SEED_PROBE` 动态 seed + `wf.model_probe` 插件 | 一条 run 扫完本页，串行、可重试、有终态 |
| 3 | 前端缓存层（TTL 2h / 容量淘汰 / 跳过条件） | 2 小时规则可验证 |
| 4 | 渲染层：括号文案 + 四种行样式 + 受限行禁点 + 顶部警示 + 重测按钮 | 视觉验收 |
| 5 | 版本号 v0.19.10 + CHANGES + 重建 Worker | 可部署 |

---

## 附：与已有状态机的分工

| 状态机 | 管什么 | 何时用 |
|---|---|---|
| `MemoryFSM`（v0.19.5） | 会话上下文窗口（token 预算、摘要、裁剪） | 每次发送前 |
| `ModelSelFSM`（v0.19.6） | 这一次请求「所选 vs 实际」 | 每次收到响应后 |
| **`ModelAvailFSM`（本版）** | 模型「现在能不能用、为什么不能用」 | 打开模型页时 |

三者互不重叠：前者管历史，中者管本次，后者管选择前的候选质量。
