import type { TerminalKey } from "./terminal-api";

export const DoomKeys = {
  rightArrow: 0xae,
  leftArrow: 0xac,
  upArrow: 0xad,
  downArrow: 0xaf,
  strafeLeft: 0xa0,
  strafeRight: 0xa1,
  use: 0xa2,
  fire: 0xa3,
  escape: 27,
  enter: 13,
  tab: 9,
  backspace: 127,
  pause: 0xff,
  equals: 0x3d,
  minus: 0x2d,
  shift: 0x80 + 0x36,
  ctrl: 0x80 + 0x1d,
} as const;

export function mapBrowserKeyToDoom(event: TerminalKey): number[] {
  switch (event.key) {
    case "ArrowUp":
      return [DoomKeys.upArrow];
    case "ArrowDown":
      return [DoomKeys.downArrow];
    case "ArrowRight":
      return [DoomKeys.rightArrow];
    case "ArrowLeft":
      return [DoomKeys.leftArrow];
    case "Enter":
      return [DoomKeys.enter];
    case "Escape":
      return [DoomKeys.escape];
    case "Tab":
      return [DoomKeys.tab];
    case "Backspace":
      return [DoomKeys.backspace];
    case " ":
    case "Spacebar":
      return [DoomKeys.use];
    case "-":
      return [DoomKeys.minus];
    case "+":
    case "=":
      return [DoomKeys.equals];
    default:
      return printableKey(event);
  }
}

function printableKey(event: TerminalKey): number[] {
  const key = event.key.toLowerCase();
  const shift = event.shiftKey ? [DoomKeys.shift] : [];
  if (event.ctrlKey) {
    return [DoomKeys.fire];
  }
  switch (key) {
    case "w":
      return [DoomKeys.upArrow, ...shift];
    case "s":
      return [DoomKeys.downArrow, ...shift];
    case "a":
      return [DoomKeys.strafeLeft, ...shift];
    case "d":
      return [DoomKeys.strafeRight, ...shift];
    case "f":
      return [DoomKeys.fire];
    case "q":
      return [DoomKeys.pause];
    default:
      if (key.length === 1) {
        return [key.charCodeAt(0)];
      }
      return [];
  }
}
