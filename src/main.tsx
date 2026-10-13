import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { Panel } from "./Panel";
import { applyTheme, storedTheme } from "./theme";
import "./styles.css";

// The writing assistant's panel is a window of its own (src-tauri/src/summon.rs).
const panel = getCurrentWindow().label === "panel";
if (panel) {
  // The panel's window is transparent, so only the salmon and the card show.
  document.documentElement.classList.add("panel-window");
} else {
  // The main window: light, dark or as macOS, as the user picked.
  applyTheme(storedTheme(), false);
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{panel ? <Panel /> : <App />}</React.StrictMode>,
);
