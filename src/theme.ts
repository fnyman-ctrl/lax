import { getCurrentWindow } from "@tauri-apps/api/window";

/** Light, dark, or as macOS ("system"). Kept in this Mac's window storage. */
export type Theme = "system" | "light" | "dark";

const KEY = "theme";

export function storedTheme(): Theme {
  try {
    const value = localStorage.getItem(KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

/** Shows `theme` in this window, title bar included, and remembers it. */
export function applyTheme(theme: Theme, remember = true) {
  const root = document.documentElement;
  if (theme === "system") {
    delete root.dataset.theme;
  } else {
    root.dataset.theme = theme;
  }
  getCurrentWindow()
    .setTheme(theme === "system" ? null : theme)
    .catch(() => {});
  if (remember) {
    try {
      localStorage.setItem(KEY, theme);
    } catch {
      // Without storage the choice lasts until the window closes.
    }
  }
}
