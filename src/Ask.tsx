import { Fragment, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

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
 * Ask your memory: a conversation with the model on this Mac about your
 * journal and memory. Follow-up questions go with the turns before them.
 * The conversation lives only in this window; nothing is saved. Answers come
 * from captured text through a model, so they are shown as text only.
 */
export function Ask({ onShowJournal }: { onShowJournal: () => void }) {
  const [question, setQuestion] = useState("");
  const [asking, setAsking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Oldest first.
  const [turns, setTurns] = useState<Turn[]>([]);
  const end = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);

  // Keep the latest message in view.
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end", behavior: "smooth" });
  }, [turns, asking, error]);

  const ask = (text: string) => {
    const q = text.trim();
    if (!q || asking) {
      return;
    }
    setAsking(q);
    setError(null);
    setQuestion("");
    const earlier = turns.map((turn) => ({
      question: turn.question,
      answer: turn.answer.answer,
    }));
    invoke<AskAnswer>("ask_memory", { question: q, earlier })
      .then(
        (answer) => setTurns((list) => [...list, { question: q, answer }]),
        (e) => {
          setError(String(e));
          setQuestion(q);
        },
      )
      .finally(() => {
        setAsking(null);
        input.current?.focus();
      });
  };

  const startOver = () => {
    setTurns([]);
    setError(null);
    input.current?.focus();
  };

  return (
    <section className="ask chat">
      <div className="chat-log" role="log" aria-live="polite">
        {turns.length === 0 && asking === null && (
          <div className="chat-empty">
            <img src={salmon} alt="" className="chat-salmon" />
            <p>
              <strong>Ask Lax about your days, people and projects.</strong> Then ask on: "and
              the day before?", "what did she say?"
            </p>
            <p className="buttons examples">
              {EXAMPLES.map((example) => (
                <button
                  key={example}
                  type="button"
                  className="secondary"
                  onClick={() => ask(example)}
                >
                  {example}
                </button>
              ))}
            </p>
          </div>
        )}

        {turns.map((turn, i) => (
          <Fragment key={i}>
            <p className="bubble from-you">{turn.question}</p>
            <div className="bubble from-lax">
              <p className="answer">{turn.answer.answer}</p>
              {turn.answer.recentOnly && turn.answer.considered > 0 && (
                <p className="muted hint">
                  The question didn't name a time, or a person, project or word Lax could
                  find, so it answered from the latest {turn.answer.considered} entries.
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
          </Fragment>
        ))}

        {asking !== null && (
          <>
            <p className="bubble from-you">{asking}</p>
            <div className="bubble from-lax thinking" aria-label="Lax is thinking">
              <img src={salmon} alt="" className="chat-salmon working" />
              <span className="dots">
                <span />
                <span />
                <span />
              </span>
            </div>
          </>
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div ref={end} />
      </div>

      <form
        className="ask-form"
        onSubmit={(e) => {
          e.preventDefault();
          ask(question);
        }}
      >
        <textarea
          ref={input}
          value={question}
          rows={1}
          maxLength={1000}
          placeholder={turns.length > 0 ? "Ask on…" : "Ask about your days, people and projects"}
          aria-label="Question"
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              ask(question);
            }
          }}
        />
        <button type="submit" disabled={asking !== null || question.trim() === ""}>
          Ask
        </button>
      </form>
      <p className="muted hint">
        Lax answers from the journal and memory, with the model on this Mac. Nothing leaves this
        Mac, and nothing you ask is saved.
        {turns.length > 0 && (
          <>
            {" "}
            <button type="button" className="link" onClick={startOver} disabled={asking !== null}>
              New conversation
            </button>
          </>
        )}
      </p>
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
