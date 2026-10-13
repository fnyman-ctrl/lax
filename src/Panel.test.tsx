import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Panel } from "./Panel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

/** The panel's commands, answered as the app would. */
function app(draft = "Hi Anna,\n\nThanks for the quote!") {
  mocked.mockImplementation(async (command: string) => {
    switch (command) {
      case "panel_call":
        return { id: 1, app: "Mail", instruction: "reply and thank her" };
      case "write_draft":
        return { draft, pages: ["you"], readWindow: true };
      case "fit_panel":
        return false;
      default:
        return null;
    }
  });
}

describe("the writing assistant's panel", () => {
  test("Enter writes a draft, and Enter again inserts the edited draft", async () => {
    app();
    render(<Panel />);
    const instruction = await screen.findByDisplayValue("reply and thank her");

    await act(async () => fireEvent.keyDown(instruction, { key: "Enter" }));
    expect(mocked).toHaveBeenCalledWith("write_draft", { instruction: "reply and thank her" });

    const draft = await screen.findByRole("textbox", { name: "The draft" });
    fireEvent.change(draft, { target: { value: "Hi Anna, thanks!" } });
    await act(async () => fireEvent.keyDown(draft, { key: "Enter" }));
    expect(mocked).toHaveBeenCalledWith("insert_draft", { text: "Hi Anna, thanks!" });
  });

  test("Escape closes the panel", async () => {
    app();
    render(<Panel />);
    await screen.findByDisplayValue("reply and thank her");
    await act(async () => fireEvent.keyDown(window, { key: "Escape" }));
    expect(mocked).toHaveBeenCalledWith("close_panel");
  });

  test("Enter while an input method composes doesn't send", async () => {
    app();
    render(<Panel />);
    const instruction = await screen.findByDisplayValue("reply and thank her");
    await act(async () => fireEvent.keyDown(instruction, { key: "Enter", isComposing: true }));
    expect(mocked).not.toHaveBeenCalledWith("write_draft", expect.anything());
  });

  test("the draft is shown as text, never as markup", async () => {
    app('<img src=x onerror="alert(1)">');
    const { container } = render(<Panel />);
    const instruction = await screen.findByDisplayValue("reply and thank her");
    await act(async () => fireEvent.keyDown(instruction, { key: "Enter" }));
    await screen.findByRole("textbox", { name: "The draft" });
    expect(container.querySelector("img[src='x']")).toBeNull();
  });
});
