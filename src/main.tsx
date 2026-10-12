import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { Panel } from "./Panel";
import "./styles.css";

// The writing assistant's panel is a window of its own (src-tauri/src/summon.rs).
const panel = getCurrentWindow().label === "panel";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{panel ? <Panel /> : <App />}</React.StrictMode>,
);
