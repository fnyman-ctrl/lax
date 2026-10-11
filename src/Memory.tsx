import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shapes returned by src-tauri/src/memory.rs.
type MemoryStatus = { enabled: boolean; model: string | null; waiting: number; journal: boolean };
type Kind = "person" | "organisation" | "project";
type EntityItem = {
  id: number;
  kind: Kind;
  name: string;
  aliases: string[];
  mentions: number;
  days: number;
  lastSeen: number;
};

const REFRESH_MS = 30_000;

const SECTIONS: { kind: Kind; title: string }[] = [
  { kind: "person", title: "People" },
  { kind: "organisation", title: "Organisations" },
  { kind: "project", title: "Projects" },
];

/**
 * The memory (Stage 4, ADR 0008): the people, organisations and projects
 * the journal's entries name. Off until the user turns it on. Names come
 * from captured text through a model, so they are shown as text only.
 */
export function Memory({ onShowModels }: { onShowModels: () => void }) {
  const [status, setStatus] = useState<MemoryStatus | null | "failed">(null);
  const [entities, setEntities] = useState<EntityItem[] | null | "failed">(null);
  const [message, setMessage] = useState<string | null>(null);
  const [pending, setPending] = useState<EntityItem | null>(null);
  const [busy, setBusy] = useState(false);
  const [forgettingAll, setForgettingAll] = useState(false);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const confirmAllButton = useRef<HTMLButtonElement>(null);

  const refresh = useCallback(() => {
    invoke<MemoryStatus>("memory_status").then(setStatus, () => setStatus("failed"));
    invoke<EntityItem[]>("memory_entities").then(setEntities, () => setEntities("failed"));
  }, []);

  useEffect(() => {
    refresh();
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") {
        refresh();
      }
    }, REFRESH_MS);
    window.addEventListener("focus", refresh);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  // Move focus to the confirm button, so keyboard and VoiceOver users land
  // on the question.
  useEffect(() => {
    if (pending) {
      confirmButton.current?.focus();
    }
  }, [pending]);
  useEffect(() => {
    if (forgettingAll) {
      confirmAllButton.current?.focus();
    }
  }, [forgettingAll]);

  const forgetAll = () => {
    setBusy(true);
    setMessage(null);
    invoke<number>("forget_memory")
      .then(
        (n) =>
          setMessage(
            n === 0
              ? "The memory is off. There was nothing to forget."
              : `The memory is off, and Lax forgot ${n} ${n === 1 ? "name" : "names"}.`,
          ),
        (e) => setMessage(String(e)),
      )
      .finally(() => {
        setBusy(false);
        setForgettingAll(false);
        refresh();
      });
  };

  const toggle = (enabled: boolean) => {
    setMessage(null);
    invoke<MemoryStatus>("set_memory_enabled", { enabled }).then(setStatus, (e) =>
      setMessage(String(e)),
    );
  };

  const forget = () => {
    if (!pending) {
      return;
    }
    setBusy(true);
    setMessage(null);
    invoke<boolean>("forget_entity", { id: pending.id })
      .then(
        (existed) => setMessage(existed ? `Forgot ${pending.name}.` : "It was already gone."),
        (e) => setMessage(String(e)),
      )
      .finally(() => {
        setBusy(false);
        setPending(null);
        refresh();
      });
  };

  if (status === null) {
    return null;
  }
  if (status === "failed") {
    return <p className="error">Lax couldn't read the memory.</p>;
  }

  return (
    <section className="memory">
      <div className="card memory-status">
        {status.enabled ? (
          <>
            <p>
              <strong>The memory is on.</strong> {status.model ?? "Your model"} on this Mac reads
              each journal entry and notes the people, organisations and projects in it.{" "}
              {status.waiting > 0 && (
                <span className="muted">
                  {status.waiting} {status.waiting === 1 ? "entry waits" : "entries wait"} to be
                  read.
                </span>
              )}
            </p>
            {!status.journal && (
              <p className="muted">The journal is off, so no new entries arrive.</p>
            )}
            <p className="buttons">
              <button type="button" className="secondary" onClick={() => toggle(false)}>
                Turn the memory off
              </button>
              <button
                type="button"
                className="secondary"
                disabled={busy}
                onClick={() => {
                  setMessage(null);
                  setForgettingAll(true);
                }}
              >
                Turn off and forget what it noted…
              </button>
            </p>
          </>
        ) : (
          <>
            <p>
              <strong>Let Lax learn who you work with.</strong> An AI model on this Mac reads the
              journal's entries and notes the people, organisations and projects they name, so
              Lax can learn about them. Nothing leaves this Mac, it stops while capture is paused,
              and you can forget anything it noted.
            </p>
            {status.model ? (
              <p className="buttons">
                <button type="button" onClick={() => toggle(true)}>
                  Turn the memory on
                </button>
                <span className="muted">Uses {status.model}.</span>
                {Array.isArray(entities) && entities.length > 0 && (
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => {
                      setMessage(null);
                      setForgettingAll(true);
                    }}
                  >
                    Forget what it noted…
                  </button>
                )}
              </p>
            ) : (
              <p className="buttons">
                <button type="button" onClick={onShowModels}>
                  Choose a model first
                </button>
                <span className="muted">The memory needs a chat model on this Mac.</span>
              </p>
            )}
          </>
        )}
        {forgettingAll && (
          <div className="confirm" role="alertdialog" aria-label="Confirm">
            <p>
              {status.enabled ? "Turn the memory off and forget" : "Forget"} every name it
              noted? The journal entries stay. If you turn it on again, it starts with new
              entries.
            </p>
            <p className="buttons">
              <button
                type="button"
                className="danger"
                disabled={busy}
                onClick={forgetAll}
                ref={confirmAllButton}
              >
                {busy ? "Forgetting…" : status.enabled ? "Turn off and forget" : "Forget"}
              </button>
              <button
                type="button"
                className="secondary"
                disabled={busy}
                onClick={() => setForgettingAll(false)}
              >
                Cancel
              </button>
            </p>
          </div>
        )}
      </div>

      <p className="muted" role="status" aria-live="polite">
        {message}
      </p>

      {entities === "failed" && <p className="error">Lax couldn't read what it has noted.</p>}
      {Array.isArray(entities) && entities.length === 0 && status.enabled && (
        <p className="muted empty">
          Nothing noted yet. Names appear here a few minutes after the journal writes an entry.
        </p>
      )}
      {Array.isArray(entities) &&
        SECTIONS.map(({ kind, title }) => {
          const list = entities.filter((e) => e.kind === kind);
          return (
            list.length > 0 && (
              <section key={kind} className="day">
                <h2>{title}</h2>
                <ul className="entities">
                  {list.map((entity) => (
                    <li key={entity.id} className="entity">
                      <div>
                        <p className="name">{entity.name}</p>
                        {entity.aliases.length > 0 && (
                          <p className="muted aliases">Also: {entity.aliases.join(", ")}</p>
                        )}
                        <p className="muted seen">{seen(entity)}</p>
                      </div>
                      <button
                        type="button"
                        className="secondary"
                        disabled={busy}
                        onClick={() => {
                          setMessage(null);
                          setPending(entity);
                        }}
                        aria-label={`Forget ${entity.name}`}
                      >
                        Forget
                      </button>
                      {pending?.id === entity.id && (
                        <div className="confirm" role="alertdialog" aria-label="Confirm">
                          <p>
                            Forget {entity.name}? Lax removes the name and where it was
                            mentioned. The journal entries stay, and if a new entry names it
                            again, Lax starts over.
                          </p>
                          <p className="buttons">
                            <button
                              type="button"
                              className="danger"
                              disabled={busy}
                              onClick={forget}
                              ref={confirmButton}
                            >
                              {busy ? "Forgetting…" : "Forget"}
                            </button>
                            <button
                              type="button"
                              className="secondary"
                              disabled={busy}
                              onClick={() => setPending(null)}
                            >
                              Cancel
                            </button>
                          </p>
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              </section>
            )
          );
        })}
    </section>
  );
}

function seen(entity: EntityItem): string {
  const entries = `${entity.mentions} ${entity.mentions === 1 ? "entry" : "entries"}`;
  const days = entity.days === 1 ? "one day" : `${entity.days} days`;
  const last = new Date(entity.lastSeen).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
  });
  return `In ${entries} on ${days}, last ${last}`;
}
