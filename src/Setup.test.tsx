import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Setup } from "./Setup";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

describe("the first start", () => {
  test("advises a model for the Mac's memory, and everything starts off", async () => {
    mocked.mockImplementation(async (command: string) => {
      if (command === "model_settings") return { chat: null, embeddings: null };
      if (command.endsWith("_status")) return { enabled: false };
      return null;
    });
    const done = vi.fn();
    render(<Setup memoryGb={16} onDone={done} />);
    expect(screen.getByText(/16 GB of memory/).textContent).toContain("Gemma 4 E4B");

    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Next" })));
    for (const box of screen.getAllByRole("checkbox")) {
      expect((box as HTMLInputElement).checked).toBe(false);
    }

    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Start using Lax" })),
    );
    expect(mocked).toHaveBeenCalledWith("finish_setup");
    expect(done).toHaveBeenCalled();
  });

  test("turning a feature on asks Touch ID in Rust; a cancel leaves it off and says nothing", async () => {
    let answer: string | null = null;
    mocked.mockReset();
    mocked.mockImplementation(async (command: string) => {
      if (command === "model_settings") return { chat: null, embeddings: null };
      if (command.endsWith("_status")) return { enabled: false };
      if (command === "set_journal_enabled") throw answer;
      return null;
    });
    render(<Setup memoryGb={16} onDone={() => {}} />);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Next" })));
    const journal = () => screen.getAllByRole("checkbox")[0] as HTMLInputElement;

    await act(async () => fireEvent.click(journal()));
    expect(mocked).toHaveBeenCalledWith("set_journal_enabled", { enabled: true });
    expect(journal().checked).toBe(false);
    expect(screen.queryByRole("alert")).toBeNull();

    answer = "Touch ID or the password wasn't accepted.";
    await act(async () => fireEvent.click(journal()));
    expect(journal().checked).toBe(false);
    expect(screen.getByRole("alert").textContent).toBe(answer);
  });

  test("a small Mac gets a small model", () => {
    mocked.mockResolvedValue(null);
    render(<Setup memoryGb={8} onDone={() => {}} />);
    expect(screen.getByText(/8 GB of memory/).textContent).toContain("2–4 billion");
  });
});
