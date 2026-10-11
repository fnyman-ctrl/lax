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
  /** Seen on fewer than two days and not kept: Lax only notes it so far. */
  isNew: boolean;
};
type DontLearnItem = { kind: Kind; name: string };

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
  const [dontLearn, setDontLearn] = useState<DontLearnItem[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [forgettingAll, setForgettingAll] = useState(false);
  // The entity whose "Change" panel is open; one at a time.
  const [open, setOpen] = useState<number | null>(null);
  const confirmAllButton = useRef<HTMLButtonElement>(null);

  const refresh = useCallback(() => {
    invoke<MemoryStatus>("memory_status").then(setStatus, () => setStatus("failed"));
    invoke<EntityItem[]>("memory_entities").then(setEntities, () => setEntities("failed"));
    invoke<DontLearnItem[]>("dont_learn_list").then(setDontLearn, () => setDontLearn([]));
  }, []);

  /** Runs a change, shows what came of it, and reloads. */
  const run = (work: Promise<string>) => {
    setBusy(true);
    setMessage(null);
    return work
      .then(
        (text) => {
          setMessage(text);
          return true;
        },
        (e) => {
          setMessage(String(e));
          return false;
        },
      )
      .finally(() => {
        setBusy(false);
        refresh();
      });
  };

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
              noted? The journal entries stay, and so does the list of names not to learn. If
              you turn it on again, it starts with new entries.
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
      {Array.isArray(entities) && entities.some((e) => e.isNew) && (
        <section className="day">
          <h2>New</h2>
          <p className="muted hint">
            Seen on one day so far. Lax keeps a name once it shows up on another day, or when
            you keep it.
          </p>
          <ul className="entities">
            {entities
              .filter((e) => e.isNew)
              .map((entity) => (
                <EntityRow
                  key={entity.id}
                  entity={entity}
                  all={entities}
                  busy={busy}
                  open={open === entity.id}
                  onOpen={(on) => setOpen(on ? entity.id : null)}
                  run={run}
                />
              ))}
          </ul>
        </section>
      )}
      {Array.isArray(entities) &&
        SECTIONS.map(({ kind, title }) => {
          const list = entities.filter((e) => e.kind === kind && !e.isNew);
          return (
            list.length > 0 && (
              <section key={kind} className="day">
                <h2>{title}</h2>
                <ul className="entities">
                  {list.map((entity) => (
                    <EntityRow
                      key={entity.id}
                      entity={entity}
                      all={entities}
                      busy={busy}
                      open={open === entity.id}
                      onOpen={(on) => setOpen(on ? entity.id : null)}
                      run={run}
                    />
                  ))}
                </ul>
              </section>
            )
          );
        })}
      {dontLearn.length > 0 && (
        <section className="day">
          <h2>Not to learn</h2>
          <p className="muted hint">
            You asked Lax not to learn about these. It skips them in new journal entries.
          </p>
          <ul className="entities">
            {dontLearn.map((item) => (
              <li key={`${item.kind} ${item.name}`} className="entity">
                <div>
                  <p className="name">{item.name}</p>
                  <p className="muted seen">{item.kind}</p>
                </div>
                <button
                  type="button"
                  className="secondary"
                  disabled={busy}
                  aria-label={`Allow Lax to learn about ${item.name} again`}
                  onClick={() =>
                    run(
                      invoke<boolean>("allow_learning", { kind: item.kind, name: item.name }).then(
                        () => `Lax may learn about ${item.name} again, from new entries.`,
                      ),
                    )
                  }
                >
                  Allow again
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </section>
  );
}

/**
 * One entity, with "Keep" while it is new and a "Change" panel to rename it,
 * merge it into another of its kind, or forget it.
 */
function EntityRow({
  entity,
  all,
  busy,
  open,
  onOpen,
  run,
}: {
  entity: EntityItem;
  all: EntityItem[];
  busy: boolean;
  open: boolean;
  onOpen: (open: boolean) => void;
  run: (work: Promise<string>) => Promise<boolean>;
}) {
  const [name, setName] = useState(entity.name);
  const [into, setInto] = useState("");
  // New names were most likely noise, so forgetting one also keeps it from
  // coming back, unless the user unticks it.
  const [dontLearn, setDontLearn] = useState(entity.isNew);
  const [confirming, setConfirming] = useState(false);
  const nameInput = useRef<HTMLInputElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const others = all
    .filter((e) => e.kind === entity.kind && e.id !== entity.id)
    .sort((a, b) => a.name.localeCompare(b.name));

  useEffect(() => {
    if (open) {
      setName(entity.name);
      setInto("");
      setDontLearn(entity.isNew);
      setConfirming(false);
      nameInput.current?.focus();
    }
    // Only when the panel opens, not on every refresh.
  }, [open]);
  useEffect(() => {
    if (confirming) {
      confirmButton.current?.focus();
    }
  }, [confirming]);

  const rename = () =>
    run(
      invoke("rename_entity", { id: entity.id, name }).then(() => `Renamed to ${name.trim()}.`),
    ).then((done) => done && onOpen(false));

  const merge = () => {
    const target = others.find((e) => String(e.id) === into);
    if (!target) {
      setInto("");
      run(Promise.reject("That one is no longer in the memory."));
      return;
    }
    run(
      invoke("merge_entities", { from: entity.id, into: target.id }).then(
        () => `${entity.name} is now one of ${target.name}'s names.`,
      ),
    ).then((done) => done && onOpen(false));
  };

  const forget = () =>
    run(
      invoke<boolean>("forget_entity", { id: entity.id, dontLearn }).then((existed) =>
        !existed
          ? "It was already gone."
          : dontLearn
            ? `Forgot ${entity.name}, and Lax won't learn about it again.`
            : `Forgot ${entity.name}.`,
      ),
    ).then((done) => done && onOpen(false));

  return (
    <li className="entity">
      <div>
        <p className="name">
          {entity.name}
          {entity.isNew && <span className="muted"> · {entity.kind}</span>}
        </p>
        {entity.aliases.length > 0 && (
          <p className="muted aliases">Also: {entity.aliases.join(", ")}</p>
        )}
        <p className="muted seen">{seen(entity)}</p>
      </div>
      <p className="buttons">
        {entity.isNew && (
          <button
            type="button"
            className="secondary"
            disabled={busy}
            aria-label={`Keep ${entity.name}`}
            onClick={() =>
              run(invoke("keep_entity", { id: entity.id }).then(() => `Kept ${entity.name}.`))
            }
          >
            Keep
          </button>
        )}
        <button
          type="button"
          className="secondary"
          aria-expanded={open}
          aria-label={`Change ${entity.name}`}
          onClick={() => onOpen(!open)}
        >
          {open ? "Close" : "Change…"}
        </button>
      </p>
      {open && (
        <div className="change">
          <p className="buttons">
            <input
              ref={nameInput}
              type="text"
              value={name}
              maxLength={100}
              aria-label="Name"
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && rename()}
            />
            <button
              type="button"
              disabled={busy || name.trim() === "" || name.trim() === entity.name}
              onClick={rename}
            >
              Rename
            </button>
          </p>
          {others.length > 0 && (
            <p className="buttons">
              <label htmlFor={`same-${entity.id}`}>Same as</label>
              <select
                id={`same-${entity.id}`}
                value={into}
                onChange={(e) => setInto(e.target.value)}
              >
                <option value="">Choose…</option>
                {others.map((e) => (
                  <option key={e.id} value={e.id}>
                    {e.name}
                  </option>
                ))}
              </select>
              <button type="button" disabled={busy || into === ""} onClick={merge}>
                Merge into it
              </button>
            </p>
          )}
          {others.length > 0 && (
            <p className="muted hint">
              Merging keeps the chosen name, and {entity.name} becomes one of its names. You can
              rename it afterwards.
            </p>
          )}
          <p className="buttons">
            <label>
              <input
                type="checkbox"
                checked={dontLearn}
                onChange={(e) => setDontLearn(e.target.checked)}
              />{" "}
              Don't learn about it again
            </label>
            <button
              type="button"
              className="secondary"
              disabled={busy}
              onClick={() => setConfirming(true)}
            >
              Forget…
            </button>
          </p>
          {confirming && (
            <div className="confirm" role="alertdialog" aria-label="Confirm">
              <p>
                Forget {entity.name}? Lax removes the name and where it was mentioned. The
                journal entries stay.{" "}
                {dontLearn
                  ? "Lax won't learn about it again until you allow it."
                  : "If a new entry names it again, Lax starts over."}
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
                  onClick={() => setConfirming(false)}
                >
                  Cancel
                </button>
              </p>
            </div>
          )}
        </div>
      )}
    </li>
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
