import { type ReactNode, useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

// Must match CHANGED_EVENT in src-tauri/src/database.rs.
const DATABASE_CHANGED = "database-changed";
/** "Opening the database…" shows only if opening takes longer than this. */
const OPENING_SHOWN_AFTER_MS = 400;

type State =
  | "opening"
  | "update"
  | "convert"
  | "encrypting"
  | "open"
  | "keyUnavailable"
  | "keyMissing"
  | "cannotUnlock"
  | "newerSchema"
  | "failed";

/**
 * Shows the app once the database is open, and until then where opening
 * stands: the note before macOS asks for the key after an update, or what
 * went wrong (ADR 0010).
 */
export function DatabaseGate({ children }: { children: ReactNode }) {
  const [state, setState] = useState<State | null>(null);
  const [postponed, setPostponed] = useState<"lowSpace" | "notEncrypted" | null>(null);

  const refresh = useCallback(() => {
    invoke<{ state: State; postponed: "lowSpace" | "notEncrypted" | null }>(
      "database_status",
    ).then(
      (status) => {
        setState(status.state);
        setPostponed(status.postponed);
      },
      () => setState("failed"),
    );
  }, []);

  useEffect(() => {
    refresh();
    // Asked again once listening: a change between the first answer and
    // the listener would otherwise be missed.
    const unlisten = listen(DATABASE_CHANGED, refresh);
    unlisten.then(refresh, () => {});
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [refresh]);

  if (state === "open") {
    return (
      <>
        {postponed && <NotEncryptedYet why={postponed} />}
        {children}
      </>
    );
  }
  if (state === null || state === "opening") {
    return <Opening text="Opening the database…" />;
  }
  if (state === "encrypting") {
    return <Opening text="Encrypting the database…" />;
  }
  return <NotOpen state={state} />;
}

function Opening({ text }: { text: string }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setShown(true), OPENING_SHOWN_AFTER_MS);
    return () => window.clearTimeout(timer);
  }, []);
  if (!shown) {
    return null;
  }
  return (
    <main className="container database">
      <img src={salmon} alt="" className="chat-salmon working" />
      <p className="muted" role="status">
        {text}
      </p>
    </main>
  );
}

/** The database is open but still plaintext; Lax tries again next start. */
function NotEncryptedYet({ why }: { why: "lowSpace" | "notEncrypted" }) {
  return (
    <aside className="notice" role="status">
      {why === "lowSpace"
        ? "Lax couldn't encrypt its database: the disk needs free space of twice its size. "
        : "Lax couldn't encrypt its database this time. "}
      It works as before, and Lax tries again the next time it starts.
    </aside>
  );
}

function NotOpen({ state }: { state: Exclude<State, "open" | "opening" | "encrypting"> }) {
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const run = (command: string) => {
    setBusy(true);
    setError(null);
    invoke(command)
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  if (state === "convert") {
    return (
      <main className="container database">
        <h1>Lax encrypts its database</h1>
        <p>
          From now on, everything Lax keeps in its database is encrypted, with a key that only
          your login keychain holds. Lax makes an encrypted copy, checks it, and then puts it in
          the old one's place. It takes a moment, and only happens once.
        </p>
        <p>
          macOS may ask whether Lax may use its key. Type your login password and choose{" "}
          <strong>Always Allow</strong>.
        </p>
        <p className="muted">
          This is one-way: an older version of Lax can't open the encrypted database. Backups
          and snapshots made before now still hold the old, unencrypted file until they are
          gone. Your memory pages in ~/Lax are not part of the database yet.
        </p>
        <p className="actions">
          <button type="button" onClick={() => run("continue_opening")} disabled={busy}>
            Encrypt
          </button>
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </main>
    );
  }

  if (state === "update") {
    return (
      <main className="container database">
        <h1>Lax was updated</h1>
        <p>
          macOS will ask once whether Lax may use its key. Type your login password and choose{" "}
          <strong>Always Allow</strong>: then this version of Lax may read its key without asking,
          until the next update. If you choose <strong>Allow</strong>, macOS will ask again every
          time Lax starts.
        </p>
        <p className="muted">
          macOS asks only now, right after this note. If it asks for Lax's key at any other time,
          or another program asks for it, choose Deny.
        </p>
        <p className="actions">
          <button type="button" onClick={() => run("continue_opening")} disabled={busy}>
            Continue
          </button>
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </main>
    );
  }

  const tryAgain = (
    <button type="button" onClick={() => run("retry_opening")} disabled={busy}>
      Try again
    </button>
  );
  const startOver = (
    <button type="button" className="secondary" onClick={() => setConfirming(true)}>
      Start over…
    </button>
  );

  let content: ReactNode;
  if (state === "keyUnavailable") {
    content = (
      <>
        <h1>Lax can't read its key</h1>
        <p>
          Lax keeps its database's key in your login keychain. The keychain is locked, or Lax was
          denied the key. The key is probably fine.
        </p>
        <p className="muted">
          Try again, and when macOS asks, type your login password and choose Always Allow.
        </p>
        <p className="actions">{tryAgain}</p>
      </>
    );
  } else if (state === "keyMissing") {
    content = (
      <>
        <h1>Lax can't find its key</h1>
        <p>
          The database is encrypted, and its key, "Lax database key", isn't in your login
          keychain. Lax doesn't make a new key while the old database is there: without its own
          key, nothing in it can be read.
        </p>
        <p className="muted">
          If you reset your login password, macOS may have kept the old keychain under a new name.
          Open Keychain Access, unlock the old keychain with your old password, and try again. If
          you kept a copy of the key, the user guide says how to put it back.
        </p>
        <p className="actions">
          {tryAgain} {startOver}
        </p>
      </>
    );
  } else if (state === "cannotUnlock") {
    content = (
      <>
        <h1>Lax can't open its database</h1>
        <p>
          The key in your keychain doesn't open it. The file is damaged, or it was made with
          another key.
        </p>
        <p className="actions">
          {tryAgain} {startOver}
        </p>
      </>
    );
  } else if (state === "newerSchema") {
    content = (
      <>
        <h1>Lax can't open its database</h1>
        <p>It was made by a newer version of Lax. Build or install the newer version again.</p>
      </>
    );
  } else {
    content = (
      <>
        <h1>Lax can't open its database</h1>
        <p>
          Something went wrong with Lax's data folder or the disk. Check that the disk isn't full,
          then try again.
        </p>
        <p className="actions">{tryAgain}</p>
      </>
    );
  }

  return (
    <main className="container database">
      {content}
      <p className="muted">Nothing is captured until the database is open.</p>
      {confirming && (
        <section className="card confirm" role="alertdialog" aria-label="Start over">
          <h2>Start over with an empty database?</h2>
          <p>
            Lax puts the old database aside in its data folder, still encrypted, and keeps its old
            key in the keychain under another name, so it can still be opened if the right key
            turns up. Then Lax starts again with a new key, an empty journal and an empty memory.
          </p>
          <p className="actions">
            <button type="button" className="secondary" onClick={() => setConfirming(false)}>
              Cancel
            </button>{" "}
            <button
              type="button"
              className="danger"
              disabled={busy}
              onClick={() => {
                setConfirming(false);
                run("start_over");
              }}
            >
              Start over
            </button>
          </p>
        </section>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </main>
  );
}
