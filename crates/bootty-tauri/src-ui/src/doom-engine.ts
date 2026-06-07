import type { EmbeddedDoomAssets } from "./doom-embedded-assets";

export type DoomAssetUrls = {
  glueUrl: string;
  wasmUrl: string;
  wadUrl: string;
};

type DoomModuleFactory = (config: DoomModuleConfig) => Promise<DoomModule>;

type DoomModuleConfig = {
  wasmBinary: Uint8Array;
  instantiateWasm?(
    imports: WebAssembly.Imports,
    successCallback: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void,
  ): void;
  locateFile(path: string): string;
  print(message: string): void;
  printErr(message: string): void;
  preRun: Array<(module: DoomModule) => void>;
};

type DoomModule = {
  _doomgeneric_Create(argc: number, argv: number): void;
  _doomgeneric_Tick(): void;
  _DG_GetFrameBuffer(): number;
  _DG_GetScreenWidth(): number;
  _DG_GetScreenHeight(): number;
  _DG_PushKeyEvent(pressed: number, key: number): void;
  _malloc(size: number): number;
  _free(ptr: number): void;
  HEAPU32: Uint32Array;
  FS_createPath(parent: string, path: string, canRead: boolean, canWrite: boolean): string;
  FS_createDataFile(parent: string, name: string, data: ArrayLike<number>, canRead: boolean, canWrite: boolean): void;
  setValue(ptr: number, value: number, type: string): void;
};

export class BrowserDoomEngine {
  private module: DoomModule | null = null;
  private frameBufferPtr = 0;
  private frameBuffer = new Uint8Array();
  private frameBufferWords: Uint32Array | null = null;
  private initialized = false;
  private screenWidth = 640;
  private screenHeight = 400;

  constructor(
    private readonly assets: DoomAssetUrls | EmbeddedDoomAssets,
    private readonly onStatus: (status: string) => void = () => {},
  ) {}

  get width(): number {
    return this.screenWidth;
  }

  get height(): number {
    return this.screenHeight;
  }

  async init(): Promise<void> {
    const { glueCode, wasmBinary, wadData } = await this.loadAssets();
    this.onStatus("loading doom wasm glue");
    const createDoomModule = loadDoomModuleFactory(glueCode);
    this.onStatus("instantiating doom wasm");
    const module = await createDoomModule({
      wasmBinary,
      instantiateWasm: (imports, successCallback) => {
        this.onStatus(`compiling doom wasm ${wasmBinary.byteLength} bytes`);
        const wasmBuffer = wasmBinary.buffer.slice(
          wasmBinary.byteOffset,
          wasmBinary.byteOffset + wasmBinary.byteLength,
        ) as ArrayBuffer;
        WebAssembly.instantiate(wasmBuffer, imports)
          .then((result) => {
            this.onStatus("doom wasm compiled");
            successCallback(result.instance, result.module);
          })
          .catch((error: unknown) => {
            this.onStatus(error instanceof Error ? error.message : String(error));
          });
      },
      locateFile: (path) => (path.endsWith(".wasm") && "wasmUrl" in this.assets ? this.assets.wasmUrl : path),
      print: () => {},
      printErr: (message) => console.debug(`[doom] ${message}`),
      preRun: [
        (preRunModule) => {
          preRunModule.FS_createPath("/", "doom", true, true);
          preRunModule.FS_createDataFile("/doom", "doom1.wad", wadData, true, false);
        },
      ],
    });

    this.module = module;
    this.onStatus("creating doom process");
    this.createDoomProcess(module);
    this.frameBufferPtr = module._DG_GetFrameBuffer();
    this.screenWidth = module._DG_GetScreenWidth();
    this.screenHeight = module._DG_GetScreenHeight();
    this.frameBuffer = new Uint8Array(this.screenWidth * this.screenHeight * 4);
    this.frameBufferWords = new Uint32Array(module.HEAPU32.buffer, this.frameBufferPtr, this.screenWidth * this.screenHeight);
    this.initialized = true;
  }

  tick(): void {
    if (this.module && this.initialized) {
      this.module._doomgeneric_Tick();
    }
  }

  getFrameRGBA(): Uint8Array {
    if (!this.initialized || !this.frameBufferWords) {
      return this.frameBuffer;
    }

    for (let pixel = 0; pixel < this.frameBufferWords.length; pixel += 1) {
      const argb = this.frameBufferWords[pixel] ?? 0;
      const offset = pixel * 4;
      this.frameBuffer[offset] = (argb >> 16) & 0xff;
      this.frameBuffer[offset + 1] = (argb >> 8) & 0xff;
      this.frameBuffer[offset + 2] = argb & 0xff;
      this.frameBuffer[offset + 3] = 255;
    }
    return this.frameBuffer;
  }

  pushKey(pressed: boolean, key: number): void {
    if (this.module && this.initialized) {
      this.module._DG_PushKeyEvent(pressed ? 1 : 0, key);
    }
  }

  private createDoomProcess(module: DoomModule): void {
    const args = ["doom", "-iwad", "/doom/doom1.wad"];
    const argPtrs = args.map((arg) => writeCString(module, arg));
    const argvPtr = module._malloc(argPtrs.length * 4);
    for (let index = 0; index < argPtrs.length; index += 1) {
      module.setValue(argvPtr + index * 4, argPtrs[index] ?? 0, "i32");
    }

    module._doomgeneric_Create(args.length, argvPtr);

    for (const ptr of argPtrs) {
      module._free(ptr);
    }
    module._free(argvPtr);
  }

  private async loadAssets(): Promise<EmbeddedDoomAssets> {
    if ("glueCode" in this.assets) {
      this.onStatus("decoding embedded doom assets");
      return this.assets;
    }

    this.onStatus("fetching doom.js");
    const glueCode = await fetchText(this.assets.glueUrl, this.onStatus);
    this.onStatus("fetching doom.wasm");
    const wasmBinary = await fetchBytes(this.assets.wasmUrl, this.onStatus);
    this.onStatus("fetching WAD");
    const wadData = await fetchBytes(this.assets.wadUrl, this.onStatus);
    return { glueCode, wasmBinary, wadData };
  }
}

async function fetchText(url: string, onStatus: (status: string) => void): Promise<string> {
  const bytes = await fetchBytes(url, onStatus);
  return new TextDecoder().decode(bytes);
}

async function fetchBytes(url: string, onStatus: (status: string) => void): Promise<Uint8Array> {
  const request = new XMLHttpRequest();
  onStatus(`opening ${assetName(url)}`);
  request.open("GET", url, false);
  request.responseType = "arraybuffer";
  onStatus(`reading ${assetName(url)}`);
  request.send();
  if (request.status < 200 || request.status >= 300) {
    throw new Error(`Failed to fetch ${url}: ${request.status} ${request.statusText}`);
  }
  return new Uint8Array(request.response as ArrayBuffer);
}

function assetName(url: string): string {
  return url.split("/").pop() || url;
}

function loadDoomModuleFactory(glueCode: string): DoomModuleFactory {
  const browserGlue = glueCode.replace("var ENVIRONMENT_IS_NODE=true;", "var ENVIRONMENT_IS_NODE=false;");
  return new Function(`${browserGlue}; return createDoomModule;`)() as DoomModuleFactory;
}

function writeCString(module: DoomModule, value: string): number {
  const ptr = module._malloc(value.length + 1);
  for (let index = 0; index < value.length; index += 1) {
    module.setValue(ptr + index, value.charCodeAt(index), "i8");
  }
  module.setValue(ptr + value.length, 0, "i8");
  return ptr;
}
