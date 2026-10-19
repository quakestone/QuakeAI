# 部署到 Cloudflare（没有本地 Rust 也能发）

## 先消除两个误解

1. **「Cloudflare 只支持单文件新建」——只对 Dashboard 网页编辑器成立。**
   官方 CLI（wrangler）和 CI 都支持多模块上传，本项目是一个 Worker + 一个
   wasm 模块（`build/index.js` + `build/index_bg.wasm`），本来就要多模块。
2. **「PC 上没有 Rust 环境」——部署从来不需要 Rust。**
   Rust 只在「改了 `src/*.rs` 之后重新编译」时才需要。仓库里的 `build/`
   已经是编译好的最终产物（部署态），拿到任何一台有 Node 的电脑，甚至
   一台都没有都能上线。

> 本文三条路线按「推荐度」排序：
> **A. wrangler CLI（推荐）** → **B. Dashboard 粘贴单文件（零安装）** → **C. GitHub Actions（零本地环境）**

---

## 路线 A：wrangler CLI（推荐）

需要：Node.js 18+（`node -v` 验证）。不需要 Rust。

```bash
cd zhipu-wasm-gateway

# 1. 登录（弹出浏览器授权）
npx --yes wrangler@latest login

# 2. 配密钥（会提示粘贴，只存 Cloudflare Secret，不进仓库）
npx --yes wrangler@latest secret put ZHIPU_API_KEY     # 智谱 API Key
npx --yes wrangler@latest secret put ADMIN_TOKEN       # 管理页鉴权_token，可选

# 3. 一键部署
npx --yes wrangler@latest deploy
```

部署时 wrangler 会：

- 上传 Worker 主模块 `build/index.js` + 它 import 的 `build/index_bg.wasm`
  （多模块，**没有**「单文件」限制）；
- 自动应用 `wrangler.toml` 里的 `[[migrations]] tag = "v1"`，
  创建 `ResourceStore` Durable Object（资源账本，存渠道/Key/事件）；
- 生成地址 `https://zhipu-wasm-gateway.<你的子域>.workers.dev`。

验证：

```bash
curl https://zhipu-wasm-gateway.<你的子域>.workers.dev/health
# → {"status":"ok",...}

curl https://zhipu-wasm-gateway.<你的子域>.workers.dev/admin
# → 资源管理页面
```

> **改 Rust 代码后**：重新编译（见文末「什么时候才需要 Rust」），再跑一次
> `npx wrangler deploy`。
>
> **换机器/换人部署**：不需要 Rust、不需要本地构建，只要 `git clone` 拉下
> 仓库（`build/` 里两个产物模块随仓库提交），重复上面三条命令即可。

---

## 路线 B：Dashboard 网页粘贴（零安装，什么都不用装）

适合：电脑上连 Node 都不想装。**单文件版本已由本项目生成并本地验证过**：
`deploy/single-file-worker.js`（598KB，wasm 已 base64 内联，gzip 约 214KB，
低于免费层 1MB 压缩上限）。

1. Cloudflare 面板 → **Workers & Pages** → **Create** → **Start with Hello
   World** → 名字随意（如 `ai-gw`）→ **Deploy** → **Edit code**。
2. 编辑器里 **Ctrl/Cmd+A 全选 → 删除**，把 `deploy/single-file-worker.js`
   **整个文件内容粘贴进去**（540KB，粘贴时浏览器会卡 1~3 秒，正常），
   点 **Deploy**。
3. 进 worker 的 **Settings → Variables and Secrets → Add**：

   | 变量 | 类型 | 值 |
   |---|---|---|
   | `ZHIPU_BASE_URL` | Text | `https://open.bigmodel.cn/api/paas/v4` |
   | `ZHIPU_API_KEY` | Secret | 你的智谱 API Key |
   | `ADMIN_TOKEN` | Secret | 自定义管理口令（可选但建议） |

4. **Settings → Bindings → Add → Durable Object**：

   | 字段 | 值 |
   |---|---|
   | Variable name | `RESOURCE_STORE` |
   | Class name | `ResourceStore` |

   （如果下拉里没有 `ResourceStore`，等上一步 Deploy 完成后刷新页面再试。）
5. 再点一次 **Deploy**。
6. 打开 `https://ai-gw.<你的子域>.workers.dev/admin`：
   **一键载入国内默认** → 在渠道行点「登记」填各资源的真实 Key → 启用。

> 粘贴被浏览器卡死/截断的，改走路线 A。
> 路线 B 不便于频繁迭代（每次都是整文件覆盖粘贴），日常开发还是 A/C。

---

## 路线 C：GitHub Actions 云端部署（本地零环境）

把仓库推到 GitHub（**`build/index.js` + `build/index_bg.wasm` 两个产物
模块必须一起提交**——`.gitignore` 已放行这两个文件，其余产物忽略），
仓库里已带 `.github/workflows/deploy.yml`，push 到 `main` 即自动部署。

在你的 GitHub 仓库 **Settings → Secrets and variables → Actions** 加：

| Secret | 值 |
|---|---|
| `CLOUDFLARE_API_TOKEN` | API Token，权限模板选 **Edit Cloudflare Workers** |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare 面板右侧栏的 Account ID |

之后 `git push`（或在 Actions 页手动 Run workflow）就会自动
`npx wrangler deploy`。本地 Node、Rust 全都不用装。

> 以后想让 CI **顺便重新编译 Rust**（改了源码时）：在 deploy 步骤前加一步
> `dtolnay/rust-toolchain@stable` + `rustup target add
> wasm32-unknown-unknown` + `cargo install worker-build` 即可；或者把改好的
> 代码发我，我编译好回传 `build/`。

---

## 什么时候才真正需要 Rust 环境

只有一种情况：你修改了 `src/*.rs`（网关逻辑本身）。

| 方式 | 操作 |
|---|---|
| 本机装 | `curl --proto '=https' --tls-version 1.2 -sSf https://sh.rustup.rs \| sh`，然后 `rustup target add wasm32-unknown-unknown` |
| CI 里编译 | 扩展路线 C 的 workflow（见上） |
| 外包给我 | 把要改的点说清楚，我编译好把 `build/` 两个文件回传 |

只改「前端页面样式 / 预设目录 / 渠道数据」这类同样要重新编译（都在
`src/admin_ui.html` 和 `src/channels.rs` 里），没有纯 JS 热改路径——
但改完只需要把编译产物传上去，运行环境永远不需要 Rust。

---

## 已验证（v0.4 交付前专门做过的验证）

- ✅ **多文件构建 mock e2e 64/64**：`bash scripts/test-e2e.sh`
- ✅ **单文件版本（路线 B 的产物）本地 workerd 单模块实测**：
  `/health`、`/admin` 管理页（类 ChatGPT 布局 + 模型管理二级子页）、seed 国内默认、
  渠道登记/删除、**Key 掩码无明文**、chat 命中动态渠道
  （`x-gateway-provider: siliconflow`）、音频 multipart、事件账本记录
- ✅ **浏览器交互回归 17/17**（puppeteer，手机/桌面双视口）：抽屉、
  模型选择器、SSE 流式 + 渠道徽章 + 停止、二级子页进入/返回、
  登记弹层、掩码 Key 拦截、双视口无横向溢出
- ✅ 单文件合成脚本可复现：`bash scripts/make-single-file.sh`
  （每次重新编译后跑一次，产出最新的 `deploy/single-file-worker.js`）

## 生产注意

- 密钥（ZHIPU_API_KEY / ADMIN_TOKEN）用 Secret 形式存，不要写进仓库；
  DO 账本里的渠道 Key 是明文存 DO storage 的，更高安全要求用 Secret
  映射后在代码里注入。
- 免费层限制：Worker 脚本压缩后 1MB（本项目 gzip 214KB，余量充足）、
  请求体 100MB、DO 有读写行数免费额度（本项目的账本操作都在免费额度内）。

---

## 路线 D：部署到 Render / Railway / Fly.io 等容器云（Docker）

> ⚠️ **关键**：这些平台的 **Rust Runtime 会跑 `cargo run --release`**，而本项目是
> Rust→WASM 的 Worker（`crate-type = ["cdylib"]`，没有 bin target），必然报
> `error: a bin target must be available for 'cargo run'`（Exit 101）。
> **必须把服务类型切成 Docker（Runtime: Docker）**，仓库根已有现成 `Dockerfile`。

### Render 操作步骤

1. New + → Web Service → 连接 GitHub 仓库；
2. **Language / Runtime 选 `Docker`**（不要选 Rust）；
3. Environment Variables 加：
   - `ZHIPU_API_KEY` = 你的智谱 Key（或留空，上线后在管理页登记渠道 Key）
   - `ADMIN_TOKEN` = 管理页密码（强烈建议设置）
   - `ZHIPU_BASE_URL` 可选（默认 `https://open.bigmodel.cn/api/paas/v4`）
4. （推荐）Disks → Add Disk，挂载路径 `/data` —— **不挂的话每次重启/升级，
   管理页登记的渠道与 Key 全部丢失**（v0.7 的 localDisk DO 持久化写在 `/data`）；
5. Deploy。启动命令无需填，`deploy/start.sh` 会按 `PORT`/环境变量自动生成
   workerd 配置并起服务（Render Docker 默认 `PORT=10000`，已对齐）。

Railway / Fly.io 同理：Fly 用 `fly launch --dockerfile Dockerfile`，
Railway 直接识别根 `Dockerfile`。环境变量与 `/data` 卷语义一致。

### 已验证（本仓库 Docker 构建 + 运行实测）

- ✅ 镜像构建通过（node:22-slim + `@cloudflare/workerd-linux-64` + ca-certificates，无 Rust 工具链）
- ✅ `/health` 返回 `{"status":"ok","version":"0.7.0"}`、`/admin` 管理页 200
- ✅ 登记渠道 → `docker restart` → 渠道与 Key 掩码仍在（`/data` 持久化生效）
