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

### 已知限制（后续批次处理）

- 对话侧 skills 三流注入、附件、魔法指令、调试浮层 → B1。
- 服务商/模型管理页、用量对账、一键导入内置通道 → B2。
- 语音 TTS/STT 递降、工作流参数 → B3；上游/接入/鉴权/本机调试 → B4。
- 旧 UI（`src/admin_ui.html`）仍在 main/gh-pages 线上运行，本分支验收后（B7）才删除——当前两套并存属计划内的中间态。

## 复现命令

```bash
bash scripts/build.sh          # 构建
bash scripts/test-ui-g0.sh     # G0 一键实测（构建+托管+mock+Playwright 28 断言）
```
