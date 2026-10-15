import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Picked = { name: string; synced: string | null };
type Exported = { pages: number; folder: string };

/** One page (an entity's, or About me for `null`), or all of them. */
export type ExportScope = { entity: number | null } | "all";

/**
 * Exports memory pages as Markdown files (ADR 0011): a notice that the copy
 * isn't encrypted, macOS's folder picker (the folder stays in Lax; this
 * window only learns its name), a second question for a synced folder, and
 * what was written.
 */
export function ExportPages({ scope, onClose }: { scope: ExportScope; onClose: () => void }) {
  const [step, setStep] = useState<"notice" | "picking" | "synced" | "writing" | "done">(
    "notice",
  );
  const [picked, setPicked] = useState<Picked | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const one = scope !== "all";

  const write = (syncedConfirmed: boolean, folder: Picked) => {
    setStep("writing");
    invoke<Exported>("export_pages", {
      entity: scope === "all" ? null : scope.entity,
      all: scope === "all",
      syncedConfirmed,
    }).then(
      (done) => {
        setResult(
          `Exported ${done.pages} ${done.pages === 1 ? "page" : "pages"} to “${done.folder}” in ${folder.name}.`,
        );
        setStep("done");
      },
      (e) => {
        setError(String(e));
        setStep("notice");
      },
    );
  };

  const pick = () => {
    setError(null);
    setStep("picking");
    invoke<Picked | null>("pick_export_folder").then(
      (folder) => {
        if (folder === null) {
          setStep("notice");
        } else if (folder.synced !== null) {
          setPicked(folder);
          setStep("synced");
        } else {
          setPicked(folder);
          write(false, folder);
        }
      },
      (e) => {
        setError(String(e));
        setStep("notice");
      },
    );
  };

  return (
    <section className="card confirm" role="alertdialog" aria-label="Export pages">
      {step === "done" ? (
        <>
          <p role="status">{result}</p>
          <p className="actions">
            <button type="button" className="secondary" onClick={onClose}>
              Done
            </button>
          </p>
        </>
      ) : step === "synced" && picked ? (
        <>
          <h3>“{picked.name}” is in {picked.synced}</h3>
          <p>
            A copy there is synced off this Mac, to {picked.synced}'s servers and every device
            signed in to it. Export there anyway?
          </p>
          <p className="actions">
            <button type="button" className="danger" onClick={() => write(true, picked)}>
              Export there anyway
            </button>{" "}
            <button type="button" className="secondary" onClick={pick}>
              Choose another folder
            </button>{" "}
            <button type="button" className="secondary" onClick={onClose}>
              Cancel
            </button>
          </p>
        </>
      ) : (
        <>
          <h3>{one ? "Export this page" : "Export all pages"}</h3>
          <p>
            {one ? "It will be saved as a plain Markdown file" : "They will be saved as plain Markdown files"},
            not encrypted. Every program you run can read {one ? "it" : "them"}, Spotlight may index{" "}
            {one ? "it" : "them"}, and backups and sync copy {one ? "it" : "them"}. Forgetting
            someone in Lax later doesn't reach this copy.
          </p>
          <p className="actions">
            <button type="button" onClick={pick} disabled={step !== "notice"}>
              {step === "notice" ? "Choose a folder…" : "Exporting…"}
            </button>{" "}
            <button type="button" className="secondary" onClick={onClose}>
              Cancel
            </button>
          </p>
        </>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
