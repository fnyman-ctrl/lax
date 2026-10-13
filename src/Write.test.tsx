import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Write } from "./Write";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

describe("Write", () => {
  test("another key can be picked to call Lax", async () => {
    mocked.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "writing_status") return { enabled: true, key: "option" };
      if (command === "set_writing_key") {
        return { enabled: true, key: (args as { key: string }).key };
      }
      return null;
    });
    render(<Write />);
    expect(await screen.findByText("Double-tap ⌥ Option")).toBeTruthy();

    const select = screen.getByRole("combobox");
    await act(async () => fireEvent.change(select, { target: { value: "rightOption" } }));
    expect(mocked).toHaveBeenCalledWith("set_writing_key", { key: "rightOption" });
    expect(await screen.findByText("Double-tap Right ⌥ Option")).toBeTruthy();
  });
});
