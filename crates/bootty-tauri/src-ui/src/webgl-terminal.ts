import type { WebCell, WebColor, WebImage, WebImageLayer, WebTerminalFrame } from "./terminal-types";

type Rgba = [number, number, number, number];

type ImageTexture = {
  texture: WebGLTexture;
  width: number;
  height: number;
  revision?: number;
};

type Glyph = {
  u: number;
  v: number;
  w: number;
  h: number;
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
};

export class WebGlTerminalRenderer {
  private readonly gl: WebGL2RenderingContext;
  private readonly solidProgram: WebGLProgram;
  private readonly imageProgram: WebGLProgram;
  private readonly textProgram: WebGLProgram;
  private readonly solidBuffer: WebGLBuffer;
  private readonly imageBuffer: WebGLBuffer;
  private readonly textBuffer: WebGLBuffer;
  private readonly atlas: GlyphAtlas;
  private readonly imageTextures = new Map<string, ImageTexture>();
  private width = 0;
  private height = 0;
  private dpr = 1;

  constructor(private readonly canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
    });
    if (!gl) {
      throw new Error("WebGL2 is unavailable");
    }

    this.gl = gl;
    this.solidProgram = createProgram(gl, SOLID_VERTEX_SHADER, SOLID_FRAGMENT_SHADER);
    this.imageProgram = createProgram(gl, IMAGE_VERTEX_SHADER, IMAGE_FRAGMENT_SHADER);
    this.textProgram = createProgram(gl, TEXT_VERTEX_SHADER, TEXT_FRAGMENT_SHADER);
    this.solidBuffer = required(gl.createBuffer(), "create solid instance buffer");
    this.imageBuffer = required(gl.createBuffer(), "create image vertex buffer");
    this.textBuffer = required(gl.createBuffer(), "create text instance buffer");
    this.atlas = new GlyphAtlas(gl);
  }

  render(frame: WebTerminalFrame): void {
    this.resize(frame);

    const surfaceInstances: number[] = [];
    const backgroundInstances: number[] = [];
    const textInstances: number[] = [];
    const cursorInstances: number[] = [];

    pushSolidInstance(
      surfaceInstances,
      0,
      0,
      frame.cols * frame.cellWidth,
      frame.rows * frame.cellHeight,
      rgba(frame.colors.background),
    );

    for (const cell of frame.cells) {
      const colors = resolvedCellColors(frame, cell);
      if (colors.background) {
        pushSolidInstance(
          backgroundInstances,
          cell.x * frame.cellWidth,
          cell.y * frame.cellHeight,
          frame.cellWidth,
          frame.cellHeight,
          rgba(colors.background),
        );
      }
    }

    for (const cell of frame.cells) {
      if (cell.style.invisible || cell.text.length === 0) {
        continue;
      }

      const color = resolvedCellColors(frame, cell).foreground;
      if (pushBoxDrawingInstance(backgroundInstances, cell, frame, rgba(color))) {
        continue;
      }

      const glyph = this.atlas.glyph(cell.text, frame.cellWidth, frame.cellHeight, this.dpr, cell.style);
      pushTextInstance(
        textInstances,
        cell.x * frame.cellWidth + glyph.offsetX,
        cell.y * frame.cellHeight + glyph.offsetY,
        glyph.width,
        glyph.height,
        glyph,
        rgba(color),
      );
      if (cell.style.underline) {
        pushSolidInstance(
          backgroundInstances,
          cell.x * frame.cellWidth,
          cell.y * frame.cellHeight + frame.cellHeight - 2,
          frame.cellWidth,
          1,
          rgba(color),
        );
      }
    }

    if (frame.cursor) {
      pushSolidInstance(
        cursorInstances,
        frame.cursor.x * frame.cellWidth,
        frame.cursor.y * frame.cellHeight,
        frame.cellWidth,
        frame.cellHeight,
        rgba(frame.cursor.color ?? frame.colors.cursor ?? frame.colors.foreground, 0.8),
      );
    }

    this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    this.gl.disable(this.gl.DEPTH_TEST);
    this.gl.disable(this.gl.CULL_FACE);
    this.gl.clearColor(
      frame.colors.background.r / 255,
      frame.colors.background.g / 255,
      frame.colors.background.b / 255,
      1,
    );
    this.gl.clear(this.gl.COLOR_BUFFER_BIT);

    this.drawSolid(surfaceInstances);
    this.drawImages(frame, "belowBackground");
    this.drawSolid(backgroundInstances);
    this.drawImages(frame, "belowText");
    this.drawText(textInstances);
    this.drawImages(frame, "aboveText");
    this.drawSolid(cursorInstances);
    this.gl.flush();
  }

  dispose(): void {
    this.gl.deleteBuffer(this.solidBuffer);
    this.gl.deleteBuffer(this.imageBuffer);
    this.gl.deleteBuffer(this.textBuffer);
    this.gl.deleteProgram(this.solidProgram);
    this.gl.deleteProgram(this.imageProgram);
    this.gl.deleteProgram(this.textProgram);
    for (const image of this.imageTextures.values()) {
      this.gl.deleteTexture(image.texture);
    }
    this.imageTextures.clear();
    this.atlas.dispose();
  }

  private resize(frame: WebTerminalFrame): void {
    this.dpr = window.devicePixelRatio || 1;
    this.width = Math.max(1, frame.cols * frame.cellWidth);
    this.height = Math.max(1, frame.rows * frame.cellHeight);
    const backingWidth = Math.ceil(this.width * this.dpr);
    const backingHeight = Math.ceil(this.height * this.dpr);
    if (this.canvas.width !== backingWidth || this.canvas.height !== backingHeight) {
      this.canvas.width = backingWidth;
      this.canvas.height = backingHeight;
    }
    this.canvas.style.width = `${this.width}px`;
    this.canvas.style.height = `${this.height}px`;
  }

  private drawSolid(instances: number[]): void {
    const count = instances.length / SOLID_INSTANCE_FLOATS;
    if (count === 0) {
      return;
    }

    const gl = this.gl;
    gl.useProgram(this.solidProgram);
    gl.uniform2f(
      required(gl.getUniformLocation(this.solidProgram, "u_resolution"), "solid resolution"),
      this.width,
      this.height,
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, this.solidBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(instances), gl.STREAM_DRAW);
    bindInstancedAttribute(gl, this.solidProgram, "a_rect", 4, SOLID_INSTANCE_FLOATS, 0);
    bindInstancedAttribute(gl, this.solidProgram, "a_color", 4, SOLID_INSTANCE_FLOATS, 4);
    gl.disable(gl.BLEND);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
  }

  private drawImages(frame: WebTerminalFrame, layer: WebImageLayer): void {
    const images = frame.images.filter((image) => image.layer === layer);
    if (images.length === 0) {
      return;
    }

    const gl = this.gl;
    gl.useProgram(this.imageProgram);
    gl.uniform2f(
      required(gl.getUniformLocation(this.imageProgram, "u_resolution"), "image resolution"),
      this.width,
      this.height,
    );
    gl.uniform1i(required(gl.getUniformLocation(this.imageProgram, "u_image"), "image sampler"), 0);
    const pixelFormatLocation = required(gl.getUniformLocation(this.imageProgram, "u_pixel_format"), "image pixel format");
    gl.bindBuffer(gl.ARRAY_BUFFER, this.imageBuffer);
    bindAttribute(gl, this.imageProgram, "a_position", 2, IMAGE_VERTEX_FLOATS, 0, false);
    bindAttribute(gl, this.imageProgram, "a_uv", 2, IMAGE_VERTEX_FLOATS, 2, false);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

    for (const image of images) {
      const vertices = imageVertices(image);
      if (vertices.length === 0) {
        continue;
      }
      const texture = this.imageTexture(image);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, texture.texture);
      gl.uniform1i(pixelFormatLocation, image.pixelFormat === "bgrx" ? 1 : 0);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(vertices), gl.STREAM_DRAW);
      gl.drawArrays(gl.TRIANGLES, 0, vertices.length / IMAGE_VERTEX_FLOATS);
    }

    gl.disable(gl.BLEND);
  }

  private imageTexture(image: WebImage): ImageTexture {
    const cached = this.imageTextures.get(image.key);
    if (cached && cached.width === image.imageWidth && cached.height === image.imageHeight) {
      if (image.revision == null || cached.revision !== image.revision) {
        this.updateImageTexture(cached.texture, image);
        cached.revision = image.revision;
      }
      return cached;
    }
    if (cached) {
      this.gl.deleteTexture(cached.texture);
    }

    const texture = required(this.gl.createTexture(), "create image texture");
    this.configureImageTexture(texture);
    this.uploadImageTexture(texture, image);
    const stored = { texture, width: image.imageWidth, height: image.imageHeight, revision: image.revision };
    this.imageTextures.set(image.key, stored);
    return stored;
  }

  private configureImageTexture(texture: WebGLTexture): void {
    this.gl.bindTexture(this.gl.TEXTURE_2D, texture);
    this.gl.texParameteri(this.gl.TEXTURE_2D, this.gl.TEXTURE_MIN_FILTER, this.gl.NEAREST);
    this.gl.texParameteri(this.gl.TEXTURE_2D, this.gl.TEXTURE_MAG_FILTER, this.gl.NEAREST);
    this.gl.texParameteri(this.gl.TEXTURE_2D, this.gl.TEXTURE_WRAP_S, this.gl.CLAMP_TO_EDGE);
    this.gl.texParameteri(this.gl.TEXTURE_2D, this.gl.TEXTURE_WRAP_T, this.gl.CLAMP_TO_EDGE);
  }

  private uploadImageTexture(texture: WebGLTexture, image: WebImage): void {
    this.gl.bindTexture(this.gl.TEXTURE_2D, texture);
    this.gl.pixelStorei(this.gl.UNPACK_ALIGNMENT, 1);
    this.gl.texImage2D(
      this.gl.TEXTURE_2D,
      0,
      this.gl.RGBA,
      image.imageWidth,
      image.imageHeight,
      0,
      this.gl.RGBA,
      this.gl.UNSIGNED_BYTE,
      imagePixels(image),
    );
  }

  private updateImageTexture(texture: WebGLTexture, image: WebImage): void {
    this.gl.bindTexture(this.gl.TEXTURE_2D, texture);
    this.gl.pixelStorei(this.gl.UNPACK_ALIGNMENT, 1);
    this.gl.texSubImage2D(
      this.gl.TEXTURE_2D,
      0,
      0,
      0,
      image.imageWidth,
      image.imageHeight,
      this.gl.RGBA,
      this.gl.UNSIGNED_BYTE,
      imagePixels(image),
    );
  }

  private drawText(instances: number[]): void {
    const count = instances.length / TEXT_INSTANCE_FLOATS;
    if (count === 0) {
      return;
    }

    const gl = this.gl;
    gl.useProgram(this.textProgram);
    gl.uniform2f(
      required(gl.getUniformLocation(this.textProgram, "u_resolution"), "text resolution"),
      this.width,
      this.height,
    );
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.atlas.texture);
    gl.uniform1i(required(gl.getUniformLocation(this.textProgram, "u_atlas"), "atlas sampler"), 0);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.textBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(instances), gl.STREAM_DRAW);
    bindInstancedAttribute(gl, this.textProgram, "a_rect", 4, TEXT_INSTANCE_FLOATS, 0);
    bindInstancedAttribute(gl, this.textProgram, "a_uv", 4, TEXT_INSTANCE_FLOATS, 4);
    bindInstancedAttribute(gl, this.textProgram, "a_color", 4, TEXT_INSTANCE_FLOATS, 8);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
    gl.disable(gl.BLEND);
  }
}

const SOLID_INSTANCE_FLOATS = 8;
const IMAGE_VERTEX_FLOATS = 4;
const TEXT_INSTANCE_FLOATS = 12;

class GlyphAtlas {
  readonly texture: WebGLTexture;
  private readonly glyphs = new Map<string, Glyph>();
  private readonly tileCanvas = document.createElement("canvas");
  private readonly tileContext: CanvasRenderingContext2D;
  private next = 0;

  private static readonly size = 2048;
  private static readonly tileWidth = 128;
  private static readonly tileHeight = 96;
  private static readonly horizontalPaddingPx = 8;

  constructor(private readonly gl: WebGL2RenderingContext) {
    this.texture = required(gl.createTexture(), "create glyph atlas texture");
    this.tileCanvas.width = GlyphAtlas.tileWidth;
    this.tileCanvas.height = GlyphAtlas.tileHeight;
    const context = this.tileCanvas.getContext("2d");
    if (!context) {
      throw new Error("2D canvas is unavailable for glyph atlas");
    }

    this.tileContext = context;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, GlyphAtlas.size, GlyphAtlas.size, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  glyph(text: string, cellWidth: number, cellHeight: number, dpr: number, style: WebCell["style"]): Glyph {
    const key = `${cellWidth}:${cellHeight}:${dpr}:${style.bold ? "b" : ""}${style.italic ? "i" : ""}:${text}`;
    const cached = this.glyphs.get(key);
    if (cached) {
      return cached;
    }

    const columns = Math.floor(GlyphAtlas.size / GlyphAtlas.tileWidth);
    const x = (this.next % columns) * GlyphAtlas.tileWidth;
    const y = Math.floor(this.next / columns) * GlyphAtlas.tileHeight;
    this.next += 1;
    if (y + GlyphAtlas.tileHeight > GlyphAtlas.size) {
      throw new Error("glyph atlas is full");
    }

    this.tileContext.setTransform(1, 0, 0, 1, 0, 0);
    this.tileContext.clearRect(0, 0, GlyphAtlas.tileWidth, GlyphAtlas.tileHeight);
    this.tileContext.fillStyle = "white";
    this.tileContext.textBaseline = "top";
    this.tileContext.textAlign = "left";
    this.tileContext.font = terminalFont(cellHeight * dpr, style);
    const horizontalPadding = Math.ceil(GlyphAtlas.horizontalPaddingPx * dpr);
    this.tileContext.fillText(text, horizontalPadding, Math.round(1 * dpr));

    const glyphWidthPx = Math.min(GlyphAtlas.tileWidth, Math.ceil(cellWidth * dpr) + horizontalPadding * 2);
    const glyphHeightPx = Math.min(GlyphAtlas.tileHeight, Math.ceil(cellHeight * dpr));

    this.gl.bindTexture(this.gl.TEXTURE_2D, this.texture);
    this.gl.pixelStorei(this.gl.UNPACK_ALIGNMENT, 1);
    this.gl.texSubImage2D(this.gl.TEXTURE_2D, 0, x, y, this.gl.RGBA, this.gl.UNSIGNED_BYTE, this.tileCanvas);

    const glyph = {
      u: x / GlyphAtlas.size,
      v: y / GlyphAtlas.size,
      w: glyphWidthPx / GlyphAtlas.size,
      h: glyphHeightPx / GlyphAtlas.size,
      width: glyphWidthPx / dpr,
      height: glyphHeightPx / dpr,
      offsetX: -horizontalPadding / dpr,
      offsetY: 0,
    };
    this.glyphs.set(key, glyph);
    return glyph;
  }

  dispose(): void {
    this.gl.deleteTexture(this.texture);
  }
}

function pushSolidInstance(instances: number[], x: number, y: number, width: number, height: number, color: Rgba): void {
  instances.push(x, y, width, height, ...color);
}

function pushBoxDrawingInstance(instances: number[], cell: WebCell, frame: WebTerminalFrame, color: Rgba): boolean {
  const x = cell.x * frame.cellWidth;
  const y = cell.y * frame.cellHeight;
  const thickness = Math.max(2, Math.ceil(frame.cellWidth * 0.18));
  const midX = x + Math.floor(frame.cellWidth / 2);
  const midY = y + Math.floor(frame.cellHeight / 2);
  const horizontal = () => pushSolidInstance(instances, x - 1, midY, frame.cellWidth + 2, thickness, color);
  const vertical = () => pushSolidInstance(instances, midX, y - 1, thickness, frame.cellHeight + 2, color);

  switch (cell.text) {
    case "─":
      horizontal();
      return true;
    case "│":
      vertical();
      return true;
    case "┌":
    case "╭":
      pushSolidInstance(instances, midX, midY, frame.cellWidth / 2 + 1, thickness, color);
      pushSolidInstance(instances, midX, midY, thickness, frame.cellHeight / 2 + 1, color);
      return true;
    case "┐":
    case "╮":
      pushSolidInstance(instances, x, midY, frame.cellWidth / 2 + 1, thickness, color);
      pushSolidInstance(instances, midX, midY, thickness, frame.cellHeight / 2 + 1, color);
      return true;
    case "└":
    case "╰":
      pushSolidInstance(instances, midX, midY, frame.cellWidth / 2 + 1, thickness, color);
      pushSolidInstance(instances, midX, y, thickness, frame.cellHeight / 2 + 1, color);
      return true;
    case "┘":
    case "╯":
      pushSolidInstance(instances, x, midY, frame.cellWidth / 2 + 1, thickness, color);
      pushSolidInstance(instances, midX, y, thickness, frame.cellHeight / 2 + 1, color);
      return true;
    default:
      return false;
  }
}

function pushTextInstance(
  instances: number[],
  x: number,
  y: number,
  width: number,
  height: number,
  glyph: Glyph,
  color: Rgba,
): void {
  instances.push(x, y, width, height, glyph.u, glyph.v, glyph.w, glyph.h, ...color);
}

function imageVertices(image: WebImage): number[] {
  if (image.source.maxX <= image.source.minX || image.source.maxY <= image.source.minY) {
    return [];
  }
  const x0 = image.destination.minX;
  const y0 = image.destination.minY;
  const x1 = image.destination.maxX;
  const y1 = image.destination.maxY;
  const u0 = image.source.minX / image.imageWidth;
  const v0 = image.source.minY / image.imageHeight;
  const u1 = image.source.maxX / image.imageWidth;
  const v1 = image.source.maxY / image.imageHeight;

  return [x0, y0, u0, v0, x0, y1, u0, v1, x1, y1, u1, v1, x0, y0, u0, v0, x1, y1, u1, v1, x1, y0, u1, v0];
}

function imagePixels(image: WebImage): Uint8Array {
  if (image.rgba instanceof Uint8Array) {
    return image.rgba;
  }
  if (ArrayBuffer.isView(image.rgba)) {
    return new Uint8Array(image.rgba.buffer, image.rgba.byteOffset, image.rgba.byteLength);
  }
  return new Uint8Array(image.rgba);
}

function bindInstancedAttribute(
  gl: WebGL2RenderingContext,
  program: WebGLProgram,
  name: string,
  size: number,
  strideFloats: number,
  offsetFloats: number,
): void {
  bindAttribute(gl, program, name, size, strideFloats, offsetFloats, true);
}

function bindAttribute(
  gl: WebGL2RenderingContext,
  program: WebGLProgram,
  name: string,
  size: number,
  strideFloats: number,
  offsetFloats: number,
  instanced: boolean,
): void {
  const location = gl.getAttribLocation(program, name);
  if (location < 0) {
    throw new Error(`missing WebGL attribute ${name}`);
  }
  gl.enableVertexAttribArray(location);
  gl.vertexAttribPointer(location, size, gl.FLOAT, false, strideFloats * 4, offsetFloats * 4);
  gl.vertexAttribDivisor(location, instanced ? 1 : 0);
}

type ResolvedCellColors = {
  foreground: WebColor;
  background: WebColor | null;
};

function resolvedCellColors(frame: WebTerminalFrame, cell: WebCell): ResolvedCellColors {
  const foreground = cell.fg ?? frame.colors.foreground;
  const background = cell.bg;
  if (!cell.style.inverse) {
    return { foreground, background };
  }
  return {
    foreground: background ?? frame.colors.background,
    background: foreground,
  };
}

function terminalFont(cellHeight: number, style: WebCell["style"]): string {
  const slant = style.italic ? "italic " : "";
  return `${slant}${Math.floor(cellHeight * 0.78)}px "Maple Mono NF", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
}

function rgba(color: WebColor, alpha = 1): Rgba {
  return [color.r / 255, color.g / 255, color.b / 255, alpha];
}

function createProgram(gl: WebGL2RenderingContext, vertexSource: string, fragmentSource: string): WebGLProgram {
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = required(gl.createProgram(), "create WebGL program");
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error(gl.getProgramInfoLog(program) ?? "link WebGL program");
  }
  return program;
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = required(gl.createShader(type), "create WebGL shader");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) ?? "compile WebGL shader");
  }
  return shader;
}

function required<T>(value: T | null, action: string): T {
  if (value == null) {
    throw new Error(`Failed to ${action}`);
  }
  return value;
}

const SOLID_VERTEX_SHADER = `#version 300 es
precision highp float;
in vec4 a_rect;
in vec4 a_color;
uniform vec2 u_resolution;
out vec4 v_color;

vec2 corner(uint index) {
  if (index == 0u) return vec2(0.0, 0.0);
  if (index == 1u) return vec2(1.0, 0.0);
  if (index == 2u) return vec2(0.0, 1.0);
  if (index == 3u) return vec2(0.0, 1.0);
  if (index == 4u) return vec2(1.0, 0.0);
  return vec2(1.0, 1.0);
}

void main() {
  vec2 position = a_rect.xy + corner(uint(gl_VertexID)) * a_rect.zw;
  vec2 clip = (position / u_resolution) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_color = a_color;
}`;

const SOLID_FRAGMENT_SHADER = `#version 300 es
precision highp float;
in vec4 v_color;
out vec4 out_color;

void main() {
  out_color = v_color;
}`;

const IMAGE_VERTEX_SHADER = `#version 300 es
precision highp float;
in vec2 a_position;
in vec2 a_uv;
uniform vec2 u_resolution;
out vec2 v_uv;

void main() {
  vec2 clip = (a_position / u_resolution) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_uv = a_uv;
}`;

const IMAGE_FRAGMENT_SHADER = `#version 300 es
precision highp float;
uniform sampler2D u_image;
uniform int u_pixel_format;
in vec2 v_uv;
out vec4 out_color;

void main() {
  vec4 color = texture(u_image, v_uv);
  out_color = u_pixel_format == 1 ? vec4(color.bgr, 1.0) : color;
}`;

const TEXT_VERTEX_SHADER = `#version 300 es
precision highp float;
in vec4 a_rect;
in vec4 a_uv;
in vec4 a_color;
uniform vec2 u_resolution;
out vec2 v_uv;
out vec4 v_color;

vec2 corner(uint index) {
  if (index == 0u) return vec2(0.0, 0.0);
  if (index == 1u) return vec2(1.0, 0.0);
  if (index == 2u) return vec2(0.0, 1.0);
  if (index == 3u) return vec2(0.0, 1.0);
  if (index == 4u) return vec2(1.0, 0.0);
  return vec2(1.0, 1.0);
}

void main() {
  vec2 local = corner(uint(gl_VertexID));
  vec2 position = a_rect.xy + local * a_rect.zw;
  vec2 clip = (position / u_resolution) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_uv = a_uv.xy + local * a_uv.zw;
  v_color = a_color;
}`;

const TEXT_FRAGMENT_SHADER = `#version 300 es
precision highp float;
uniform sampler2D u_atlas;
in vec2 v_uv;
in vec4 v_color;
out vec4 out_color;

void main() {
  float alpha = texture(u_atlas, v_uv).a;
  out_color = vec4(v_color.rgb, v_color.a * alpha);
}`;
