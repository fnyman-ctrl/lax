import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePaged } from "./usePaged";

// Shapes returned by src-tauri/src/journal.rs.
type JournalStatus = {
  enabled: boolean;
  model: string | null;
  waiting: number;
  modelDownSince: number | null;
};
type Episode = {
  id: number;
  startedAt: number;
  endedAt: number;
  summary: string;
  appNames: string[];
  captures: number;
};

const PAGE = 100;
const REFRESH_MS = 30_000;

const startedAt = (episode: Episode) => episode.startedAt;

/**
 * The journal (Stage 3): short episodes the chosen model writes from the
 * captures, as a timeline of each day. Off until the user turns it on.
 * Summaries are model output from captured text, so they are shown as text
 * only.
 */
export function Journal({ onShowModels }: { onShowModels: () => void }) {
  const [status, setStatus] = useState<JournalStatus | null | "failed">(null);
  const [message, setMessage] = useState<string | null>(null);
  const paged = usePaged("journal_episodes", startedAt, PAGE);
  const episodes = paged.items;
  const refreshEpisodes = paged.refresh;

  const refresh = useCallback(() => {
    invoke<JournalStatus>("journal_status").then(setStatus, () => setStatus("failed"));
    refreshEpisodes();
  }, [refreshEpisodes]);

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

  const toggle = (enabled: boolean) => {
    setMessage(null);
    invoke<JournalStatus>("set_journal_enabled", { enabled }).then(setStatus, (e) =>
      setMessage(String(e)),
    );
  };

  if (status === null) {
    return null;
  }
  if (status === "failed") {
    return <p className="error">Lax couldn't read the journal.</p>;
  }

  return (
    <section className="journal">
      {status.enabled && status.modelDownSince !== null && status.waiting > 0 && (
        <div className="warning" role="status">
          <p>
            <strong>Lax can't reach the model server</strong> (since{" "}
            {new Date(status.modelDownSince).toLocaleTimeString(undefined, {
              hour: "2-digit",
              minute: "2-digit",
            })}
            ), so new entries wait. Start it again: in LM Studio, the Developer tab, or{" "}
            <code>~/.lmstudio/bin/lms server start</code>; in Ollama, open the app. Lax tries
            again every ten minutes.
          </p>
        </div>
      )}
      <div className="card journal-status">
        {status.enabled ? (
          <>
            <p>
              <strong>The journal is on.</strong> Every few minutes, {status.model ?? "your model"}{" "}
              on this Mac writes short entries about what you did.{" "}
              {status.waiting > 0 && (
                <span className="muted">
                  {status.waiting} {status.waiting === 1 ? "capture waits" : "captures wait"} to be
                  summarized.
                </span>
              )}
            </p>
            <p className="buttons">
              <button type="button" className="secondary" onClick={() => toggle(false)}>
                Turn the journal off
              </button>
            </p>
          </>
        ) : (
          <>
            <p>
              <strong>Let Lax write a journal of your day.</strong> Every few minutes, an AI model on
              this Mac reads what Lax captured and writes one to three sentences about it: what you
              worked on, in which apps, with whom. Nothing leaves this Mac, and it stops while
              capture is paused.
            </p>
            {status.model ? (
              <p className="buttons">
                <button type="button" onClick={() => toggle(true)}>
                  Turn the journal on
                </button>
                <span className="muted">Uses {status.model}.</span>
              </p>
            ) : (
              <p className="buttons">
                <button type="button" onClick={onShowModels}>
                  Choose a model first
                </button>
                <span className="muted">The journal needs a chat model on this Mac.</span>
              </p>
            )}
          </>
        )}
        {message && <p className="error">{message}</p>}
      </div>

      {paged.failed && <p className="error">Lax couldn't read the newest entries.</p>}
      {episodes !== null && episodes.length === 0 && status.enabled && (
        <p className="muted empty">
          No entries yet. The first one appears about 15 minutes after capture starts.
        </p>
      )}
      {episodes !== null &&
        groupByDay(episodes).map((day) => (
          <section key={day.key} className="day">
            <h2>{day.label}</h2>
            <ul className="episodes">
              {day.episodes.map((episode) => (
                <li key={episode.id} className="episode">
                  <p className="when">
                    <span className="time">
                      {time(episode.startedAt)}–{time(episode.endedAt)}
                    </span>
                    <span className="muted"> · {episode.appNames.join(", ")}</span>
                  </p>
                  <p className="summary">{episode.summary}</p>
                </li>
              ))}
            </ul>
          </section>
        ))}
      {paged.olderFailed && <p className="error">Lax couldn't read older entries.</p>}
      {paged.more && (
        <p className="actions">
          <button
            type="button"
            className="secondary"
            onClick={paged.loadOlder}
            disabled={paged.loadingMore}
          >
            {paged.loadingMore ? "Loading…" : "Show older"}
          </button>
        </p>
      )}
    </section>
  );
}

type Day = { key: string; label: string; episodes: Episode[] };

function groupByDay(episodes: Episode[]): Day[] {
  const days: Day[] = [];
  for (const episode of episodes) {
    const date = new Date(episode.startedAt);
    const key = date.toDateString();
    let day = days[days.length - 1];
    if (!day || day.key !== key) {
      day = { key, label: dayLabel(date), episodes: [] };
      days.push(day);
    }
    day.episodes.push(episode);
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
  const label = date.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
  return label.charAt(0).toUpperCase() + label.slice(1);
}

function time(millis: number): string {
  return new Date(millis).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}
