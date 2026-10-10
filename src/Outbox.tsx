import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shape returned by src-tauri/src/outbox.rs.
type OutboxItem = {
  id: number;
  sentAt: number;
  provider: string;
  task: string;
  model: string;
  preview: string;
  payloadChars: number;
  status: number;
  responseBytes: number;
};

const PAGE = 100;
const RETENTION_CHOICES = [7, 30, 90, 180, 365];

/**
 * Everything sent off this Mac (GUARDRAILS §1, §3): each request to a remote
 * model provider, exactly as sent after redaction. Payloads are captured
 * text and are shown as text only.
 */
export function Outbox() {
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<OutboxItem[] | null>(null);
  const [error, setError] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [retention, setRetention] = useState<number | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(() => {
    invoke<OutboxItem[]>("outbox_entries", { query: query.trim() || null, limit: PAGE }).then(
      (found) => {
        setError(false);
        setItems(found);
      },
      () => setError(true),
    );
  }, [query]);

  useEffect(() => {
    const timer = window.setTimeout(load, 200);
    return () => window.clearTimeout(timer);
  }, [load]);

  useEffect(() => {
    invoke<number>("outbox_retention_days").then(setRetention, () => setRetention(null));
  }, []);

  const remove = (id?: number) => {
    invoke<number>("delete_outbox", { id: id ?? null }).then(
      (n) => {
        setMessage(n === 1 ? "Deleted 1 entry." : `Deleted ${n} entries.`);
        setConfirmClear(false);
        load();
      },
      (e) => setMessage(String(e)),
    );
  };

  const changeRetention = (days: number) => {
    invoke("set_outbox_retention_days", { days }).then(
      () => {
        setRetention(days);
        setMessage(`The outbox now keeps entries for ${days} days.`);
        load();
      },
      (e) => setMessage(String(e)),
    );
  };

  return (
    <section className="outbox">
      <p className="muted">
        Everything Lax has sent to a remote AI provider, exactly as it was sent, after secrets,
        emails, phone numbers and account numbers were masked. Only you can see this log, and you
        can delete any of it.
      </p>

      <label className="search">
        <span className="visually-hidden">Search the outbox</span>
        <input
          type="search"
          placeholder="Search what was sent"
          value={query}
          maxLength={200}
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>

      {error && <p className="error">Lax couldn't read the outbox.</p>}
      {items !== null && items.length === 0 && !error && (
        <div className="card empty">
          {query.trim() ? (
            <p>Nothing sent matches “{query.trim()}”.</p>
          ) : (
            <>
              <p>
                <strong>Nothing has been sent off this Mac.</strong>
              </p>
              <p className="muted">
                Lax only uses AI models running on this Mac. If you ever turn on a remote provider,
                every request it sends is listed here.
              </p>
            </>
          )}
        </div>
      )}
      {items !== null && items.length > 0 && (
        <ul className="sent">
          {items.map((item) => (
            <SentRow key={item.id} item={item} onDelete={() => remove(item.id)} />
          ))}
        </ul>
      )}

      <h2>Keep the outbox for</h2>
      {retention === null ? (
        <p className="muted">Lax couldn't read this setting.</p>
      ) : (
        <p className="buttons">
          <select
            value={retention}
            aria-label="Days the outbox is kept"
            onChange={(e) => changeRetention(Number(e.target.value))}
          >
            {(RETENTION_CHOICES.includes(retention)
              ? RETENTION_CHOICES
              : [...RETENTION_CHOICES, retention].sort((a, b) => a - b)
            ).map((days) => (
              <option key={days} value={days}>
                {days} days
              </option>
            ))}
          </select>
          <button type="button" className="danger" onClick={() => setConfirmClear(true)}>
            Delete the whole outbox…
          </button>
        </p>
      )}
      {confirmClear && (
        <div className="confirm" role="alertdialog" aria-label="Confirm">
          <p>Delete every entry in the outbox? This can't be undone.</p>
          <p className="buttons">
            <button type="button" className="danger" onClick={() => remove()} autoFocus>
              Delete everything
            </button>
            <button type="button" className="secondary" onClick={() => setConfirmClear(false)}>
              Cancel
            </button>
          </p>
        </div>
      )}
      <p className="muted" role="status" aria-live="polite">
        {message}
      </p>
    </section>
  );
}

function SentRow({ item, onDelete }: { item: OutboxItem; onDelete: () => void }) {
  const [open, setOpen] = useState(false);
  const [payload, setPayload] = useState<string | null | "failed">(null);

  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && (payload === null || payload === "failed")) {
      invoke<string | null>("outbox_payload", { id: item.id }).then(
        (text) => setPayload(text ?? ""),
        () => setPayload("failed"),
      );
    }
  };

  return (
    <li className="capture">
      <button type="button" className="row" aria-expanded={open} onClick={toggle}>
        <span className="time">
          {new Date(item.sentAt).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}
        </span>
        <span className="title">
          {item.task} · {item.model}
        </span>
        <span className="site">{item.provider}</span>
      </button>
      {!open && <p className="preview">{item.preview}</p>}
      {open && (
        <div className="detail">
          <p className="meta muted">
            {new Date(item.sentAt).toLocaleString()} · {item.provider} · status{" "}
            {item.status || "no answer"} · {item.responseBytes} bytes back
          </p>
          {payload === null && <p className="muted">Loading…</p>}
          {payload === "failed" && <p className="error">Lax couldn't read this entry.</p>}
          {typeof payload === "string" && payload !== "failed" && <p className="text">{payload}</p>}
          <p className="buttons">
            <button type="button" className="link" onClick={onDelete}>
              Delete this entry
            </button>
          </p>
        </div>
      )}
    </li>
  );
}
