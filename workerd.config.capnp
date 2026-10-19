# 本地 workerd 配置（e2e 默认）：BASE_URL 指向 mock 智谱（127.0.0.1:8788），
# DO 绑定 RESOURCE_STORE / ResourceStore（inMemory storage）。
using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    ( name = "main", worker = .w ),
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
  durableObjectStorage = ( inMemory = void ),
  bindings = [
    ( name = "ZHIPU_API_KEY", text = "test-key-local" ),
    ( name = "ZHIPU_BASE_URL", text = "http://127.0.0.1:8788" ),
    ( name = "RESOURCE_STORE", durableObjectNamespace = "ResourceStore" ),
  ],
);
