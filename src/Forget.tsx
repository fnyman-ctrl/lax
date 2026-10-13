import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shapes used by src-tauri/src/forget.rs.
type ForgetRequest =
  | { kind: "last"; minutes: number }
  | { kind: "range"; from: number; to: number }
  | { kind: "app"; bundleId: string };

type AppItem = { bundleId: string; appName: string; captures: number };

/** An action waiting for the user to confirm it. */
type Pending = { question: string; button: string; run: () => Promise<string> };

const RETENTION_CHOICES = [7, 30, 90, 180, 365];

/**
 * Forget a time range or an app, delete everything, and choose how long raw
 * captures are kept (GUARDRAILS §4). Every deletion asks once more first.
 */
export function Forget({ onForgotten }: { onForgotten: () => void }) {
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [apps, setApps] = useState<AppItem[]>([]);
  const [app, setApp] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [retention, setRetention] = useState<number | null>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);

  // Move focus to the confirm button, so keyboard and VoiceOver users land
  // on the question.
  useEffect(() => {
    if (pending) {
      confirmButton.current?.focus();
    }
  }, [pending]);

  const loadApps = () => {
    invoke<AppItem[]>("captured_apps").then(
      (list) => {
        setApps(list);
        setApp((current) => (list.some((a) => a.bundleId === current) ? current : ""));
      },
      () => setApps([]),
    );
  };

  useEffect(() => {
    loadApps();
    invoke<number>("retention_days").then(setRetention, () => setRetention(null));
  }, []);

  const ask = (question: string, button: string, run: () => Promise<string>) => {
    setMessage(null);
    setPending({ question, button, run });
  };

  const confirm = () => {
    if (!pending) {
      return;
    }
    setBusy(true);
    pending
      .run()
      .then(
        (text) => setMessage({ text, error: false }),
        (error) => setMessage({ text: String(error), error: true }),
      )
      .finally(() => {
        // Also after an error: part of it may have been deleted.
        onForgotten();
        loadApps();
        setBusy(false);
        setPending(null);
      });
  };

  const forget = (what: ForgetRequest) =>
    invoke<number>("forget_captures", { what }).then(forgotten);

  const askLast = (minutes: number, label: string) =>
    ask(`Forget everything from the ${label}?`, "Forget", () =>
      forget({ kind: "last", minutes }),
    );

  const askRange = () => {
    const start = new Date(from).getTime();
    const end = new Date(to).getTime();
    if (Number.isNaN(start) || Number.isNaN(end) || start >= end) {
      setMessage({ text: "Choose a start before the end.", error: true });
      return;
    }
    ask(
      `Forget everything from ${new Date(start).toLocaleString()} to ${new Date(end).toLocaleString()}?`,
      "Forget",
      () => forget({ kind: "range", from: start, to: end }),
    );
  };

  const askApp = () => {
    const chosen = apps.find((a) => a.bundleId === app);
    if (!chosen) {
      setMessage({ text: "Choose an app.", error: true });
      return;
    }
    ask(
      `Forget everything from ${chosen.appName || chosen.bundleId} (${plural(chosen.captures)})?`,
      "Forget",
      () => forget({ kind: "app", bundleId: chosen.bundleId }),
    );
  };

  const askDeleteAll = () =>
    ask(
      "Delete everything Lax has captured, and its logs? This can't be undone.",
      "Delete everything",
      () => invoke<number>("delete_all_data").then((n) => `Deleted everything (${plural(n)}).`),
    );

  const changeRetention = (days: number) => {
    if (retention !== null && days < retention) {
      ask(
        `Keep raw captures for ${days} days? Captures older than that are deleted now.`,
        "Change and delete",
        () => saveRetention(days),
      );
    } else {
      saveRetention(days).then(
        (text) => setMessage({ text, error: false }),
        (error) => setMessage({ text: String(error), error: true }),
      );
    }
  };

  const saveRetention = (days: number) =>
    invoke("set_retention_days", { days }).then(() => {
      setRetention(days);
      return `Raw captures are now kept for ${days} days.`;
    });

  const choices =
    retention === null || RETENTION_CHOICES.includes(retention)
      ? RETENTION_CHOICES
      : [...RETENTION_CHOICES, retention].sort((a, b) => a - b);

  return (
    <details className="forget">
      <summary>Forget or delete…</summary>
      <p className="muted">
        Deleted captures are gone right away, also from the files on disk. Forgetting doesn't
        stop capture; to stop it, pause from the menu bar.
      </p>

      <h3>Recent activity</h3>
      <p className="buttons">
        <button type="button" className="secondary" disabled={busy} onClick={() => askLast(15, "last 15 minutes")}>
          Last 15 minutes
        </button>
        <button type="button" className="secondary" disabled={busy} onClick={() => askLast(60, "last hour")}>
          Last hour
        </button>
        <button type="button" className="secondary" disabled={busy} onClick={() => askLast(24 * 60, "last 24 hours")}>
          Last 24 hours
        </button>
      </p>

      <h3>A time range</h3>
      <p className="buttons">
        <label>
          From <input type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
        </label>
        <label>
          to <input type="datetime-local" value={to} onChange={(e) => setTo(e.target.value)} />
        </label>
        <button type="button" className="secondary" disabled={busy || !from || !to} onClick={askRange}>
          Forget range
        </button>
      </p>

      <h3>An app</h3>
      {apps.length === 0 ? (
        <p className="muted">No captures yet.</p>
      ) : (
        <p className="buttons">
          <select value={app} onChange={(e) => setApp(e.target.value)} aria-label="App to forget">
            <option value="">Choose an app</option>
            {apps.map((a) => (
              <option key={a.bundleId} value={a.bundleId}>
                {a.appName || a.bundleId} ({plural(a.captures)})
              </option>
            ))}
          </select>
          <button type="button" className="secondary" disabled={busy || !app} onClick={askApp}>
            Forget app
          </button>
        </p>
      )}

      <h3>How long raw captures are kept</h3>
      <p className="muted">
        Once the journal has summarized them, raw captures are deleted after 30 days; the
        journal and the memory keep what was learned. Captures not yet summarized are kept
        until the limit below.
      </p>
      {retention === null ? (
        <p className="muted">Lax couldn't read this setting.</p>
      ) : (
        <p className="buttons">
          <select
            value={retention}
            disabled={busy}
            aria-label="Days raw captures are kept at most"
            onChange={(e) => changeRetention(Number(e.target.value))}
          >
            {choices.map((days) => (
              <option key={days} value={days}>
                {days} days
              </option>
            ))}
          </select>
        </p>
      )}

      <h3>Everything</h3>
      <p className="buttons">
        <button type="button" className="danger" disabled={busy} onClick={askDeleteAll}>
          Delete all data…
        </button>
      </p>

      {pending && (
        <div className="confirm" role="alertdialog" aria-label="Confirm">
          <p>{pending.question}</p>
          <p className="buttons">
            <button
              type="button"
              className="danger"
              disabled={busy}
              onClick={confirm}
              ref={confirmButton}
            >
              {busy ? "Deleting…" : pending.button}
            </button>
            <button type="button" className="secondary" disabled={busy} onClick={() => setPending(null)}>
              Cancel
            </button>
          </p>
        </div>
      )}
      <p className={message?.error ? "error" : "muted"} role="status" aria-live="polite">
        {message?.text}
      </p>
    </details>
  );
}

function forgotten(n: number): string {
  return n === 0 ? "There was nothing to forget." : `Forgot ${plural(n)}.`;
}

function plural(n: number): string {
  return `${n} ${n === 1 ? "capture" : "captures"}`;
}
