import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shape returned by src-tauri/src/summon.rs.
type WritingStatus = { enabled: boolean };

/**
 * The writing assistant (ADR 0009): off until the user turns it on. Then a
 * double tap of ⌥ Option in a text field calls Lax there.
 */
export function Write() {
  const [status, setStatus] = useState<WritingStatus | null | "failed">(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    invoke<WritingStatus>("writing_status").then(setStatus, () => setStatus("failed"));
  }, []);

  const toggle = (enabled: boolean) => {
    setMessage(null);
    invoke<WritingStatus>("set_writing_enabled", { enabled }).then(setStatus, (e) =>
      setMessage(String(e)),
    );
  };

  if (status === null) {
    return null;
  }
  if (status === "failed") {
    return <p className="error">Lax couldn't read the setting.</p>;
  }

  return (
    <section className="write">
      <div className="card">
        {status.enabled ? (
          <>
            <p>
              <strong>Double-tap ⌥ Option</strong> in a text field, and Lax comes to it. Type
              what to write, or write it in the field first ("reply in my tone and thank her"),
              and press Enter for a draft. Read and edit it; Enter puts it in the field, and
              Escape sends Lax away.
            </p>
            <p className="muted">
              Drafts are written by the chat model on this Mac from your instruction, what
              Lax knows about you and the people you name, and the window you're writing
              in. Nothing about them is saved, and Lax never sends anything.
            </p>
            <p className="buttons">
              <button type="button" className="secondary" onClick={() => toggle(false)}>
                Turn the writing assistant off
              </button>
            </p>
          </>
        ) : (
          <>
            <p>
              <strong>Call Lax to any text field.</strong> Double-tap ⌥ Option, and Lax comes to
              where you write. Nothing shows up in text fields until you do.
            </p>
            <p className="muted">
              To tell a double tap from ⌥2 (@), Lax notices when keys are pressed, never which
              ones, and nothing about them is saved. It doesn't come to excluded apps, password
              managers or while capture is paused.
            </p>
            <p className="buttons">
              <button type="button" onClick={() => toggle(true)}>
                Turn the writing assistant on
              </button>
            </p>
          </>
        )}
        {message && (
          <p className="error" role="alert">
            {message}
          </p>
        )}
      </div>
    </section>
  );
}
