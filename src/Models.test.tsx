import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { BugReportDetails, Models } from "./Models";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

const servers = (name: string) => [
  { name, baseUrl: "http://127.0.0.1:1234/v1", models: ["google/gemma-4-e4b"] },
];

async function find(name: string) {
  mocked.mockImplementation(async (command: string) => {
    if (command === "model_settings") return { chat: null, embeddings: null };
    if (command === "find_local_models") return servers(name);
    return null;
  });
  render(<Models />);
  const button = await screen.findByRole("button", { name: "Find models on this Mac" });
  await act(async () => fireEvent.click(button));
}

describe("models on this Mac", () => {
  test("LM Studio gets the advice to keep both models loaded", async () => {
    await find("LM Studio");
    expect(await screen.findByText(/Only Keep Last JIT Loaded Model/)).toBeTruthy();
  });

  test("other servers don't", async () => {
    await find("Ollama");
    await screen.findByText(/gemma-4-e4b/);
    expect(screen.queryByText(/Only Keep Last JIT Loaded Model/)).toBeNull();
  });
});

describe("details for a bug report", () => {
  test("are copied and shown, as the Rust side made them", async () => {
    mocked.mockReset();
    mocked.mockImplementation(async (command: string) =>
      command === "bug_report_details" ? "Lax 0.2.0 (commit abc)\nJournal entries: 3.\n" : null,
    );
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<BugReportDetails />);
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Copy details for a bug report" })),
    );
    expect(writeText).toHaveBeenCalledWith("Lax 0.2.0 (commit abc)\nJournal entries: 3.\n");
    expect(screen.getByText(/Copied/)).toBeTruthy();
    expect(document.querySelector("pre.details")?.textContent).toContain("Journal entries: 3.");
  });
});
