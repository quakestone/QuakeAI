# Render / Railway / Fly.io 等容器云部署（Docker Runtime）。
# 项目是 Rust→WASM 的 Cloudflare Worker（cdylib，无 bin target）——
# 在这些平台上**不能用 Rust Runtime / cargo run**（会报
# "a bin target must be available for cargo run"）。必须选 Docker Runtime。
#
# 镜像里不带 Rust 工具链：build/ 两个部署产物（index.js + index_bg.wasm）
# 已随仓库提交，直接 embed 进 workerd 即可。
FROM node:22-slim

WORKDIR /app
COPY build/ build/
COPY deploy/start.sh /app/start.sh

# workerd 官方 Linux 二进制（npm 分发）+ 系统 CA（上游 API 均为 https，必须信任根证书）
RUN chmod +x /app/start.sh \
 && apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && npm install --omit=dev --no-audit --no-fund @cloudflare/workerd-linux-64 \
 && node_modules/@cloudflare/workerd-linux-64/bin/workerd --version

# Render Docker 服务默认注入 PORT（10000）；本地跑可 -e PORT=8787
ENV PORT=10000 \
    STORAGE_DIR=/data \
    ZHIPU_BASE_URL=https://open.bigmodel.cn/api/paas/v4

RUN mkdir -p /data
VOLUME /data
EXPOSE 10000

ENTRYPOINT ["/app/start.sh"]
