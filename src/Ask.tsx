import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
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
  "What did I do yesterday?",
  "What have I worked on this week?",
  "What did I work on last week?",
];

/** The earlier questions that go along with a follow-up; must match
 * MAX_EARLIER_TURNS in src-tauri/src/ask.rs. */
const REMEMBERED_TURNS = 4;

/**
 * Ask your memory: a conversation with the model on this Mac about your
 * journal and memory. Follow-up questions go with the turns before them.
 * The conversation lives only in this window; nothing is saved. Answers come
 * from captured text through a model, so they are shown as text only.
 *
 * While the model writes, the answer's text so far is shown as it arrives,
 * before it is validated (GUARDRAILS §5). It is only ever plain text, and it
 * is replaced by the validated answer, or removed and replaced by the error
 * if the answer fails. Only validated answers go along with a follow-up.
 */
export function Ask({ onShowJournal }: { onShowJournal: () => void }) {
  const [question, setQuestion] = useState("");
  const [asking, setAsking] = useState<string | null>(null);
  // The answer to `asking` as the model writes it, not yet validated.
  const [streamed, setStreamed] = useState("");
  const [error, setError] = useState<string | null>(null);
  // Oldest first.
  const [turns, setTurns] = useState<Turn[]>([]);
  const input = useRef<HTMLTextAreaElement>(null);
  // The user is at the end of the conversation, not reading further up.
  const following = useRef(true);

  // The window scrolls, not the conversation. Its end is below the question
  // box and the note under it: scrolling only to the conversation's last
  // message would move the window up when the user is already at the end.
  const toEnd = (behavior: ScrollBehavior = "auto") => {
    const page = document.scrollingElement ?? document.documentElement;
    window.scrollTo({ top: page.scrollHeight, behavior });
  };
  // A new question, or an error: show it.
  useEffect(() => {
    toEnd("smooth");
  }, [asking, error]);
  // The checked answer, and each piece while it is written: follow it,
  // unless the user has scrolled up to read something else.
  // Before the window is drawn, so a new line never shows under the
  // question box first.
  useLayoutEffect(() => {
    if (following.current) {
      toEnd(streamed === "" ? "smooth" : "auto");
    }
  }, [turns, streamed]);
  // Scrolling up, however little, leaves the end; scrolling back down to it
  // follows again. The wheel and keys are read as they are used: a piece of
  // the answer can arrive before the scroll they caused is reported, and
  // would scroll the window back down. The window moving up because the
  // page got shorter, such as when the thinking salmon makes way for the
  // first line, leaves it at the end and isn't the user leaving.
  useEffect(() => {
    const page = document.scrollingElement ?? document.documentElement;
    const gap = () => page.scrollHeight - page.scrollTop - page.clientHeight;
    let last = page.scrollTop;
    const leave = () => {
      if (page.scrollTop > 0) {
        following.current = false;
      }
    };
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0) {
        leave();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLElement && e.target.closest("textarea, input");
      if (!typing && ["ArrowUp", "PageUp", "Home"].includes(e.key)) {
        leave();
      }
    };
    const onScroll = () => {
      const top = page.scrollTop;
      if (top < last && gap() > 1) {
        following.current = false;
      } else if (top >= last && gap() < 48) {
        following.current = true;
      }
      last = top;
    };
    window.addEventListener("wheel", onWheel, { passive: true });
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      window.removeEventListener("wheel", onWheel);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll);
    };
  }, []);

  const ask = (text: string) => {
    const q = text.trim();
    if (!q || asking) {
      return;
    }
    setAsking(q);
    following.current = true;
    setStreamed("");
    setError(null);
    setQuestion("");
    const earlier = turns.map((turn) => ({
      question: turn.question,
      answer: turn.answer.answer,
    }));
    // Pieces of this question's answer. Once it is answered or has failed,
    // pieces that arrive late are dropped.
    let open = true;
    const onText = new Channel<string>();
    onText.onmessage = (piece) => {
      if (open && typeof piece === "string") {
        setStreamed((text) => text + piece);
      }
    };
    invoke<AskAnswer>("ask_memory", { question: q, earlier, onText })
      .then(
        (answer) => setTurns((list) => [...list, { question: q, answer }]),
        (e) => {
          setError(String(e));
          setQuestion(q);
        },
      )
      .finally(() => {
        open = false;
        setStreamed("");
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
      <div
        className="chat-log"
        role="log"
        aria-live="polite"
      >
        {turns.length === 0 && asking === null && (
          <div className="chat-empty">
            <img src={salmon} alt="" className="chat-salmon" />
            <p>
              Start with one of these, then ask on: "and the day before?", "what did she say?"
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
              <AnswerText text={turn.answer.answer} />
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
            {streamed === "" ? (
              <div className="bubble from-lax thinking" aria-label="Lax is thinking">
                <img src={salmon} alt="" className="chat-salmon working" />
                <span className="dots">
                  <span />
                  <span />
                  <span />
                </span>
              </div>
            ) : (
              // Plain text only (AnswerText). Screen readers hear the
              // validated answer once, not each piece as it comes.
              <div className="bubble from-lax writing" aria-live="off" aria-busy="true">
                <AnswerText text={streamed} />
              </div>
            )}
          </>
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </div>

      <form
        className="ask-form"
        onSubmit={(e) => {
          e.preventDefault();
          ask(question);
        }}
      >
        {turns.length > 0 && (
          <div className="conversation">
            {turns.length >= REMEMBERED_TURNS && (
              <p className="muted">
                Lax keeps at most the last {REMEMBERED_TURNS} questions in mind. For a new subject,
                start a new conversation.
              </p>
            )}
            <button
              type="button"
              className="secondary"
              onClick={startOver}
              disabled={asking !== null}
            >
              New conversation
            </button>
          </div>
        )}
        <div className="ask-row">
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
        </div>
      </form>
      <p className="muted hint">
        Lax answers from the journal and memory, with the model on this Mac. Nothing leaves this
        Mac, and nothing you ask is saved.
      </p>
    </section>
  );
}

type Block = { kind: "text"; text: string } | { kind: "points"; points: string[] };

// Also a lone "-", the start of a point still being written.
const POINT = /^\s*[-•*](?:\s+(.*)|\s*)$/;
// A list the model wrote on one line: "It covered: - this. - that."
const INLINE_POINT = /([.:!?])\s+-\s+(?=\S)/g;

/** The answer as paragraphs and lists of points, for reading: lines
 * starting with "- " become a list, and a blank line ends a paragraph. */
export function answerBlocks(text: string): Block[] {
  const lines = text.split("\n").flatMap((line) => {
    const split = line.replace(INLINE_POINT, "$1\n- ");
    return split.split("\n").length > 2 ? split.split("\n") : [line];
  });
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let points: string[] = [];
  const end = () => {
    if (paragraph.length > 0) {
      blocks.push({ kind: "text", text: paragraph.join("\n") });
    }
    if (points.length > 0) {
      blocks.push({ kind: "points", points });
    }
    paragraph = [];
    points = [];
  };
  for (const line of lines) {
    const point = POINT.exec(line);
    if (point) {
      if (paragraph.length > 0) {
        end();
      }
      points.push(point[1] ?? "");
    } else if (line.trim() === "") {
      end();
    } else {
      if (points.length > 0) {
        end();
      }
      paragraph.push(line);
    }
  }
  end();
  return blocks;
}

/** An answer as plain text in paragraphs and lists: React puts it in as
 * text, never as HTML, and nothing else of Markdown is read. */
function AnswerText({ text }: { text: string }) {
  return (
    <div className="answer">
      {answerBlocks(text).map((block, i) =>
        block.kind === "points" ? (
          <ul key={i}>
            {block.points.map((point, j) => (
              <li key={j}>{point}</li>
            ))}
          </ul>
        ) : (
          <p key={i}>{block.text}</p>
        ),
      )}
    </div>
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
