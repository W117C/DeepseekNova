/**
 * store/theme.ts — 主题 + 显示模式管理
 * 主题：light / dark / system
 * 显示模式：icon / text
 */

import { create } from "zustand";

export type ThemeMode = "light" | "dark" | "system";
export type DisplayMode = "icon" | "text";

interface ThemeState {
  theme: ThemeMode;
  displayMode: DisplayMode;
  /** 实际生效的主题（system 解析后的结果） */
  resolvedTheme: "light" | "dark";
  setTheme: (t: ThemeMode) => void;
  setDisplayMode: (d: DisplayMode) => void;
  toggleDisplayMode: () => void;
  setResolvedTheme: (t: "light" | "dark") => void;
}

function getInitial<T extends string>(key: string, fallback: T): T {
  return (localStorage.getItem(key) as T) || fallback;
}

export const useTheme = create<ThemeState>((set) => ({
  theme: getInitial<ThemeMode>("dn-theme", "dark"),
  displayMode: getInitial<DisplayMode>("dn-display", "icon"),
  resolvedTheme: "dark",
  setTheme: (theme) => {
    localStorage.setItem("dn-theme", theme);
    set({ theme });
    applyTheme(theme);
  },
  setDisplayMode: (displayMode) => {
    localStorage.setItem("dn-display", displayMode);
    document.documentElement.setAttribute("data-display", displayMode);
    set({ displayMode });
  },
  toggleDisplayMode: () => {
    const next = getInitial<DisplayMode>("dn-display", "icon") === "icon" ? "text" : "icon";
    localStorage.setItem("dn-display", next);
    document.documentElement.setAttribute("data-display", next);
    set({ displayMode: next });
  },
  setResolvedTheme: (resolvedTheme) => set({ resolvedTheme }),
}));

/** 监听系统主题变化 */
let mediaQuery: MediaQueryList | null = null;

export function applyTheme(theme: ThemeMode) {
  const root = document.documentElement;

  // 清除旧属性
  root.removeAttribute("data-theme");

  if (theme === "system") {
    if (!mediaQuery) {
      mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      mediaQuery.addEventListener("change", (e) => {
        const resolved = e.matches ? "dark" : "light";
        document.documentElement.setAttribute("data-theme", resolved);
        useTheme.getState().setResolvedTheme(resolved);
      });
    }
    const resolved = mediaQuery.matches ? "dark" : "light";
    root.setAttribute("data-theme", resolved);
    useTheme.getState().setResolvedTheme(resolved);
  } else {
    if (mediaQuery) {
      mediaQuery.removeEventListener("change", () => {});
      mediaQuery = null;
    }
    root.setAttribute("data-theme", theme);
    useTheme.getState().setResolvedTheme(theme);
  }
}

/** 初始化主题（在 App 挂载时调用） */
export function initTheme() {
  const theme = getInitial<ThemeMode>("dn-theme", "dark");
  const displayMode = getInitial<DisplayMode>("dn-display", "icon");
  applyTheme(theme);
  document.documentElement.setAttribute("data-display", displayMode);
}
