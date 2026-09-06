import type { LibraryThemeCfg, ThemePreset } from "./types";

/** 主题色 → CSS 变量映射 */
const THEME_VARS: Array<[string, keyof LibraryThemeCfg]> = [
  ["--accent", "accent"],
  ["--bg", "bg"],
  ["--panel", "panel"],
  ["--panel-2", "panel2"],
  ["--ink", "ink"],
  ["--muted", "muted"],
  ["--read", "read"],
  ["--reading", "reading"],
  ["--wish", "wish"],
];

/** 应用书库主题（仅覆盖已配置的颜色，其余保持内置默认） */
export function applyTheme(theme?: LibraryThemeCfg | null) {
  const root = document.documentElement;
  for (const [cssVar, key] of THEME_VARS) {
    const v = theme?.[key];
    if (v && /^#[0-9a-fA-F]{3,8}$/.test(v.trim())) {
      root.style.setProperty(cssVar, v.trim());
    }
  }
}

/** 内置主题预设（暗金为应用默认；其余为整体换色的成套方案） */
export const THEME_PRESETS: ThemePreset[] = [
  {
    name: "暗金",
    colors: {
      accent: "#c9a86a",
      bg: "#101216",
      panel: "#20242c",
      panel2: "#232830",
      ink: "#e8e4d8",
      muted: "#8a8f98",
      read: "#4caf87",
      reading: "#d9a441",
      wish: "#6b7a8f",
    },
  },
  {
    name: "青瓷",
    colors: {
      accent: "#6aa8a0",
      bg: "#0e1413",
      panel: "#1a2422",
      panel2: "#1e2a28",
      ink: "#dde8e4",
      muted: "#7e938d",
      read: "#4caf87",
      reading: "#d9a441",
      wish: "#5f8a8f",
    },
  },
  {
    name: "绯红",
    colors: {
      accent: "#c96a7a",
      bg: "#141012",
      panel: "#281e22",
      panel2: "#2c2226",
      ink: "#f0dfe2",
      muted: "#9a8289",
      read: "#4caf87",
      reading: "#d9a441",
      wish: "#8f6b78",
    },
  },
  {
    name: "苍蓝",
    colors: {
      accent: "#6a9bc9",
      bg: "#0f1216",
      panel: "#1c222c",
      panel2: "#202733",
      ink: "#dfe6f0",
      muted: "#828da0",
      read: "#4caf87",
      reading: "#d9a441",
      wish: "#6b7a9f",
    },
  },
  {
    name: "紫罗兰",
    colors: {
      accent: "#a08ac9",
      bg: "#121016",
      panel: "#221e2c",
      panel2: "#262232",
      ink: "#e8e0f0",
      muted: "#8f829a",
      read: "#4caf87",
      reading: "#d9a441",
      wish: "#7a6b9f",
    },
  },
  {
    name: "墨白",
    colors: {
      accent: "#b8b8b8",
      bg: "#0e0e0e",
      panel: "#1c1c1c",
      panel2: "#212121",
      ink: "#e6e6e6",
      muted: "#8a8a8a",
      read: "#7fae8f",
      reading: "#c9b078",
      wish: "#8f8f8f",
    },
  },
];
