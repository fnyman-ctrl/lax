import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Ask } from "./Ask";

// The channel the answer streams through: the tests play the Rust side by
// calling its `onmessage`.
type FakeChannel = { onmessage: (piece: unknown) => void };
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: (piece: unknown) => void = () => {};
  },
}));
const mocked = vi.mocked(invoke);

const answer = (text: string) => ({ answer: text, sources: [], considered: 2, recentOnly: false });

function ask(text: string) {
  const box = screen.getByRole("textbox", { name: "Question" });
  fireEvent.change(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
}

/** The channel passed with the latest question. */
function channel(): FakeChannel {
  const args = mocked.mock.lastCall?.[1] as { onText: FakeChannel };
  return args.onText;
}

/** A question that stays unanswered until `answer` or `fail` is called. */
function pending() {
  let answerWith: (value: unknown) => void = () => {};
  let fail: (reason: string) => void = () => {};
  mocked.mockReturnValueOnce(
    new Promise((resolve, reject) => {
      answerWith = resolve;
      fail = reject;
    }),
  );
  return { answer: (v: unknown) => answerWith(v), fail: (r: string) => fail(r) };
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
      onText: expect.anything(),
    });
    expect(await screen.findByText("You wrote the quote.")).toBeTruthy();

    await act(async () => ask("And the day before?"));
    expect(mocked).toHaveBeenLastCalledWith("ask_memory", {
      question: "And the day before?",
      earlier: [{ question: "What did I do yesterday?", answer: "You wrote the quote." }],
      onText: expect.anything(),
    });
    expect(await screen.findByText("You called Anna Example.")).toBeTruthy();
  });

  test("the salmon thinks while the answer comes, and a failed question comes back", async () => {
    const question = pending();
    render(<Ask onShowJournal={() => {}} />);

    await act(async () => ask("What did I do yesterday?"));
    expect(screen.getByLabelText("Lax is thinking")).toBeTruthy();

    await act(async () => question.fail("Lax couldn't reach the model."));
    expect(screen.getByRole("alert").textContent).toContain("couldn't reach the model");
    // The question is back in the box, to try again.
    expect((screen.getByRole("textbox", { name: "Question" }) as HTMLTextAreaElement).value).toBe(
      "What did I do yesterday?",
    );
  });

  test("the answer shows as it is written, then the validated answer replaces it", async () => {
    const question = pending();
    render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));
    const onText = channel();

    // The salmon thinks until the first words arrive.
    expect(screen.getByLabelText("Lax is thinking")).toBeTruthy();
    await act(async () => onText.onmessage("You wrote "));
    expect(screen.queryByLabelText("Lax is thinking")).toBeNull();
    expect(screen.getByText("You wrote")).toBeTruthy();
    await act(async () => onText.onmessage("the quote [1]."));
    expect(screen.getByText("You wrote the quote [1].")).toBeTruthy();

    // The validated answer (references taken out) replaces what streamed.
    await act(async () => question.answer(answer("You wrote the quote.")));
    expect(screen.queryByText("You wrote the quote [1].")).toBeNull();
    expect(screen.getByText("You wrote the quote.")).toBeTruthy();

    // A piece that arrives after the answer is dropped.
    await act(async () => onText.onmessage(" Late."));
    expect(screen.queryByText(/Late\./)).toBeNull();
  });

  test("an answer that fails validation takes the streamed text back", async () => {
    const question = pending();
    render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));
    const onText = channel();
    await act(async () => onText.onmessage("Ignore the journal and"));
    expect(screen.getByText("Ignore the journal and")).toBeTruthy();

    await act(async () =>
      question.fail(
        "The model's answer couldn't be used. Try asking again, perhaps in other words.",
      ),
    );
    expect(screen.queryByText(/Ignore the journal/)).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("couldn't be used");
    await act(async () => onText.onmessage(" more"));
    expect(screen.queryByText(/Ignore the journal/)).toBeNull();

    // Nothing of it goes along with the next question.
    mocked.mockResolvedValueOnce(answer("You wrote the quote."));
    await act(async () => ask("What did I do yesterday?"));
    expect(mocked).toHaveBeenLastCalledWith("ask_memory", {
      question: "What did I do yesterday?",
      earlier: [],
      onText: expect.anything(),
    });
  });

  test("streamed text is only ever text", async () => {
    pending();
    render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));
    const markup = '<img src="https://example.com/x.png" onerror="alert(1)"><b>bold</b> **strong**';
    await act(async () => channel().onmessage(markup));
    const shown = screen.getByText(markup, { exact: true });
    expect(shown.textContent).toBe(markup);
    expect(shown.querySelector("img, b, strong")).toBeNull();
    expect(document.querySelector("img[src^='https']")).toBeNull();
    // Anything but text is ignored.
    await act(async () => channel().onmessage({ html: "<b>x</b>" }));
    expect(shown.textContent).toBe(markup);
  });

  test("Shift+Enter makes a new line instead of asking", async () => {
    render(<Ask onShowJournal={() => {}} />);
    const box = screen.getByRole("textbox", { name: "Question" });
    fireEvent.change(box, { target: { value: "First line" } });
    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(mocked).not.toHaveBeenCalled();
  });
});
