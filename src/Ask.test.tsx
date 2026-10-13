import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Ask } from "./Ask";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

const answer = (text: string) => ({ answer: text, sources: [], considered: 2, recentOnly: false });

function ask(text: string) {
  const box = screen.getByRole("textbox", { name: "Question" });
  fireEvent.change(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
}

describe("Ask", () => {
  test("a follow-up goes with the turns before it", async () => {
    mocked.mockResolvedValueOnce(answer("You wrote the quote."));
    mocked.mockResolvedValueOnce(answer("You called Anna Example."));
    render(<Ask onShowJournal={() => {}} />);

    await act(async () => ask("What did I do yesterday?"));
    expect(mocked).toHaveBeenLastCalledWith("ask_memory", {
      question: "What did I do yesterday?",
      earlier: [],
    });
    expect(await screen.findByText("You wrote the quote.")).toBeTruthy();

    await act(async () => ask("And the day before?"));
    expect(mocked).toHaveBeenLastCalledWith("ask_memory", {
      question: "And the day before?",
      earlier: [{ question: "What did I do yesterday?", answer: "You wrote the quote." }],
    });
    expect(await screen.findByText("You called Anna Example.")).toBeTruthy();
  });

  test("the salmon thinks while the answer comes, and a failed question comes back", async () => {
    let fail: (reason: string) => void = () => {};
    mocked.mockReturnValueOnce(new Promise((_, reject) => (fail = reject)));
    render(<Ask onShowJournal={() => {}} />);

    await act(async () => ask("What did I do yesterday?"));
    expect(screen.getByLabelText("Lax is thinking")).toBeTruthy();

    await act(async () => fail("Lax couldn't reach the model."));
    expect(screen.getByRole("alert").textContent).toContain("couldn't reach the model");
    // The question is back in the box, to try again.
    expect((screen.getByRole("textbox", { name: "Question" }) as HTMLTextAreaElement).value).toBe(
      "What did I do yesterday?",
    );
  });

  test("Shift+Enter makes a new line instead of asking", async () => {
    render(<Ask onShowJournal={() => {}} />);
    const box = screen.getByRole("textbox", { name: "Question" });
    fireEvent.change(box, { target: { value: "First line" } });
    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(mocked).not.toHaveBeenCalled();
  });
});
