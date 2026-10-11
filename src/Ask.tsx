import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shapes returned by src-tauri/src/ask.rs.
type Source = {
  id: number;
  startedAt: number;
  endedAt: number;
  summary: string;
  appNames: string[];
};
type AskAnswer = {
  answer: string;
  sources: Source[];
  considered: number;
  recentOnly: boolean;
};
type Turn = { question: string; answer: AskAnswer };

const EXAMPLES = [
  "Vad gjorde jag igår?",
  "Vad har jag jobbat med i veckan?",
  "What did I work on last week?",
];

/**
 * Ask your memory (Stage 5, first version): questions answered from the
 * journal by the model on this Mac. Questions and answers live only in this
 * window; nothing is saved. Answers come from captured text through a model,
 * so they are shown as text only.
 */
export function Ask({ onShowJournal }: { onShowJournal: () => void }) {
  const [question, setQuestion] = useState("");
  const [asking, setAsking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);

  const ask = (text: string) => {
    const q = text.trim();
    if (!q || asking) {
      return;
    }
    setAsking(q);
    setError(null);
    invoke<AskAnswer>("ask_memory", { question: q })
      .then(
        (answer) => {
          setTurns((list) => [{ question: q, answer }, ...list]);
          setQuestion("");
        },
        (e) => setError(String(e)),
      )
      .finally(() => setAsking(null));
  };

  return (
    <section className="ask">
      <form
        className="ask-form"
        onSubmit={(e) => {
          e.preventDefault();
          ask(question);
        }}
      >
        <input
          type="text"
          value={question}
          maxLength={1000}
          placeholder="Ask about your days, people and projects"
          aria-label="Question"
          disabled={asking !== null}
          onChange={(e) => setQuestion(e.target.value)}
        />
        <button type="submit" disabled={asking !== null || question.trim() === ""}>
          Ask
        </button>
      </form>
      <p className="muted hint">
        Lax answers from the journal, with the model on this Mac. Nothing leaves this Mac, and
        nothing you ask is saved.
      </p>

      {turns.length === 0 && asking === null && (
        <p className="buttons examples">
          {EXAMPLES.map((example) => (
            <button
              key={example}
              type="button"
              className="secondary"
              onClick={() => {
                setQuestion(example);
                ask(example);
              }}
            >
              {example}
            </button>
          ))}
        </p>
      )}

      <div role="status" aria-live="polite">
        {asking !== null && (
          <div className="turn">
            <p className="question">{asking}</p>
            <p className="muted">Thinking… With a model on this Mac this can take a minute.</p>
          </div>
        )}
        {error && <p className="error">{error}</p>}
      </div>

      {turns.map((turn, i) => (
        <div key={turns.length - i} className="turn">
          <p className="question">{turn.question}</p>
          <p className="answer">{turn.answer.answer}</p>
          {turn.answer.recentOnly && turn.answer.considered > 0 && (
            <p className="muted hint">
              The question didn't name a time, or a person, project or word Lax could find, so
              it answered from the latest {turn.answer.considered} entries.
            </p>
          )}
          {turn.answer.considered === 0 && (
            <p className="buttons">
              <button type="button" className="secondary" onClick={onShowJournal}>
                Open the journal
              </button>
            </p>
          )}
          {turn.answer.sources.length > 0 && (
            <details className="sources">
              <summary>
                Based on {turn.answer.sources.length}{" "}
                {turn.answer.sources.length === 1 ? "entry" : "entries"}
              </summary>
              <ul className="episodes">
                {turn.answer.sources.map((source) => (
                  <li key={source.id} className="episode">
                    <p className="when">
                      <span className="time">{when(source)}</span>
                      <span className="muted"> · {source.appNames.join(", ")}</span>
                    </p>
                    <p className="summary">{source.summary}</p>
                  </li>
                ))}
              </ul>
            </details>
          )}
        </div>
      ))}
    </section>
  );
}

function when(source: Source): string {
  const start = new Date(source.startedAt);
  const day = start.toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
  });
  const time = (d: Date) => d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return `${day} ${time(start)}–${time(new Date(source.endedAt))}`;
}
