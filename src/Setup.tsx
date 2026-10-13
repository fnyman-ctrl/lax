import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Models } from "./Models";
// The pixel salmon (docs/BRAND.md).
import salmon from "../src-tauri/icons/salmon.svg";

/** What the user turns on in the guide, and the commands behind each. */
const FEATURES: {
  key: "journal" | "memory" | "writing";
  title: string;
  about: string;
  status: string;
  set: string;
}[] = [
  {
    key: "journal",
    title: "Journal",
    about: "Every few minutes, the model writes a few lines about what you did.",
    status: "journal_status",
    set: "set_journal_enabled",
  },
  {
    key: "memory",
    title: "Memory",
    about:
      "Lax learns the people, projects and organisations in your journal, and keeps a page about each in ~/Lax.",
    status: "memory_status",
    set: "set_memory_enabled",
  },
  {
    key: "writing",
    title: "Writing assistant",
    about: "Double-tap ⌥ Option in any text field, and Lax drafts the text with you.",
    status: "writing_status",
    set: "set_writing_enabled",
  },
];

/** Model advice for a Mac with this much memory (docs/MODELS.md). */
function advice(memoryGb: number): string {
  if (memoryGb === 0) {
    return "Gemma 4 E4B for writing and nomic-embed-text for matching work well on most Macs.";
  }
  if (memoryGb < 16) {
    return `This Mac has ${memoryGb} GB of memory. A small chat model, 2–4 billion parameters (such as Gemma 4 E2B), leaves room for your other apps. Add nomic-embed-text for matching.`;
  }
  if (memoryGb < 32) {
    return `This Mac has ${memoryGb} GB of memory. Gemma 4 E4B (about 7 GB) for writing and nomic-embed-text for matching are tested on 16 GB and quick.`;
  }
  return `This Mac has ${memoryGb} GB of memory. Gemma 4 E4B is quick; a larger model such as Qwen 3.5 9B writes better journal entries if you don't mind the wait.`;
}

/**
 * The first start, after the Accessibility permission: a model on this Mac,
 * then what to turn on. Everything stays off until the user turns it on.
 */
export function Setup({ memoryGb, onDone }: { memoryGb: number; onDone: () => void }) {
  const [step, setStep] = useState<"model" | "features">("model");
  const [on, setOn] = useState<Record<string, boolean>>({});
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (step !== "features") {
      return;
    }
    for (const feature of FEATURES) {
      invoke<{ enabled: boolean }>(feature.status).then(
        (status) => setOn((o) => ({ ...o, [feature.key]: status.enabled })),
        () => {},
      );
    }
  }, [step]);

  const toggle = (feature: (typeof FEATURES)[number], enabled: boolean) => {
    setMessage(null);
    invoke<{ enabled: boolean }>(feature.set, { enabled }).then(
      (status) => setOn((o) => ({ ...o, [feature.key]: status.enabled })),
      (e) => setMessage(String(e)),
    );
  };

  const finish = () => {
    invoke("finish_setup").then(onDone, (e) => setMessage(String(e)));
  };

  return (
    <main className="container setup">
      <header className="setup-head">
        <img src={salmon} alt="" />
        <div>
          <h1>{step === "model" ? "A model on this Mac" : "What should Lax do?"}</h1>
          <p className="muted">
            {step === "model"
              ? "Lax's AI runs on your Mac, so nothing you do leaves it. Pick the models it uses."
              : "Everything is off until you turn it on. You can change it later in each section."}
          </p>
        </div>
      </header>

      {step === "model" ? (
        <>
          <section className="card setup-advice">
            <p>{advice(memoryGb)}</p>
            <details>
              <summary>No model server yet?</summary>
              <ol>
                <li>
                  Install <strong>LM Studio</strong> (lmstudio.ai) or <strong>Ollama</strong>{" "}
                  (ollama.com).
                </li>
                <li>Download the models above in it.</li>
                <li>
                  Start its local server: in LM Studio, the Developer tab, or{" "}
                  <code>~/.lmstudio/bin/lms server start</code>.
                </li>
                <li>
                  In LM Studio, turn off "Only Keep Last JIT Loaded Model" (Developer → Server
                  Settings), so the two models stay loaded together.
                </li>
                <li>Come back and press "Find models on this Mac" below.</li>
              </ol>
            </details>
          </section>
          <Models />
          <p className="buttons setup-nav">
            <button type="button" onClick={() => setStep("features")}>
              Next
            </button>
            <button type="button" className="secondary" onClick={() => setStep("features")}>
              Skip for now
            </button>
          </p>
        </>
      ) : (
        <>
          <ul className="setup-features">
            {FEATURES.map((feature) => (
              <li key={feature.key} className="card">
                <label>
                  <input
                    type="checkbox"
                    checked={on[feature.key] ?? false}
                    onChange={(e) => toggle(feature, e.target.checked)}
                  />
                  <span>
                    <strong>{feature.title}</strong>
                    <span className="muted"> {feature.about}</span>
                  </span>
                </label>
              </li>
            ))}
          </ul>
          {message && (
            <p className="error" role="alert">
              {message}
            </p>
          )}
          <p className="buttons setup-nav">
            <button type="button" onClick={finish}>
              Start using Lax
            </button>
            <button type="button" className="secondary" onClick={() => setStep("model")}>
              Back
            </button>
          </p>
        </>
      )}
    </main>
  );
}
