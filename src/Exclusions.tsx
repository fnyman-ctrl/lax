import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shapes used by src-tauri/src/exclusion_settings.rs and forget.rs.
type Kind = "app" | "site" | "title";

type ExclusionSettings = {
  apps: string[];
  sites: string[];
  titles: string[];
  defaults: { name: string; category: "banking" | "health"; excluded: boolean }[];
  always: string[];
};

type AppItem = { bundleId: string; appName: string; captures: number };

/**
 * What Lax doesn't capture (GUARDRAILS §4): always excluded (tier 1),
 * banking and health switches (tier 2), and the user's own apps, websites
 * and title patterns (tier 3). Changes apply before the next capture.
 */
export function Exclusions({ onForgotten }: { onForgotten: () => void }) {
  const [settings, setSettings] = useState<ExclusionSettings | null | "failed">(null);
  const [apps, setApps] = useState<AppItem[]>([]);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [busy, setBusy] = useState(false);

  const loadApps = () => {
    invoke<AppItem[]>("captured_apps").then(setApps, () => setApps([]));
  };

  useEffect(() => {
    invoke<ExclusionSettings>("exclusion_settings").then(setSettings, () => setSettings("failed"));
    loadApps();
  }, []);

  /** Runs a change and shows the settings it returns. */
  const run = (command: string, args: Record<string, unknown>, done: string) => {
    setBusy(true);
    setMessage(null);
    return invoke<ExclusionSettings>(command, args)
      .then(
        (next) => {
          setSettings(next);
          setMessage({ text: done, error: false });
          return true;
        },
        (error) => {
          setMessage({ text: String(error), error: true });
          return false;
        },
      )
      .finally(() => setBusy(false));
  };

  const add = (kind: Kind, value: string, forgetApp?: string) =>
    run("add_exclusion", { kind, value }, "Excluded. Lax won't capture it from now on.").then(
      (ok) => {
        if (ok && forgetApp) {
          invoke<number>("forget_captures", { what: { kind: "app", bundleId: forgetApp } }).then(
            (n) => {
              setMessage({
                text: `Excluded, and forgot ${n} ${n === 1 ? "capture" : "captures"} from it.`,
                error: false,
              });
              onForgotten();
              loadApps();
            },
            (error) => setMessage({ text: String(error), error: true }),
          );
        }
        return ok;
      },
    );

  const remove = (kind: Kind, value: string) =>
    run("remove_exclusion", { kind, value }, "Removed. Lax captures it again.");

  if (settings === null) {
    return null;
  }
  if (settings === "failed") {
    return <p className="error">Lax couldn't read your exclusions.</p>;
  }

  const appName = (bundleId: string) =>
    apps.find((a) => a.bundleId.toLowerCase() === bundleId.toLowerCase())?.appName;

  return (
    <section className="exclusions">
      <p className="muted">
        Excluded means not read: Lax checks these before it reads anything from a window.
        Changes apply right away.
      </p>

      <h2>Your exclusions</h2>
      <AddApp apps={apps} excluded={settings.apps} busy={busy} onAdd={add} />
      <List
        items={settings.apps}
        label={(id) => (appName(id) ? `${appName(id)} (${id})` : id)}
        empty="No apps excluded."
        busy={busy}
        onRemove={(id) => remove("app", id)}
      />

      <AddText
        kind="site"
        label="Website"
        placeholder="example.com"
        hint="The site and all its subdomains."
        busy={busy}
        onAdd={add}
      />
      <List
        items={settings.sites}
        empty="No websites excluded."
        busy={busy}
        onRemove={(site) => remove("site", site)}
      />

      <AddText
        kind="title"
        label="Window title containing"
        placeholder="Private chat"
        hint="Any window whose title contains this text, in any app."
        busy={busy}
        onAdd={add}
      />
      <List
        items={settings.titles}
        empty="No title patterns."
        busy={busy}
        onRemove={(title) => remove("title", title)}
      />

      <h2>Banking and health</h2>
      <p className="muted">Excluded unless you turn them off. Matched by window title and website.</p>
      <ul className="switches">
        {settings.defaults.map((d) => (
          <li key={d.name}>
            <label>
              <input
                type="checkbox"
                checked={d.excluded}
                disabled={busy}
                onChange={(e) =>
                  run(
                    "set_default_excluded",
                    { name: d.name, excluded: e.target.checked },
                    e.target.checked ? `${d.name} is excluded again.` : `${d.name} is captured now.`,
                  )
                }
              />{" "}
              {d.name}
              <span className="muted"> · {d.category === "banking" ? "banking" : "health"}</span>
            </label>
          </li>
        ))}
      </ul>

      <h2>Always excluded</h2>
      <p className="muted">These can't be turned off, not even here.</p>
      <ul className="always">
        {settings.always.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>

      <p className={message?.error ? "error" : "muted"} role="status" aria-live="polite">
        {message?.text}
      </p>
    </section>
  );
}

function AddApp({
  apps,
  excluded,
  busy,
  onAdd,
}: {
  apps: AppItem[];
  excluded: string[];
  busy: boolean;
  onAdd: (kind: Kind, value: string, forgetApp?: string) => Promise<boolean>;
}) {
  const [choice, setChoice] = useState("");
  const [typed, setTyped] = useState("");
  const [forget, setForget] = useState(false);
  // Forgetting deletes, so it asks once more, like the Forget panel.
  const [confirming, setConfirming] = useState(false);
  const isExcluded = (id: string) => excluded.some((e) => e.toLowerCase() === id.toLowerCase());
  const candidates = apps.filter((a) => !isExcluded(a.bundleId));
  const value = choice || typed.trim();
  const app = apps.find((a) => a.bundleId === value);
  const forgetting = forget && app !== undefined;

  const submit = () =>
    onAdd("app", value, forgetting ? value : undefined).then((ok) => {
      if (ok) {
        setChoice("");
        setTyped("");
        setForget(false);
      }
      setConfirming(false);
    });

  return (
    <div className="add">
      <p className="buttons">
        <label>
          App{" "}
          <select value={choice} onChange={(e) => setChoice(e.target.value)} disabled={busy}>
            <option value="">Choose an app Lax has seen</option>
            {candidates.map((a) => (
              <option key={a.bundleId} value={a.bundleId}>
                {a.appName || a.bundleId}
              </option>
            ))}
          </select>
        </label>
        {!choice && (
          <input
            type="text"
            placeholder="or a bundle id, e.g. com.example.chat"
            value={typed}
            maxLength={255}
            disabled={busy}
            onChange={(e) => setTyped(e.target.value)}
            aria-label="Bundle id"
          />
        )}
        <button
          type="button"
          className="secondary"
          disabled={busy || !value || confirming}
          onClick={forgetting ? () => setConfirming(true) : submit}
        >
          Exclude app
        </button>
      </p>
      {app && (
        <label className="muted">
          <input
            type="checkbox"
            checked={forget}
            onChange={(e) => {
              setForget(e.target.checked);
              setConfirming(false);
            }}
          />{" "}
          Also forget what Lax already captured from it
        </label>
      )}
      {confirming && app && (
        <div className="confirm" role="alertdialog" aria-label="Confirm">
          <p>
            Exclude {app.appName || app.bundleId} and forget its {app.captures}{" "}
            {app.captures === 1 ? "capture" : "captures"}? This can't be undone.
          </p>
          <p className="buttons">
            <button type="button" className="danger" disabled={busy} onClick={submit} autoFocus>
              Exclude and forget
            </button>
            <button type="button" className="secondary" disabled={busy} onClick={() => setConfirming(false)}>
              Cancel
            </button>
          </p>
        </div>
      )}
    </div>
  );
}

function AddText({
  kind,
  label,
  placeholder,
  hint,
  busy,
  onAdd,
}: {
  kind: Kind;
  label: string;
  placeholder: string;
  hint: string;
  busy: boolean;
  onAdd: (kind: Kind, value: string) => Promise<boolean>;
}) {
  const [value, setValue] = useState("");
  const submit = () =>
    onAdd(kind, value).then((ok) => {
      if (ok) {
        setValue("");
      }
    });
  return (
    <div className="add">
      <p className="buttons">
        <label>
          {label}{" "}
          <input
            type="text"
            placeholder={placeholder}
            value={value}
            maxLength={kind === "title" ? 200 : 2000}
            disabled={busy}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && value.trim()) {
                submit();
              }
            }}
          />
        </label>
        <button type="button" className="secondary" disabled={busy || !value.trim()} onClick={submit}>
          Exclude
        </button>
      </p>
      <p className="muted hint">{hint}</p>
    </div>
  );
}

function List({
  items,
  label = (item) => item,
  empty,
  busy,
  onRemove,
}: {
  items: string[];
  label?: (item: string) => string;
  empty: string;
  busy: boolean;
  onRemove: (item: string) => void;
}) {
  if (items.length === 0) {
    return <p className="muted list-empty">{empty}</p>;
  }
  return (
    <ul className="excluded">
      {items.map((item) => (
        <li key={item}>
          <span>{label(item)}</span>
          <button type="button" className="link" disabled={busy} onClick={() => onRemove(item)}>
            Remove
          </button>
        </li>
      ))}
    </ul>
  );
}
