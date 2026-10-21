/* tslint:disable */
/* eslint-disable */

/**
 * 管理面：method / 子路径(x-gw-sub 头) / 请求体 / admin token 全部从 Request 内读。
 */
export function admin(req: Request): Promise<Response>;

/**
 * 语音合成：Request 的 body 是 JSON 文本。
 */
export function audio_speech(req: Request): Promise<Response>;

/**
 * 语音转写：Request 的 body 是浏览器 FormData。
 */
export function audio_transcriptions(req: Request): Promise<Response>;

/**
 * 对话：前端把整段 fetch 的 Request 直接传进来，WASM 内用原生 web_sys 读
 * 请求体 + x-gw-features / x-device-keys 头。
 */
export function chat(req: Request): Promise<Response>;

/**
 * 健康检查（同步）：直接返回 JSON Response。
 * 注意：浏览器内 WASM 函数若返回 String，在部分运行时会被解成 [ptr,len] 数组，
 * 故这里返回 web_sys::Response（与 admin/chat 同范式），由调用方直接透传。
 */
export function health(): Response;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly admin: (a: any) => any;
    readonly audio_speech: (a: any) => any;
    readonly audio_transcriptions: (a: any) => any;
    readonly chat: (a: any) => any;
    readonly health: () => any;
    readonly wasm_bindgen__convert__closures_____invoke__h01c5e3eaf350a4d0: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h84cbb50b2f88d044: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__ha532af14353132eb: (a: number, b: number, c: any) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
