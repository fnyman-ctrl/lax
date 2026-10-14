import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

// Shapes returned by src-tauri/src/write.rs.
type PanelCall = { id: number; app: string; instruction: string; selected: number };
type Draft = { draft: string; pages: string[]; readWindow: boolean };

/** When the card has finished opening: `.panel-card`'s unfold in styles.css. */
const CARD_OPENS_MS = 600;
/** How long the salmon takes to leave: `.panel.leaving` in styles.css. */
const LEAVE_MS = 170;

/** How the salmon leaves: swims away on Escape, dives into the text on Enter. */
type Leaving = "away" | "dive";

const stillMotion = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

type Stage =
  | { kind: "ask" }
  | { kind: "writing" }
  | { kind: "draft"; draft: Draft; text: string; error?: string }
  | { kind: "error"; message: string };

/**
 * The writing assistant's panel (ADR 0009), shown when the user double-taps
 * ⌥ Option. The salmon swims in to the text cursor, then the card opens with
 * the instruction from the field, or empty when text is selected: then the
 * user says what to do with the selection. Enter writes a draft with the model on this
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
  const [leaving, setLeaving] = useState<Leaving | null>(null);
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
        setLeaving(null);
        setVisit((v) => v + 1);
        setStage({ kind: "ask" });
        setCall(c);
        setInstruction(c?.instruction ?? "");
      },
      () => setCall(null),
    );
  }, []);

  // The salmon leaves, then `then` runs: the card and the glass go first, so
  // nothing is left hanging while the window closes. If the window stays
  // (a draft that couldn't be inserted), everything comes back.
  // One leave at a time: a second Enter or Escape meanwhile does nothing, so
  // a draft is never inserted twice.
  const going = useRef(false);
  const leave = useRef<(how: Leaving, then: () => Promise<unknown>) => void>(() => {});
  leave.current = (how, then) => {
    if (going.current) {
      return;
    }
    going.current = true;
    const done = () => {
      going.current = false;
      setLeaving(null);
    };
    if (stillMotion()) {
      void then().finally(done);
      return;
    }
    setLeaving(how);
    opened.current = false;
    fit();
    window.setTimeout(() => void then().finally(done), LEAVE_MS);
  };

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        leave.current("away", () => invoke("close_panel").catch(() => {}));
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

  // Whether this Mac draws Liquid Glass behind the card (macOS 26 and later).
  const [glass, setGlass] = useState(false);
  // The glass shows once the card has opened, not while the salmon swims in.
  const opened = useRef(false);

  // The window is as tall as the card needs, with the glass behind the card.
  const fit = useCallback(() => {
    const c = card.current;
    const height = (c?.offsetTop ?? 0) + (c?.offsetHeight ?? 0) + 14;
    const rect =
      opened.current && c
        ? { x: c.offsetLeft, y: c.offsetTop, width: c.offsetWidth, height: c.offsetHeight }
        : null;
    invoke<boolean>("fit_panel", { height, card: rect }).then(setGlass, () => {});
  }, []);

  useLayoutEffect(fit, [fit, stage, call, instruction, visit]);

  useEffect(() => {
    opened.current = false;
    const timer = window.setTimeout(() => {
      opened.current = true;
      fit();
    }, CARD_OPENS_MS);
    return () => window.clearTimeout(timer);
  }, [fit, visit]);

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
    leave.current("dive", () =>
      invoke("insert_draft", { text }).catch((e) => {
        // The panel stays, with the draft: the card and its glass come back.
        opened.current = true;
        fit();
        setStage((s) => (s.kind === "draft" ? { ...s, error: String(e) } : s));
      }),
    );
  };

  return (
    <main
      className={["panel", glass && "glass", leaving && `leaving ${leaving}`]
        .filter(Boolean)
        .join(" ")}
      key={visit}
    >
      <div className={stage.kind === "writing" ? "swim working" : "swim"}>
        <img className="salmon" src={salmon} alt="" />
        <span className="air one" />
        <span className="air two" />
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
              placeholder={
                call?.selected ? "What should Lax do with the selected text?" : "What should Lax write?"
              }
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
                {call?.selected
                  ? `Changes the ${call.selected} characters you selected · Esc closes`
                  : `Enter writes${call?.app ? ` for ${call.app}` : ""} · Esc closes`}
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
