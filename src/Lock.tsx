import { type ReactNode, useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

// Must match CHANGED_EVENT in src-tauri/src/lock/app.rs.
const LOCK_CHANGED = "lock-changed";

type Changed = { locked: boolean; message: string | null };

/**
 * Shows the app only while the window is unlocked, and the lock screen
 * otherwise (ADR 0012). The lock is kept in Rust, which refuses every
 * command that returns what Lax captured or learned while the window is
 * locked; this keeps the window from showing anything that would ask.
 */
export function LockGate({ children }: { children: ReactNode }) {
  // null while the status is read.
  const [locked, setLocked] = useState<boolean | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<{ locked: boolean }>("lock_status").then(
      (status) => setLocked(status.locked),
      () => setLocked(true),
    );
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = listen<Changed>(LOCK_CHANGED, (event) => {
      // Locking hides the app at once; Rust reloads the window too.
      if (event.payload.locked) {
        setLocked(true);
      }
      setMessage(event.payload.message);
      refresh();
    });
    // Asked again once listening: a change between the first answer and
    // the listener would otherwise be missed.
    unlisten.then(refresh, () => {});
    window.addEventListener("focus", refresh);
    return () => {
      unlisten.then((stop) => stop());
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  if (locked === null) {
    return null;
  }
  if (!locked) {
    return <>{children}</>;
  }
  return <LockScreen message={message} onMessage={setMessage} onUnlocked={refresh} />;
}

/** The salmon, "Lax is locked" and Unlock. Only the button asks. */
function LockScreen({
  message,
  onMessage,
  onUnlocked,
}: {
  message: string | null;
  onMessage: (message: string | null) => void;
  onUnlocked: () => void;
}) {
  const [asking, setAsking] = useState(false);

  const unlock = () => {
    setAsking(true);
    onMessage(null);
    invoke("unlock")
      .then(onUnlocked, (e) => onMessage(typeof e === "string" ? e : null))
      .finally(() => setAsking(false));
  };

  return (
    <main className="container lock">
      <img src={salmon} alt="" className="chat-salmon" />
      <h1>Lax is locked</h1>
      <p className="actions">
        <button type="button" onClick={unlock} disabled={asking}>
          Unlock
        </button>
      </p>
      {message && (
        <p className="error" role="alert">
          {message}
        </p>
      )}
    </main>
  );
}
