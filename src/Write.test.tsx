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

  test("a cancelled Touch ID leaves it off and says nothing", async () => {
    mocked.mockReset();
    mocked.mockImplementation(async (command: string) => {
      if (command === "writing_status") return { enabled: false, key: "option" };
      if (command === "set_writing_enabled") throw null;
      return null;
    });
    const { container } = render(<Write />);
    const turnOn = await screen.findByRole("button", { name: "Turn the writing assistant on" });
    await act(async () => fireEvent.click(turnOn));
    expect(mocked).toHaveBeenCalledWith("set_writing_enabled", { enabled: true });
    expect(screen.getByRole("button", { name: "Turn the writing assistant on" })).toBeTruthy();
    expect(container.textContent).not.toContain("null");
    expect(container.querySelector(".error")).toBeNull();
  });
});
