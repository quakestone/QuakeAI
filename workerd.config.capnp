# 本地 workerd 配置（e2e 默认）：BASE_URL 指向 mock 智谱（127.0.0.1:8788），
# DO 绑定 RESOURCE_STORE / ResourceStore。
# storage 用 localDisk（.wd-storage/）而非 inMemory——否则每重启一次进程，
# 管理页登记的渠道/Key 全部丢失（"升级后 key 全没了"就是它）。目录已 gitignore。
using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    ( name = "main", worker = .w ),
    ( name = "storage", disk = ( path = ".wd-storage", writable = true ) ),
    ( name = "internet", network = ( allow = ["public","private"], tlsOptions = ( trustBrowserCas = true ) ) ),
  ],
  sockets = [ ( name = "http", address = "*:8787", http = (), service = "main" ) ],
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
    ( name = "ZHIPU_API_KEY", text = "test-key-local" ),
    ( name = "ZHIPU_BASE_URL", text = "http://127.0.0.1:8788" ),
    ( name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore" ),
  ],
);
