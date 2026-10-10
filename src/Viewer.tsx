import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Exclusions } from "./Exclusions";
import { Forget } from "./Forget";

// Shapes returned by src-tauri/src/viewer.rs.
type CaptureStatus = {
  paused: boolean;
  label: string;
  permission: boolean;
  capturing: boolean;
  storeError: string | null;
};

type CaptureItem = {
  id: number;
  capturedAt: number;
  appName: string;
  bundleId: string;
  windowTitle: string;
  site: string | null;
  preview: string;
  textChars: number;
};

type Hit = { capture: CaptureItem; snippet: string };

type CaptureDetail = {
  id: number;
  capturedAt: number;
  appName: string;
  windowTitle: string;
  site: string | null;
  text: string;
};

const PAGE = 100;
const SEARCH_LIMIT = 50;
/** How often the newest captures and the status are fetched while the window is shown. */
const REFRESH_MS = 15_000;
const SEARCH_DELAY_MS = 200;

// Must match MATCH_START and MATCH_END in src-tauri/src/store.rs.
const MATCH_START = "\u0002";
const MATCH_END = "\u0003";

/**
 * Everything Lax has captured, newest first, with search (GUARDRAILS §1: you
 * can see everything that was captured). Captured text is untrusted: it is
 * only ever rendered as text, never as HTML.
 */
export function Viewer() {
  const [tab, setTab] = useState<"captures" | "exclusions">("captures");
  const [query, setQuery] = useState("");
  // Bumped after a deletion, so the lists start over from the database.
  const [version, setVersion] = useState(0);
  const searching = query.trim() !== "";
  const forgotten = () => setVersion((v) => v + 1);

  return (
    <main className="container viewer">
      <header className="viewer-header">
        <h1>Lax</h1>
        <Status />
      </header>
      <nav className="tabs" aria-label="Sections">
        <button
          type="button"
          aria-current={tab === "captures" ? "page" : undefined}
          onClick={() => setTab("captures")}
        >
          Captures
        </button>
        <button
          type="button"
          aria-current={tab === "exclusions" ? "page" : undefined}
          onClick={() => setTab("exclusions")}
        >
          Exclusions
        </button>
      </nav>
      {tab === "exclusions" ? (
        <Exclusions onForgotten={forgotten} />
      ) : (
        <>
          <label className="search">
            <span className="visually-hidden">Search what Lax has captured</span>
            <input
              type="search"
              placeholder="Search what Lax has captured"
              value={query}
              maxLength={200}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
          <Forget onForgotten={forgotten} />
          {searching ? (
            <SearchResults key={version} query={query} />
          ) : (
            <Timeline key={version} />
          )}
        </>
      )}
    </main>
  );
}

/** Calls `refresh` now, when the window gets focus, and every REFRESH_MS while it is visible. */
function useRefresh(refresh: () => void) {
  useEffect(() => {
    refresh();
    const onFocus = () => refresh();
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") {
        refresh();
      }
    }, REFRESH_MS);
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh]);
}

function Status() {
  const [status, setStatus] = useState<CaptureStatus | null | "failed">(null);
  const [settingsFailed, setSettingsFailed] = useState(false);

  const refresh = useCallback(() => {
    invoke<CaptureStatus>("capture_status").then(setStatus, () => setStatus("failed"));
  }, []);
  useRefresh(refresh);

  if (status === null) {
    return null;
  }
  if (status === "failed") {
    return <p className="status error">Lax couldn't read its own status.</p>;
  }
  if (status.storeError !== null) {
    return (
      <div className="status error" role="alert">
        Lax can't capture anything: {status.storeError}.
      </div>
    );
  }
  if (!status.permission) {
    return (
      <div className="status error">
        Accessibility access is off, so nothing is captured.{" "}
        <button
          type="button"
          className="link"
          onClick={() => {
            setSettingsFailed(false);
            invoke("open_accessibility_settings").catch(() => setSettingsFailed(true));
          }}
        >
          Turn it on in System Settings
        </button>
        {settingsFailed && " (System Settings didn't open: go to Privacy & Security → Accessibility.)"}
      </div>
    );
  }
  if (!status.capturing) {
    return (
      <div className="status error" role="alert">
        Capture isn't running. Quit Lax from the menu bar and open it again.
      </div>
    );
  }
  return (
    <p className={status.paused ? "status paused" : "status running"}>
      <span className="dot" aria-hidden="true" />
      {status.label}
      {status.paused ? "" : " · pause from the menu bar"}
    </p>
  );
}

function Timeline() {
  const [items, setItems] = useState<CaptureItem[] | null>(null);
  const [error, setError] = useState(false);
  const [more, setMore] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const itemsRef = useRef<CaptureItem[]>([]);

  const show = (next: CaptureItem[]) => {
    itemsRef.current = next;
    setItems(next);
  };

  // Fetches the newest page and replaces what it covers, so new captures
  // appear and deleted ones disappear. Older pages already loaded are kept.
  const refresh = useCallback(() => {
    invoke<CaptureItem[]>("recent_captures", { limit: PAGE }).then(
      (newest) => {
        setError(false);
        if (newest.length < PAGE) {
          // That is everything there is.
          setMore(false);
          show(newest);
          return;
        }
        const last = newest[newest.length - 1];
        const older = itemsRef.current.filter(
          (c) => c.capturedAt < last.capturedAt || (c.capturedAt === last.capturedAt && c.id < last.id),
        );
        if (older.length === 0) {
          setMore(true);
        }
        show([...newest, ...older]);
      },
      () => setError(true),
    );
  }, []);
  useRefresh(refresh);

  const loadOlder = () => {
    const last = itemsRef.current[itemsRef.current.length - 1];
    if (!last) {
      return;
    }
    setLoadingMore(true);
    invoke<CaptureItem[]>("recent_captures", {
      beforeAt: last.capturedAt,
      beforeId: last.id,
      limit: PAGE,
    })
      .then(
        (older) => {
          setMore(older.length === PAGE);
          show([...itemsRef.current, ...older]);
        },
        () => setError(true),
      )
      .finally(() => setLoadingMore(false));
  };

  if (error && items === null) {
    return <p className="error">Lax couldn't read what it has captured.</p>;
  }
  if (items === null) {
    return null;
  }
  if (items.length === 0) {
    return (
      <p className="muted empty">
        Nothing captured yet. While capture is on, Lax reads the active window every few
        seconds, and what it reads shows up here.
      </p>
    );
  }
  return (
    <>
      {error && <p className="error">Lax couldn't read the newest captures.</p>}
      {groupByDayAndApp(items).map((day) => (
        <section key={day.key} className="day">
          <h2>{day.label}</h2>
          {day.runs.map((run) => (
            <AppRun key={run.items[0].id} items={run.items} />
          ))}
        </section>
      ))}
      {more && (
        <p className="actions">
          <button type="button" className="secondary" onClick={loadOlder} disabled={loadingMore}>
            {loadingMore ? "Loading…" : "Show older"}
          </button>
        </p>
      )}
    </>
  );
}

function AppRun({ items }: { items: CaptureItem[] }) {
  const newest = items[0];
  const oldest = items[items.length - 1];
  const span =
    items.length === 1
      ? time(newest.capturedAt)
      : `${time(oldest.capturedAt)}–${time(newest.capturedAt)}`;
  return (
    <div className="run">
      <h3>
        {newest.appName || newest.bundleId}
        <span className="muted"> · {span}</span>
      </h3>
      <ul>
        {items.map((item) => (
          <CaptureRow key={item.id} item={item} />
        ))}
      </ul>
    </div>
  );
}

function CaptureRow({
  item,
  snippet,
  context,
}: {
  item: CaptureItem;
  snippet?: string;
  /** A line above the row, for search results. */
  context?: string;
}) {
  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<CaptureDetail | null | "missing" | "failed">(null);

  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && (detail === null || detail === "failed")) {
      invoke<CaptureDetail | null>("capture_detail", { id: item.id }).then(
        (found) => setDetail(found ?? "missing"),
        () => setDetail("failed"),
      );
    }
  };

  return (
    <li className="capture">
      {context && <p className="hit-app muted">{context}</p>}
      <button type="button" className="row" aria-expanded={open} onClick={toggle}>
        <span className="time">{time(item.capturedAt)}</span>
        <span className="title">{item.windowTitle || "Untitled window"}</span>
        {item.site && <span className="site">{item.site}</span>}
      </button>
      {!open && (
        <p className="preview">
          {snippet !== undefined ? <Snippet text={snippet} /> : item.preview}
        </p>
      )}
      {open && <Detail detail={detail} />}
    </li>
  );
}

function Detail({ detail }: { detail: CaptureDetail | null | "missing" | "failed" }) {
  if (detail === null) {
    return <p className="muted">Loading…</p>;
  }
  if (detail === "missing") {
    return <p className="muted">This capture is gone. It may have expired or been forgotten.</p>;
  }
  if (detail === "failed") {
    return <p className="error">Lax couldn't read this capture.</p>;
  }
  return (
    <div className="detail">
      <p className="muted">
        {new Date(detail.capturedAt).toLocaleString()} · {detail.appName}
        {detail.site ? ` · ${detail.site}` : ""}
      </p>
      {detail.text ? (
        <pre className="text">{detail.text}</pre>
      ) : (
        <p className="muted">No text, only the window title.</p>
      )}
    </div>
  );
}

function SearchResults({ query }: { query: string }) {
  const [hits, setHits] = useState<Hit[] | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    let current = true;
    const timer = window.setTimeout(() => {
      invoke<Hit[]>("search_captures", { query, limit: SEARCH_LIMIT }).then(
        (found) => {
          if (current) {
            setError(false);
            setHits(found);
          }
        },
        () => current && setError(true),
      );
    }, SEARCH_DELAY_MS);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [query]);

  if (error) {
    return <p className="error">Lax couldn't search what it has captured.</p>;
  }
  if (hits === null) {
    return null;
  }
  if (hits.length === 0) {
    return <p className="muted empty">Nothing matches “{query.trim()}”.</p>;
  }
  return (
    <section className="day">
      <h2>
        {hits.length === SEARCH_LIMIT ? `The ${SEARCH_LIMIT} best matches` : plural(hits.length, "match", "matches")}
      </h2>
      <ul>
        {hits.map((hit) => (
          <CaptureRow
            key={hit.capture.id}
            item={hit.capture}
            snippet={hit.snippet}
            context={`${hit.capture.appName} · ${new Date(hit.capture.capturedAt).toLocaleDateString()}`}
          />
        ))}
      </ul>
    </section>
  );
}

/** Renders a snippet with its matched words marked, as text only. */
export function Snippet({ text }: { text: string }) {
  return (
    <>
      {splitMatches(text).map((part, i) =>
        part.match ? <mark key={i}>{part.text}</mark> : <Fragment key={i}>{part.text}</Fragment>,
      )}
    </>
  );
}

export function splitMatches(text: string): { text: string; match: boolean }[] {
  const parts: { text: string; match: boolean }[] = [];
  let rest = text;
  while (rest.length > 0) {
    const start = rest.indexOf(MATCH_START);
    if (start === -1) {
      parts.push({ text: rest, match: false });
      break;
    }
    if (start > 0) {
      parts.push({ text: rest.slice(0, start), match: false });
    }
    const end = rest.indexOf(MATCH_END, start + 1);
    const stop = end === -1 ? rest.length : end;
    parts.push({ text: rest.slice(start + 1, stop), match: true });
    rest = end === -1 ? "" : rest.slice(end + 1);
  }
  // A stray end marker is dropped rather than shown.
  return parts
    .map((p) => ({ ...p, text: p.text.split(MATCH_END).join("") }))
    .filter((p) => p.text !== "");
}

type Day = { key: string; label: string; runs: { items: CaptureItem[] }[] };

/** Groups captures (newest first) by local day, then into runs of the same app. */
function groupByDayAndApp(items: CaptureItem[]): Day[] {
  const days: Day[] = [];
  for (const item of items) {
    const date = new Date(item.capturedAt);
    const key = date.toDateString();
    let day = days[days.length - 1];
    if (!day || day.key !== key) {
      day = { key, label: dayLabel(date), runs: [] };
      days.push(day);
    }
    const run = day.runs[day.runs.length - 1];
    if (run && run.items[0].bundleId === item.bundleId) {
      run.items.push(item);
    } else {
      day.runs.push({ items: [item] });
    }
  }
  return days;
}

function dayLabel(date: Date): string {
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) {
    return "Today";
  }
  if (date.toDateString() === yesterday.toDateString()) {
    return "Yesterday";
  }
  return date.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
}

function time(millis: number): string {
  return new Date(millis).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}
