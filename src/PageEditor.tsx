import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** Saves this long after the user stops typing. */
const SAVE_AFTER_MS = 1500;

type Saved = { saved: boolean; current: string | null };

/**
 * Edits one memory page, the page of `entity` or, for `null`, the About me
 * page (ADR 0011). Plain text in a text field, never rendered as HTML: Lax's
 * block is written from captured text. Saves a moment after typing stops
 * and when closed, and only over what it loaded, so nothing is lost if the
 * page changed meanwhile: then both texts are shown.
 */
export function PageEditor({ entity, onClose }: { entity: number | null; onClose: () => void }) {
  const [text, setText] = useState<string | null>(null);
  // What the page holds now when a save found it changed; `undefined` when
  // the page is gone.
  const [conflict, setConflict] = useState<string | null | undefined>(null);
  const [error, setError] = useState<string | null>(null);
  const [state, setState] = useState<"saved" | "saving" | "unsaved">("saved");
  // What the page held when last loaded or saved: a save goes only over it.
  const loaded = useRef<string | null>(null);
  const latest = useRef<string | null>(null);
  // The first save of this visit starts a new version of the user's.
  const first = useRef(true);
  const saving = useRef<Promise<void>>(Promise.resolve());

  const save = useCallback(() => {
    saving.current = saving.current.then(async () => {
      const value = latest.current;
      if (value === null || loaded.current === null || value === loaded.current) {
        return;
      }
      setState("saving");
      try {
        const result = await invoke<Saved>("save_page", {
          entity,
          loaded: loaded.current,
          text: value,
          first: first.current,
        });
        if (result.saved) {
          first.current = false;
          loaded.current = value;
          setState(latest.current === value ? "saved" : "unsaved");
        } else {
          setConflict(result.current ?? undefined);
          setState("unsaved");
        }
      } catch (e) {
        setError(String(e));
        setState("unsaved");
      }
    });
    return saving.current;
  }, [entity]);

  useEffect(() => {
    let closed = false;
    invoke<string>("page_text", { entity }).then(
      (page) => {
        if (!closed) {
          loaded.current = page;
          latest.current = page;
          setText(page);
        }
      },
      (e) => setError(String(e)),
    );
    return () => {
      closed = true;
      // Whatever is still unsaved is saved before Lax may write the page.
      void save().finally(() => invoke("close_page", { entity }));
    };
  }, [entity, save]);

  // A moment after typing stops.
  useEffect(() => {
    if (text === null || text === loaded.current) {
      return;
    }
    const timer = window.setTimeout(() => void save(), SAVE_AFTER_MS);
    return () => window.clearTimeout(timer);
  }, [text, save]);

  const keepMine = () => {
    // The page now holds `conflict`; save the user's text over it.
    if (typeof conflict === "string") {
      loaded.current = conflict;
      setConflict(null);
      void save();
    }
  };
  const usePages = () => {
    if (typeof conflict === "string") {
      loaded.current = conflict;
      latest.current = conflict;
      setText(conflict);
      setConflict(null);
      setState("saved");
    }
  };

  if (error && text === null) {
    return (
      <p className="error" role="alert">
        {error}
      </p>
    );
  }
  if (text === null) {
    return null;
  }
  return (
    <div className="page-editor">
      <textarea
        value={text}
        spellCheck={false}
        aria-label="Page text"
        onChange={(e) => {
          latest.current = e.target.value;
          setText(e.target.value);
          setState("unsaved");
        }}
      />
      <p className="muted hint">
        Lax writes only between the <code>lax:begin</code> and <code>lax:end</code> lines.
        Everything else is yours, and Lax never changes it. A line you change between them becomes
        yours too. {state === "saving" ? "Saving…" : state === "saved" ? "Saved." : ""}
      </p>
      {conflict === undefined && (
        <section className="card confirm" role="alert">
          <h3>The page is gone</h3>
          <p>
            Its name was forgotten meanwhile, and the page with it. Your text is still in the box
            above: copy it if you want to keep it.
          </p>
        </section>
      )}
      {typeof conflict === "string" && (
        <section className="card confirm" role="alertdialog" aria-label="The page changed">
          <h3>The page changed while you were editing</h3>
          <p>Your text is still in the box above. The page now says:</p>
          <textarea value={conflict} readOnly aria-label="The page's text now" />
          <p className="actions">
            <button type="button" onClick={keepMine}>
              Keep my text
            </button>{" "}
            <button type="button" className="secondary" onClick={usePages}>
              Use the page's text
            </button>
          </p>
        </section>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <p className="actions">
        <button type="button" className="secondary" onClick={onClose}>
          Done
        </button>
      </p>
    </div>
  );
}
