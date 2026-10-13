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

  test("a small Mac gets a small model", () => {
    mocked.mockResolvedValue(null);
    render(<Setup memoryGb={8} onDone={() => {}} />);
    expect(screen.getByText(/8 GB of memory/).textContent).toContain("2–4 billion");
  });
});
