/* tslint:disable */
/* eslint-disable */

export class SiteBackend {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    frame(): any;
    input(input: string): any;
    mouse(kind: string, x: number, y: number, _button: number): any;
    static new(): SiteBackend;
    resize(cols: number, rows: number): any;
    set_fps(fps: number): any;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_sitebackend_free: (a: number, b: number) => void;
    readonly sitebackend_frame: (a: number) => [number, number, number];
    readonly sitebackend_input: (a: number, b: number, c: number) => [number, number, number];
    readonly sitebackend_mouse: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly sitebackend_new: () => number;
    readonly sitebackend_resize: (a: number, b: number, c: number) => [number, number, number];
    readonly sitebackend_set_fps: (a: number, b: number) => [number, number, number];
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
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
