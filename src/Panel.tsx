import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
// The app icon is the one source of the mascot (docs/BRAND.md).
import laxIcon from "../src-tauri/icons/app-icon.svg";

/**
 * The writing assistant's panel (ADR 0009), shown when the user double-taps
 * ⌥ Option. Escape closes it and gives focus back to the app it came to.
 */
export function Panel() {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        invoke("close_panel").catch(() => {});
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <main className="panel">
      <img src={laxIcon} alt="" />
      <div>
        <p>
          <strong>Lax is here.</strong>
        </p>
        <p className="muted">Writing drafts comes in the next version. Escape closes.</p>
      </div>
    </main>
  );
}
