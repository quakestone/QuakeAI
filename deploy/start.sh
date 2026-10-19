#!/usr/bin/env bash
# Render / Railway / Fly 等容器云的启动脚本：
# 从环境变量生成 workerd capnp 配置（workerd 不支持配置里读 env，只能启动时生成），
# 然后 exec workerd serve。
#
# 必读：产物 build/index.js + build/index_bg.wasm 已随仓库提交，容器里无需 Rust。
# 持久化：STORAGE_DIR（默认 /data）挂 Render Persistent Disk，否则重启丢 Key。
set -euo pipefail
cd "$(dirname "$0")"

PORT="${PORT:-10000}"
STORAGE_DIR="${STORAGE_DIR:-/data}"
ZHIPU_BASE_URL="${ZHIPU_BASE_URL:-https://open.bigmodel.cn/api/paas/v4}"

# capnp 文本值需要转义 \ 和 "
esc() { local s="$1"; s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; printf '%s' "$s"; }

KEY=$(esc "${ZHIPU_API_KEY:-}")
TOKEN=$(esc "${ADMIN_TOKEN:-}")
BASE=$(esc "$ZHIPU_BASE_URL")
AUDIO_MODEL=$(esc "${ZHIPU_AUDIO_MODEL:-}")
CHANNELS=$(esc "${ZHIPU_CHANNELS:-}")

mkdir -p "$STORAGE_DIR" 2>/dev/null || true
if ! [ -w "$STORAGE_DIR" ]; then
  echo "⚠️  STORAGE_DIR=$STORAGE_DIR 不可写，退回 ./wd-storage（容器重启会丢登记的渠道/Key，请挂载持久盘）" >&2
  STORAGE_DIR="$PWD/wd-storage"
  mkdir -p "$STORAGE_DIR"
fi

cat > /app/gw.render.capnp <<EOF
using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    ( name = "main", worker = .w ),
    ( name = "storage", disk = ( path = "$STORAGE_DIR", writable = true ) ),
    ( name = "internet", network = ( allow = ["public","private"], tlsOptions = ( trustBrowserCas = true ) ) ),
  ],
  sockets = [ ( name = "http", address = "*:$PORT", http = (), service = "main" ) ],
);

const w :Workerd.Worker = (
  modules = [
    ( name = "index.js", esModule = embed "build/index.js" ),
    ( name = "index_bg.wasm", wasm = embed "build/index_bg.wasm" ),
  ],
  compatibilityDate = "2026-10-01",
  durableObjectNamespaces = [ ( className = "ResourceStore", uniqueKey = "gw-resource-store-key" ) ],
  durableObjectStorage = ( localDisk = "storage" ),
  bindings = [
    ( name = "ZHIPU_API_KEY", text = "$KEY" ),
    ( name = "ZHIPU_BASE_URL", text = "$BASE" ),
    ( name = "ADMIN_TOKEN", text = "$TOKEN" ),
    ( name = "ZHIPU_AUDIO_MODEL", text = "$AUDIO_MODEL" ),
    ( name = "ZHIPU_CHANNELS", text = "$CHANNELS" ),
    ( name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore" ),
  ],
);
EOF

# 注意：embed 路径相对 capnp 文件所在目录解析，必须与 /app/build/ 同层
exec ./node_modules/@cloudflare/workerd-linux-64/bin/workerd serve /app/gw.render.capnp
