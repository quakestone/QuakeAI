# Linux 自托管方案（workerd + systemd）

在不依赖 Cloudflare 的 Linux 机器（内网/自有服务器）上跑同一份网关：

```
┌─ Internet ─┐   ┌─ Linux 主机 ──────────────────────────────┐
│  客户端     │──▶│ Caddy(:443, 自动 TLS)                    │
└────────────┘   │   └─ 反代 → 127.0.0.1:8787                │
                 │ workerd serve workerd.config.capnp         │
                 │   · WASM 网关（build/index.js + wasm）      │
                 │   · ResourceStore DO（localDisk 持久化）    │
                 │ systemd: quakeai-gateway.service           │
                 │   Restart=always / 看门狗 curl /health      │
                 └────────────────────────────────────────────┘
```

**部署步骤**

1. 放产物：`build/index.js` + `build/index_bg.wasm` + `workerd.config.capnp`
   （GitHub 仓库已有，`git clone` 即得，**运行机零编译**）。
2. 下载 workerd：`curl -sL registry.npmjs.org/@cloudflare/workerd-linux-64/-/workerd-linux-64-<ver>.tgz | tar xz`，
   二进制放 `/usr/local/bin/workerd`。
3. 配置改两点：`durableObjectStorage` 由 `inMemory` 改 `localDisk`
   （实验特性，跨重启持久；注意 workerd idle 逐出，保留 5s alarm 保活）；
   socket 绑 `127.0.0.1:8787`（不直接暴露）。
4. systemd 单元：`ExecStart=/usr/local/bin/workerd serve /opt/quakeai/workerd.config.capnp`，
   `Restart=always`；健康检查用 `curl -fs http://127.0.0.1:8787/health`。
5. 反代终结 TLS：Caddy `reverse_proxy 127.0.0.1:8787`（自动证书），
   防火墙只放 443/80。
6. 密钥：`ZHIPU_API_KEY` / `ADMIN_TOKEN` 不进仓库——写进
   `/etc/quakeai/env`（chmod 600），systemd `EnvironmentFile` 引入；
   各资源 Key 仍在 `/admin` 页面登记（明文只进 DO storage）。
7. 升级：`git pull && bash scripts/build.sh && systemctl restart quakeai-gateway`
   （或 GitHub Actions SSH 部署，见 README「联动发布」）。

**与 Cloudflare 路线取舍**：Linux 自托管 = 数据完全自主、无 Workers 免费层
限制（body 100MB、DO 额度），但要自己管 TLS/可用性；Cloudflare 路线 =
零运维、全球边缘，DO 存储天然持久。两者同一份产物，`workerd.config.capnp`
换 socket/存储即可互换。
