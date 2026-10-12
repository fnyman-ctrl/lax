import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
// The salmon from the app icon, on its own (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

/**
 * The writing assistant's panel (ADR 0009), shown when the user double-taps
 * ⌥ Option. The salmon swims in to the text cursor, then the card opens.
 * Escape closes it and gives focus back to the app it came to.
 */
export function Panel() {
  // Bumped every time the panel is shown, so the salmon swims in again.
  const [visit, setVisit] = useState(0);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        invoke("close_panel").catch(() => {});
      }
    };
    const onFocus = () => setVisit((v) => v + 1);
    window.addEventListener("keydown", onKey);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  return (
    <main className="panel" key={visit}>
      <div className="swim">
        <img className="salmon" src={salmon} alt="" />
        <span className="bubble one" />
        <span className="bubble two" />
      </div>
      <div className="panel-card">
        <p>
          <strong>Lax is here.</strong>
        </p>
        <p className="muted">Drafts come in the next version. Esc closes.</p>
      </div>
    </main>
  );
}
