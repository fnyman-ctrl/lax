import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ExportPages } from "./ExportPages";
import { PageEditor } from "./PageEditor";

// Shapes returned by src-tauri/src/memory.rs.
type MemoryStatus = {
  enabled: boolean;
  model: string | null;
  waiting: number;
  journal: boolean;
  folder: string | null;
  folderProblem: string | null;
  ownNames: string[];
  aboutMe: PageItem | null;
  /** The pages are in the encrypted database and edited here (ADR 0011). */
  pagesInDatabase: boolean;
  /** The old page files in ~/Lax after the move, while there are any. */
  oldPageFiles: { count: number; kept: boolean } | null;
};
type PageItem = { path: string; state: "ok" | "missing" | "detached" | "recreate" };
type Kind = "person" | "organisation" | "project";
export type EntityItem = {
  id: number;
  kind: Kind;
  name: string;
  aliases: string[];
  mentions: number;
  days: number;
  lastSeen: number;
  /** Seen on fewer than two days and not kept: Lax only notes it so far. */
  isNew: boolean;
  page: PageItem | null;
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
  const [exportingAll, setExportingAll] = useState(false);
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
            {status.pagesInDatabase ? (
              <>
                <p className="buttons">
                  <span className="muted">
                    Lax keeps a page for each name it's sure of, in its encrypted database. Write
                    in them freely: Lax only changes its own part.
                  </span>
                  <button
                    type="button"
                    className="secondary"
                    onClick={() => setExportingAll(true)}
                  >
                    Export pages…
                  </button>
                </p>
                {exportingAll && (
                  <ExportPages scope="all" onClose={() => setExportingAll(false)} />
                )}
              </>
            ) : status.folderProblem ? (
              <p className="error">{status.folderProblem}</p>
            ) : (
              status.folder && (
                <p className="buttons">
                  <span className="muted">
                    Lax keeps a page for each name it's sure of in {status.folder}. Write in them
                    freely: Lax only changes its own part.
                  </span>
                  <button
                    type="button"
                    className="secondary"
                    onClick={() =>
                      invoke("open_memory_folder").catch((e) => setMessage(String(e)))
                    }
                  >
                    Open in Finder
                  </button>
                </p>
              )
            )}
            {status.oldPageFiles && (
              <OldPageFiles
                files={status.oldPageFiles}
                busy={busy}
                run={run}
                onKept={setStatus}
              />
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
              noted?{" "}
              {status.pagesInDatabase
                ? "All its pages go too, your page and what you wrote on them included: export them first if you want to keep them."
                : "Lax's part of the pages goes too; anything you wrote on them stays."}{" "}
              The journal entries stay, and so does the list of names not to learn. If you turn
              it on again, it starts with new entries.
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

      {status.enabled && (
        <AboutYou
          status={status}
          busy={busy}
          onChanged={setStatus}
          run={run}
          onMessage={setMessage}
        />
      )}

      <p className="muted" role="status" aria-live="polite">
        {message}
      </p>

      {entities === "failed" && <p className="error">Lax couldn't read what it has noted.</p>}
      {Array.isArray(entities) && entities.length === 0 && status.enabled && (
        <p className="muted empty">
          Nothing noted yet. Names appear here a few minutes after the journal writes an entry.
        </p>
      )}
      {Array.isArray(entities) && (
        <NewNames
          entities={entities}
          row={(entity) => (
            <EntityRow
              key={entity.id}
              entity={entity}
              all={entities}
              editable={status.pagesInDatabase}
              busy={busy}
              open={open === entity.id}
              onOpen={(on) => setOpen(on ? entity.id : null)}
              run={run}
            />
          )}
        />
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
                      editable={status.pagesInDatabase}
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
 * About the user: other names they go by (kept out of the memory's people)
 * and `about-me.md`.
 */
function AboutYou({
  status,
  busy,
  onChanged,
  run,
  onMessage,
}: {
  status: MemoryStatus;
  busy: boolean;
  onChanged: (status: MemoryStatus) => void;
  run: (work: Promise<string>) => Promise<boolean>;
  onMessage: (message: string | null) => void;
}) {
  const [names, setNames] = useState(status.ownNames.join(", "));
  const [editing, setEditing] = useState(false);
  const saved = status.ownNames.join(", ");

  const save = () => {
    const list = names
      .split(",")
      .map((n) => n.trim())
      .filter((n) => n !== "");
    onMessage(null);
    invoke<MemoryStatus>("set_own_names", { names: list }).then(
      (next) => {
        onChanged(next);
        setNames(next.ownNames.join(", "));
        onMessage("Saved. Lax won't note these names as people, and forgot any it had.");
      },
      (e) => onMessage(String(e)),
    );
  };

  return (
    <div className="card about-you">
      <h2>About you</h2>
      <p className="muted hint">
        Lax keeps your own name from this Mac out of the people it notes. Add other names you go
        by, separated by commas.
      </p>
      <p className="buttons">
        <input
          type="text"
          value={names}
          maxLength={1000}
          placeholder="Other names, e.g. a nickname"
          aria-label="Other names you go by"
          onChange={(e) => setNames(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && save()}
        />
        <button type="button" className="secondary" disabled={names === saved} onClick={save}>
          Save
        </button>
      </p>
      {status.aboutMe?.state === "ok" && !status.pagesInDatabase && (
        <p className="muted hint">
          {status.aboutMe.path} says who and what you work with most, and what Lax learned about
          your work.
        </p>
      )}
      {status.pagesInDatabase && status.aboutMe && status.aboutMe.state !== "missing" && (
        <>
          <p className="buttons">
            <span className="muted">
              Your page says who and what you work with most, and what Lax learned about your work.
            </span>
            <button type="button" className="secondary" onClick={() => setEditing(!editing)}>
              {editing ? "Close your page" : "Edit your page"}
            </button>
          </p>
          {editing && <PageEditor entity={null} onClose={() => setEditing(false)} />}
        </>
      )}
      {(status.aboutMe?.state === "missing" || status.aboutMe?.state === "detached") && (
        <p className="buttons page-problem">
          <span>
            {status.aboutMe.state === "missing"
              ? "Lax can't find about-me.md. It won't write it again unless you ask."
              : "Lax's part was removed from about-me.md, so Lax leaves that file alone."}
          </span>
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() =>
              run(invoke("recreate_about_me").then(() => "Wrote about-me.md again."))
            }
          >
            {status.aboutMe.state === "missing" ? "Recreate page" : "Write a new page"}
          </button>
        </p>
      )}
    </div>
  );
}

/**
 * After the pages moved into the database (ADR 0011): asks once whether to
 * delete the old files in ~/Lax, with Delete as the default; after "Keep
 * them", a quiet line that still offers it. Only files that still hold
 * exactly what was moved are deleted.
 */
export function OldPageFiles({
  files,
  busy,
  run,
  onKept,
}: {
  files: { count: number; kept: boolean };
  busy: boolean;
  run: (work: Promise<string>) => Promise<boolean>;
  onKept: (status: MemoryStatus) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const remove = () =>
    run(
      invoke<{ deleted: number; changed: string[] }>("delete_old_page_files").then((done) => {
        const deleted = `Deleted ${done.deleted} old ${done.deleted === 1 ? "file" : "files"}.`;
        return done.changed.length === 0
          ? deleted
          : `${deleted} Kept ${done.changed.join(", ")}: changed after the move.`;
      }),
    );
  if (files.kept && !confirming) {
    return (
      <p className="buttons">
        <span className="muted">Your old page files are still in ~/Lax.</span>
        <button type="button" className="secondary" disabled={busy} onClick={() => setConfirming(true)}>
          Delete the old files
        </button>
      </p>
    );
  }
  return (
    <section className="card confirm" role="alertdialog" aria-label="The old page files">
      <h3>Your pages are now in Lax's encrypted database. Delete the old files in ~/Lax?</h3>
      <ul>
        <li>Edits to the old files are no longer read.</li>
        <li>
          Deleting doesn't reliably erase the old text from the disk: on APFS and SSDs, old blocks
          can stay until they're reused. FileVault, if it's on, keeps them unreadable. Time Machine
          backups and local snapshots keep the files until they expire.
        </li>
        <li>If you keep them, they stay readable by every program, as now.</li>
      </ul>
      <p className="muted">
        Only the {files.count} {files.count === 1 ? "file" : "files"} Lax moved, and only if they
        still hold what was moved. Your own files stay.
      </p>
      <p className="actions">
        <button type="button" className="danger" disabled={busy} autoFocus onClick={remove}>
          Delete the old files
        </button>{" "}
        {files.kept ? (
          <button type="button" className="secondary" onClick={() => setConfirming(false)}>
            Cancel
          </button>
        ) : (
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() =>
              run(
                invoke<MemoryStatus>("keep_old_page_files").then((status) => {
                  onKept(status);
                  return "Kept the old files. You can still delete them here.";
                }),
              )
            }
          >
            Keep them
          </button>
        )}
      </p>
    </section>
  );
}

/**
 * The names Lax only notes so far, folded away (ADR 0013): they don't need
 * the user. Opened, most mentioned first, as the list already comes.
 */
export function NewNames({
  entities,
  row,
}: {
  entities: EntityItem[];
  row: (entity: EntityItem) => ReactNode;
}) {
  const fresh = entities.filter((e) => e.isNew);
  if (fresh.length === 0) return null;
  const count = fresh.length === 1 ? "one new name" : `${fresh.length} new names`;
  return (
    <section className="day">
      <h2>New</h2>
      <details className="new-names">
        <summary>
          Lax is watching {count}. They get a page when they come back on another day, and
          leave after two weeks if they don't.
        </summary>
        <ul className="entities">{fresh.map(row)}</ul>
      </details>
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
  editable,
  busy,
  open,
  onOpen,
  run,
}: {
  entity: EntityItem;
  all: EntityItem[];
  /** The page can be edited here (the pages are in the database). */
  editable: boolean;
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
  const [editing, setEditing] = useState(false);
  // The user's own text on the page, shown before a forget deletes it
  // with the page (ADR 0011); `undefined` until read.
  const [notes, setNotes] = useState<string | null | undefined>(undefined);
  const [exporting, setExporting] = useState(false);
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

  useEffect(() => {
    if (confirming && editable && entity.page) {
      invoke<string | null>("page_notes", { entity: entity.id }).then(setNotes, () => setNotes(null));
    }
  }, [confirming, editable, entity.id, entity.page]);

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
        {entity.page?.state === "ok" && !editable && (
          <p className="muted seen">Page: {entity.page.path}</p>
        )}
        {editable && entity.page && entity.page.state !== "missing" && (
          <p className="buttons">
            <button type="button" className="secondary" onClick={() => setEditing(!editing)}>
              {editing ? "Close the page" : "Edit page"}
            </button>
          </p>
        )}
        {editing && <PageEditor entity={entity.id} onClose={() => setEditing(false)} />}
        {(entity.page?.state === "missing" || entity.page?.state === "detached") && (
          <p className="buttons page-problem">
            <span>
              {entity.page.state === "missing"
                ? editable
                  ? "This page has no text, so Lax doesn't write it. It won't write it again unless you ask."
                  : "Lax can't find its page in the memory folder. It won't write it again unless you ask."
                : editable
                  ? "Lax's part was removed from this page, so Lax leaves it alone."
                  : `Lax's part was removed from ${entity.page.path}, so Lax leaves that file alone.`}
            </span>
            <button
              type="button"
              className="secondary"
              disabled={busy}
              onClick={() =>
                run(
                  invoke("recreate_page", { id: entity.id }).then(
                    () => `Wrote a page for ${entity.name} again.`,
                  ),
                )
              }
            >
              {entity.page.state === "missing" ? "Recreate page" : "Write a new page"}
            </button>
          </p>
        )}
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
              Merging keeps the chosen name, and {entity.name} becomes one of its names. What you
              wrote on its page moves to the chosen one's page. You can rename it afterwards.
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
          {entity.page && <PageHistory entityId={entity.id} busy={busy} run={run} />}
          {confirming && (
            <div className="confirm" role="alertdialog" aria-label="Confirm">
              <p>
                Forget {entity.name}? Lax removes the name and where it was mentioned. The
                journal entries stay.{" "}
                {entity.page &&
                  !editable &&
                  "Lax's part of its page goes too; anything you wrote there stays. "}
                {entity.page && editable && !notes && "Its page goes too. "}
                {dontLearn
                  ? "Lax won't learn about it again until you allow it."
                  : "If a new entry names it again, Lax starts over."}
              </p>
              {editable && notes && (
                <>
                  <p>Its page goes too, with what you wrote on it:</p>
                  <pre className="notes">{notes}</pre>
                  {exporting ? (
                    <ExportPages scope={{ entity: entity.id }} onClose={() => setExporting(false)} />
                  ) : (
                    <p className="buttons">
                      <button type="button" className="secondary" onClick={() => setExporting(true)}>
                        Export it first
                      </button>
                    </p>
                  )}
                </>
              )}
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

type VersionItem = {
  id: number;
  writtenAt: number;
  byLax: boolean;
  block: string;
  facts: string[];
};

/**
 * What Lax wrote on a page over time, and the user's edits it found in its
 * part, with a way to put back earlier facts. Loaded when opened.
 */
function PageHistory({
  entityId,
  busy,
  run,
}: {
  entityId: number;
  busy: boolean;
  run: (work: Promise<string>) => Promise<boolean>;
}) {
  const [versions, setVersions] = useState<VersionItem[] | null | "failed">(null);

  const load = () =>
    invoke<VersionItem[]>("page_history", { id: entityId }).then(setVersions, () =>
      setVersions("failed"),
    );
  // The newest of Lax's versions is what its part of the page says now.
  const current = Array.isArray(versions) ? versions.find((v) => v.byLax)?.id : undefined;

  return (
    <details
      className="history"
      onToggle={(e) => {
        if ((e.target as HTMLDetailsElement).open) {
          load();
        }
      }}
    >
      <summary>History</summary>
      {versions === "failed" && <p className="error">Lax couldn't read the history.</p>}
      {Array.isArray(versions) && versions.length === 0 && (
        <p className="muted">No versions yet.</p>
      )}
      {Array.isArray(versions) && (
        <ol className="versions">
          {versions.map((version) => (
            <li key={version.id}>
              <p className="muted when">
                {new Date(version.writtenAt).toLocaleString(undefined, {
                  day: "numeric",
                  month: "short",
                  hour: "2-digit",
                  minute: "2-digit",
                })}{" "}
                · {version.byLax ? "Lax" : "your edit"}
                {version.id === current && " · on the page now"}
              </p>
              <pre className="block">{version.block}</pre>
              {version.byLax && version.id !== current && version.facts.length > 0 && (
                <p className="buttons">
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      run(
                        invoke("restore_page_version", { version: version.id }).then(
                          () =>
                            "Put back those facts. The page shows them again, and the facts it took away won't be written again.",
                        ),
                      ).then(load)
                    }
                  >
                    Restore these facts
                  </button>
                </p>
              )}
            </li>
          ))}
        </ol>
      )}
    </details>
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
