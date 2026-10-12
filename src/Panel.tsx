import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

// Shapes returned by src-tauri/src/write.rs.
type PanelCall = { app: string; instruction: string };
type Draft = { draft: string; pages: string[]; readWindow: boolean };

type Stage =
  | { kind: "ask" }
  | { kind: "writing" }
  | { kind: "draft"; draft: Draft }
  | { kind: "error"; message: string };

/**
 * The writing assistant's panel (ADR 0009), shown when the user double-taps
 * ⌥ Option. The salmon swims in to the text cursor, then the card opens with
 * the instruction from the field. Enter writes a draft with the model on this
 * Mac. Escape closes the panel and gives focus back to the app. Nothing is
 * saved; the draft is shown as text only.
 */
export function Panel() {
  // Bumped every time the panel is shown, so the salmon swims in again.
  const [visit, setVisit] = useState(0);
  const [call, setCall] = useState<PanelCall | null>(null);
  const [instruction, setInstruction] = useState("");
  const [stage, setStage] = useState<Stage>({ kind: "ask" });
  const card = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);

  const open = useCallback(() => {
    setVisit((v) => v + 1);
    setStage({ kind: "ask" });
    invoke<PanelCall | null>("panel_call").then(
      (c) => {
        setCall(c);
        setInstruction(c?.instruction ?? "");
      },
      () => setCall(null),
    );
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        invoke("close_panel").catch(() => {});
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("focus", open);
    open();
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("focus", open);
    };
  }, [open]);

  // The window is as tall as the card needs.
  useLayoutEffect(() => {
    const height = (card.current?.offsetTop ?? 0) + (card.current?.offsetHeight ?? 0) + 14;
    invoke("fit_panel", { height }).catch(() => {});
  }, [stage, call, instruction, visit]);

  useEffect(() => {
    if (stage.kind === "ask") {
      input.current?.focus();
    }
  }, [stage, visit]);

  const write = () => {
    if (!instruction.trim() || stage.kind === "writing") {
      return;
    }
    setStage({ kind: "writing" });
    invoke<Draft>("write_draft", { instruction }).then(
      (draft) => setStage({ kind: "draft", draft }),
      (e) => setStage({ kind: "error", message: String(e) }),
    );
  };

  return (
    <main className="panel" key={visit}>
      <div className="swim">
        <img className="salmon" src={salmon} alt="" />
        <span className="bubble one" />
        <span className="bubble two" />
      </div>
      <div className="panel-card" ref={card}>
        {stage.kind === "ask" || stage.kind === "error" ? (
          <>
            <textarea
              ref={input}
              className="panel-input"
              value={instruction}
              maxLength={2000}
              rows={Math.min(4, Math.max(1, Math.ceil(instruction.length / 38)))}
              placeholder="What should Lax write?"
              aria-label="What Lax should write"
              onChange={(event) => setInstruction(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  write();
                }
              }}
            />
            {stage.kind === "error" ? (
              <p className="error" role="alert">
                {stage.message}
              </p>
            ) : (
              <p className="muted">
                Enter writes{call?.app ? ` for ${call.app}` : ""} · Esc closes
              </p>
            )}
          </>
        ) : stage.kind === "writing" ? (
          <p className="muted" aria-live="polite">
            Writing…
          </p>
        ) : (
          <>
            <div className="panel-draft">{stage.draft.draft}</div>
            <p className="muted">
              {stage.draft.pages.length > 0
                ? `From your memory: ${stage.draft.pages.join(", ")}. `
                : ""}
              Select and ⌘C to copy · Esc closes
            </p>
          </>
        )}
      </div>
    </main>
  );
}
