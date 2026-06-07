export type WebColor = { r: number; g: number; b: number };

export type WebCell = {
  x: number;
  y: number;
  text: string;
  fg: WebColor | null;
  bg: WebColor | null;
  osc8: string | null;
  style: {
    bold: boolean;
    italic: boolean;
    faint: boolean;
    blink: boolean;
    inverse: boolean;
    invisible: boolean;
    strikethrough: boolean;
    overline: boolean;
    underline: boolean;
  };
};
export type WebRect = {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
};

export type WebImageLayer = "belowBackground" | "belowText" | "aboveText";

export type WebImage = {
  key: string;
  layer: WebImageLayer;
  imageWidth: number;
  imageHeight: number;
  source: WebRect;
  destination: WebRect;
  rgba: ArrayLike<number>;
};

export type WebTerminalFrame = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
  colors: {
    background: WebColor;
    foreground: WebColor;
    cursor: WebColor | null;
  };
  cursor: { x: number; y: number; color: WebColor | null } | null;
  cells: WebCell[];
  images: WebImage[];
};
