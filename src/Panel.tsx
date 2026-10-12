import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

// Shapes returned by src-tauri/src/write.rs.
type PanelCall = { id: number; app: string; instruction: string };
type Draft = { draft: string; pages: string[]; readWindow: boolean };

type Stage =
  | { kind: "ask" }
  | { kind: "writing" }
  | { kind: "draft"; draft: Draft; text: string; error?: string }
  | { kind: "error"; message: string };

/**
 * The writing assistant's panel (ADR 0009), shown when the user double-taps
 * ⌥ Option. The salmon swims in to the text cursor, then the card opens with
 * the instruction from the field. Enter writes a draft with the model on this
 * Mac; the user can edit it, and Enter puts it into the field in place of the
 * instruction. Escape closes the panel and gives focus back to the app.
 * Nothing is saved; the draft is shown as text only.
 */
export function Panel() {
  // Bumped every time the panel is shown, so the salmon swims in again.
  const [visit, setVisit] = useState(0);
  const [call, setCall] = useState<PanelCall | null>(null);
  const [instruction, setInstruction] = useState("");
  const [stage, setStage] = useState<Stage>({ kind: "ask" });
  const card = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const draftInput = useRef<HTMLTextAreaElement>(null);

  // The call this panel shows. Coming back to the same call (after a draft
  // couldn't be inserted) keeps the draft; a new call starts over.
  const shown = useRef<number | null>(null);

  const open = useCallback(() => {
    invoke<PanelCall | null>("panel_call").then(
      (c) => {
        if (c && c.id === shown.current) {
          return;
        }
        shown.current = c?.id ?? null;
        setVisit((v) => v + 1);
        setStage({ kind: "ask" });
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

  // The text cursor at the end, ready to go on typing or press Enter.
  useEffect(() => {
    const field = stage.kind === "draft" ? draftInput.current : input.current;
    if (field && (stage.kind === "ask" || stage.kind === "draft")) {
      field.focus();
      field.setSelectionRange(field.value.length, field.value.length);
    }
  }, [stage.kind, visit]);

  const write = () => {
    if (!instruction.trim() || stage.kind === "writing") {
      return;
    }
    setStage({ kind: "writing" });
    invoke<Draft>("write_draft", { instruction }).then(
      (draft) => setStage({ kind: "draft", draft, text: draft.draft }),
      (e) => setStage({ kind: "error", message: String(e) }),
    );
  };

  const insert = (text: string) => {
    if (!text.trim()) {
      return;
    }
    invoke("insert_draft", { text }).catch((e) =>
      setStage((s) => (s.kind === "draft" ? { ...s, error: String(e) } : s)),
    );
  };

  return (
    <main className="panel" key={visit}>
      <div className={stage.kind === "writing" ? "swim working" : "swim"}>
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
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
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
            <textarea
              ref={draftInput}
              className="panel-draft"
              value={stage.text}
              maxLength={4000}
              rows={Math.min(
                12,
                Math.max(3, Math.ceil(stage.text.length / 40) + stage.text.split("\n").length),
              )}
              aria-label="The draft"
              onChange={(event) => {
                const text = event.target.value;
                setStage((s) => (s.kind === "draft" ? { ...s, text, error: undefined } : s));
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  insert(stage.text);
                }
              }}
            />
            {stage.error ? (
              <p className="error" role="alert">
                {stage.error}
              </p>
            ) : (
              <p className="muted">
                {stage.draft.pages.length > 0
                  ? `From your memory: ${stage.draft.pages.join(", ")}. `
                  : ""}
                Enter inserts · ⇧Enter new line · Esc closes
              </p>
            )}
          </>
        )}
      </div>
    </main>
  );
}
