#!/usr/bin/env bash
# 合成 Dashboard 网页粘贴用的单文件版本：deploy/single-file-worker.js
# （wasm base64 内联，无需多模块上传）。
#
# 原理：worker-build 生成的 build/index.js 是 ES module，其中对 wasm 的引用
# 形如 `import X from './index_bg.wasm'`（多模块部署时 workerd 直接提供编译好的
# WebAssembly.Module）。单文件版删掉该 import，注入同名 const：
#   const X = new WebAssembly.Module(__wasmBytes());
# 其中 __wasmBytes() 由内联 base64 还原 —— 语义与多模块版完全一致。
# 最后追加 ResourceStore DO 类（若构建时未追加）。
set -euo pipefail
cd "$(dirname "$0")/.."

[ -f build/index.js ] && [ -f build/index_bg.wasm ] || {
  echo "缺少 build/index.js 或 build/index_bg.wasm —— 先跑 bash scripts/build.sh"; exit 1;
}

mkdir -p deploy

python3 - <<'PYEOF'
import base64, re, sys

wasm_b64 = base64.b64encode(open("build/index_bg.wasm", "rb").read()).decode()
index_js = open("build/index.js", encoding="utf-8").read()

# 1) 删除对 ./index_bg.wasm 的 import，并记住其标识符（字节已内联）。
#    兼容：压缩/未压缩产物、默认导入（import X from "..."）、
#    命名空间导入（import * as wasm from "..."）、from 后有无空格。
pat = re.compile(
    r'import\s+(?:\*\s+as\s+(\w+)|(\w+))\s+from\s*["\']\./index_bg\.wasm["\'];?'
)
m = pat.search(index_js)
if not m:
    sys.exit("未找到 ./index_bg.wasm 的 import 语句 —— glue 格式可能变了，请更新 make-single-file.sh")
wasm_ident = m.group(1) or m.group(2)
index_js = index_js[: m.start()] + index_js[m.end():]

# 2) 顶部注入与导入同名的 const：用内联字节同步编译出 WebAssembly.Module。
#    （workerd/Workers 均支持同步 new WebAssembly.Module； glue 里对该标识符的
#      所有引用——典型为 new WebAssembly.Instance(X, imports)——随之全部生效）
index_js = (
    "const %s = new WebAssembly.Module(__wasmBytes());\n" % wasm_ident
) + index_js

# 3) 组装：注释头 + base64 数据 + 改造后的 glue
preamble = '''// ===========================================================================
// 自动生成：zhipu-wasm-gateway 单文件部署版（wasm 已 base64 内联）。
// 由 scripts/make-single-file.sh 生成，请勿手改；改代码请走 scripts/build.sh。
// 用法：Cloudflare Dashboard -> Workers -> Create/Edit code -> 全量粘贴 -> Deploy。
// ===========================================================================
function __wasmBytes(){
  return Uint8Array.from(atob(WASM_B64), c => c.charCodeAt(0));
}
const WASM_B64 =
"%s";
''' % wasm_b64

out = preamble + index_js

# 4) 如果 glue 里还没有 ResourceStore（build.sh 未追加过），这里补上
if "class ResourceStore extends DurableObject" not in out:
    out += "\n" + open("scripts/storage-do.js", encoding="utf-8").read()

open("deploy/single-file-worker.js", "w", encoding="utf-8").write(out)
print("deploy/single-file-worker.js: %d bytes" % len(out.encode()))
PYEOF
