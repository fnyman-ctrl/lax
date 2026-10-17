import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Shapes used by src-tauri/src/model_settings.rs.
type Task = "chat" | "embeddings";
type Choice = { baseUrl: string; model: string };
type ModelSettings = { chat: Choice | null; embeddings: Choice | null };
type LocalServer = { name: string; baseUrl: string; models: string[] };
type TestResult = { ok: boolean; elapsedMs: number; detail: string };

const TASKS: { task: Task; title: string; hint: string; recommended: string }[] = [
  {
    task: "chat",
    title: "Chat model",
    hint: "Writes the journal and keeps the memory pages up to date.",
    recommended: "gemma-4-e4b",
  },
  {
    task: "embeddings",
    title: "Embedding model",
    hint: "Matches names and topics, so the same project is recognised under different words.",
    recommended: "bge-m3",
  },
];

const isEmbeddingModel = (id: string) => /embed/i.test(id);

/**
 * Choosing local models (#15). Only servers on this Mac can be chosen, and
 * looking for them only happens when the user asks (GUARDRAILS §3, §7).
 */
export function Models() {
  const [settings, setSettings] = useState<ModelSettings | null | "failed">(null);
  const [servers, setServers] = useState<LocalServer[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [tests, setTests] = useState<Partial<Record<Task, TestResult | "running" | string>>>({});

  useEffect(() => {
    invoke<ModelSettings>("model_settings").then(setSettings, () => setSettings("failed"));
  }, []);

  const find = () => {
    setSearching(true);
    setMessage(null);
    invoke<LocalServer[]>("find_local_models")
      .then(setServers, () => setServers([]))
      .finally(() => setSearching(false));
  };

  const choose = (task: Task, server: LocalServer, model: string) => {
    setMessage(null);
    setTests((t) => ({ ...t, [task]: undefined }));
    invoke<ModelSettings>("set_model", { task, baseUrl: server.baseUrl, model }).then(
      (next) => {
        setSettings(next);
        setMessage({ text: `${model} is now the ${task === "chat" ? "chat" : "embedding"} model.`, error: false });
      },
      (error) => setMessage({ text: String(error), error: true }),
    );
  };

  const clear = (task: Task) => {
    invoke<ModelSettings>("clear_model", { task }).then(setSettings, (error) =>
      setMessage({ text: String(error), error: true }),
    );
    setTests((t) => ({ ...t, [task]: undefined }));
  };

  const test = (task: Task) => {
    setTests((t) => ({ ...t, [task]: "running" }));
    invoke<TestResult>("test_model", { task }).then(
      (result) => setTests((t) => ({ ...t, [task]: result })),
      (error) => setTests((t) => ({ ...t, [task]: String(error) })),
    );
  };

  if (settings === null) {
    return null;
  }
  if (settings === "failed") {
    return <p className="error">Lax couldn't read its model settings.</p>;
  }

  return (
    <section className="models">
      <p className="muted">
        Lax uses AI models on this Mac to write the journal and learn who you work with. Nothing is
        sent anywhere: only model servers running on this Mac can be chosen. See docs/MODELS.md
        for how we picked the recommendations.
      </p>

      {TASKS.map(({ task, title, hint }) => {
        const choice = settings[task];
        const result = tests[task];
        return (
          <div className="card model-choice" key={task}>
            <h2>{title}</h2>
            <p className="muted hint">{hint}</p>
            {choice ? (
              <>
                <p>
                  <strong>{choice.model}</strong> <span className="muted">at {choice.baseUrl}</span>
                </p>
                <p className="buttons">
                  <button
                    type="button"
                    className="secondary"
                    disabled={result === "running"}
                    onClick={() => test(task)}
                  >
                    {result === "running" ? "Testing…" : "Test"}
                  </button>
                  <button type="button" className="link" onClick={() => clear(task)}>
                    Remove
                  </button>
                </p>
                {result && result !== "running" && (
                  <p className={typeof result === "string" || !result.ok ? "error" : "muted"} role="status">
                    {typeof result === "string"
                      ? result
                      : `${result.ok ? "✓" : "✕"} ${result.detail} (${(result.elapsedMs / 1000).toFixed(1)} s)`}
                  </p>
                )}
              </>
            ) : (
              <p className="muted">None chosen yet.</p>
            )}
          </div>
        );
      })}

      <h2>Models on this Mac</h2>
      <p className="buttons">
        <button type="button" onClick={find} disabled={searching}>
          {searching ? "Looking…" : "Find models on this Mac"}
        </button>
      </p>
      {servers !== null && servers.length === 0 && (
        <div className="card">
          <p>No model server is running on this Mac.</p>
          <ol>
            <li>
              Install <strong>LM Studio</strong> (lmstudio.ai) or <strong>Ollama</strong> (ollama.com).
            </li>
            <li>
              Download a chat model (we recommend Gemma 4 E4B) and an embedding model
              (bge-m3).
            </li>
            <li>Start its local server, then look again.</li>
          </ol>
        </div>
      )}
      {servers?.map((server) => (
        <div className="card server" key={server.baseUrl}>
          <h3>
            {server.name} <span className="muted">· {server.baseUrl}</span>
          </h3>
          {/lm studio/i.test(server.name) && (
            <p className="muted">
              Keep both models loaded: in LM Studio, Developer → Server Settings, turn off
              "Only Keep Last JIT Loaded Model". Otherwise LM Studio reloads the chat model
              after every search, and each answer takes 15–30 s instead of a few.
            </p>
          )}
          {server.models.length === 0 ? (
            <p className="muted">It's running, but has no models. Download one in {server.name}.</p>
          ) : (
            <ul className="model-list">
              {server.models.map((model) => {
                // A guess from the name; both choices stay available,
                // since not every embedding model says "embed".
                const guess: Task = isEmbeddingModel(model) ? "embeddings" : "chat";
                const recommended = TASKS.find((t) => t.task === guess)?.recommended ?? "";
                const chosenFor = (task: Task) =>
                  settings[task]?.model === model && settings[task]?.baseUrl.startsWith(server.baseUrl);
                // Always in the same order, so the choices line up in a column.
                const order: Task[] = ["chat", "embeddings"];
                return (
                  <li key={model} className="model-row">
                    <span className="model-name">
                      {model}{" "}
                      <span className="muted">
                        · probably {guess === "chat" ? "chat" : "embeddings"}
                        {model.toLowerCase().includes(recommended) ? " · recommended" : ""}
                      </span>
                    </span>
                    <span className="model-actions">
                      {order.map((task) =>
                        chosenFor(task) ? (
                          <span key={task} className="muted">
                            ✓ Chosen<span className="visually-hidden"> for {task}</span>
                          </span>
                        ) : (
                          <button
                            key={task}
                            type="button"
                            className="link"
                            onClick={() => choose(task, server, model)}
                          >
                            Use for {task}
                          </button>
                        ),
                      )}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      ))}

      <p className={message?.error ? "error" : "muted"} role="status" aria-live="polite">
        {message?.text}
      </p>

      <BugReportDetails />
    </section>
  );
}

/**
 * "Copy details for a bug report" (#171): versions, the Mac, the models,
 * what's on, and counts. Never anything captured. Shown before it's pasted,
 * so the user sees exactly what they share.
 */
export function BugReportDetails() {
  const [details, setDetails] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const copy = () => {
    setNote(null);
    invoke<string>("bug_report_details").then(
      (text) => {
        setDetails(text);
        navigator.clipboard.writeText(text).then(
          () => setNote("Copied. Paste it into your issue."),
          () => setNote("Lax couldn't copy it. Select the text below and copy it."),
        );
      },
      (e) => setNote(String(e)),
    );
  };
  return (
    <div className="card bug-report">
      <h3>Found a bug?</h3>
      <p className="muted">
        Copy what a bug report needs: versions, your Mac, the models and what's on, and counts.
        Nothing Lax has read, and no names. Nothing is sent.
      </p>
      <p className="buttons">
        <button type="button" className="secondary" onClick={copy}>
          Copy details for a bug report
        </button>
        {note && (
          <span className="muted" role="status">
            {note}
          </span>
        )}
      </p>
      {details && <pre className="details">{details}</pre>}
    </div>
  );
}
