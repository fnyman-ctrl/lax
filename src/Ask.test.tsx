import { describe, expect, onTestFinished, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { Ask, answerBlocks } from "./Ask";

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

  test("the answer is followed as it streams, unless the user scrolled up", async () => {
    const question = pending();
    const scrolled = vi.spyOn(window, "scrollTo").mockImplementation(() => {});
    const page = document.documentElement;
    onTestFinished(() => {
      scrolled.mockRestore();
      for (const key of ["scrollHeight", "clientHeight", "scrollTop"]) {
        delete (page as unknown as Record<string, unknown>)[key];
      }
    });
    const setPage = (top: number) => {
      Object.defineProperty(page, "scrollHeight", { configurable: true, value: 2000 });
      Object.defineProperty(page, "clientHeight", { configurable: true, value: 500 });
      Object.defineProperty(page, "scrollTop", { configurable: true, value: top });
      window.dispatchEvent(new Event("scroll"));
    };
    render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));

    // At the end of the page: each piece keeps the window at its end, below
    // the question box, not just at the answer's last line.
    setPage(1500);
    scrolled.mockClear();
    await act(async () => channel().onmessage("You wrote "));
    expect(scrolled).toHaveBeenCalledWith({ top: 2000, behavior: "auto" });

    // Scrolled up to read: the window is left where it is.
    setPage(200);
    scrolled.mockClear();
    await act(async () => channel().onmessage("the quote"));
    expect(scrolled).not.toHaveBeenCalled();

    // A turn of the wheel upward stops following at once, before the
    // window reports the scroll, even this close to the end.
    setPage(1490);
    window.dispatchEvent(new WheelEvent("wheel", { deltaY: -10 }));
    scrolled.mockClear();
    await act(async () => channel().onmessage(" Then"));
    expect(scrolled).not.toHaveBeenCalled();
    setPage(200);

    // The page getting shorter at the end moves the window up, but it is
    // still at the end: followed.
    setPage(1490);
    Object.defineProperty(page, "scrollHeight", { configurable: true, value: 1980 });
    Object.defineProperty(page, "scrollTop", { configurable: true, value: 1480 });
    window.dispatchEvent(new Event("scroll"));
    scrolled.mockClear();
    await act(async () => channel().onmessage(" too"));
    expect(scrolled).toHaveBeenCalled();
    setPage(200);

    // Back down at the end: followed again.
    setPage(1490);
    await act(async () => channel().onmessage("."));
    expect(scrolled).toHaveBeenCalled();

    await act(async () => question.answer(answer("You wrote the quote.")));
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

  test("an answer reads as paragraphs and lists of points", () => {
    expect(answerBlocks("You worked on the quote.\n- Prices\n- Delivery in week 46")).toEqual([
      { kind: "text", text: "You worked on the quote." },
      { kind: "points", points: ["Prices", "Delivery in week 46"] },
    ]);
    // A list written on one line, as small models sometimes do.
    expect(answerBlocks("It covered: - Prices for Example AB. - Delivery.")).toEqual([
      { kind: "text", text: "It covered:" },
      { kind: "points", points: ["Prices for Example AB.", "Delivery."] },
    ]);
    // A dash inside a sentence, or a single one, is not a list.
    expect(answerBlocks("Example AB - the client - called. Then - nothing.")).toEqual([
      { kind: "text", text: "Example AB - the client - called. Then - nothing." },
    ]);
    // A point still being written is a point from its dash.
    expect(answerBlocks("Two things:\n- Prices\n-")).toEqual([
      { kind: "text", text: "Two things:" },
      { kind: "points", points: ["Prices", ""] },
    ]);
    // A blank line ends a paragraph; text after a list starts a new one.
    expect(answerBlocks("One.\n\nTwo.\n- A\nThree.")).toEqual([
      { kind: "text", text: "One." },
      { kind: "text", text: "Two." },
      { kind: "points", points: ["A"] },
      { kind: "text", text: "Three." },
    ]);
  });

  test("an answer is shown as text, never as HTML", async () => {
    const question = pending();
    render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));
    await act(async () => question.answer(answer("Notes:\n- <b>bold</b>\n- **two**")));
    const points = screen.getAllByRole("listitem").map((item) => item.textContent);
    expect(points).toEqual(["<b>bold</b>", "**two**"]);
    expect(document.querySelector(".answer b")).toBeNull();
  });

  test("a new conversation is in plain sight, and suggested when it gets long", async () => {
    render(<Ask onShowJournal={() => {}} />);
    expect(screen.queryByRole("button", { name: "New conversation" })).toBeNull();
    for (let i = 1; i <= 4; i++) {
      const question = pending();
      await act(async () => ask(`Question ${i}?`));
      await act(async () => question.answer(answer(`Answer ${i}.`)));
      const tip = screen.queryByText(/keeps at most the last 4 questions/);
      expect(tip === null).toBe(i < 4);
    }
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "New conversation" })),
    );
    expect(screen.queryByText("Answer 1.")).toBeNull();
    expect(screen.queryByRole("button", { name: "New conversation" })).toBeNull();
  });
});

describe("a question from elsewhere", () => {
  test("is asked at once, and again only for a new request", async () => {
    mocked.mockReset();
    mocked.mockResolvedValue(answer("Anna Example leads the Q3 launch."));
    const { rerender } = render(
      <Ask onShowJournal={() => {}} request={{ question: "Brief me on Anna Example", n: 1 }} />,
    );
    await act(async () => {});
    expect(mocked).toHaveBeenCalledTimes(1);
    expect(mocked.mock.calls[0][1]).toMatchObject({ question: "Brief me on Anna Example" });
    // The same request again, with nothing new: not asked twice.
    rerender(
      <Ask onShowJournal={() => {}} request={{ question: "Brief me on Anna Example", n: 1 }} />,
    );
    await act(async () => {});
    expect(mocked).toHaveBeenCalledTimes(1);
    rerender(<Ask onShowJournal={() => {}} request={{ question: "Brief me on Bo", n: 2 }} />);
    await act(async () => {});
    expect(mocked).toHaveBeenCalledTimes(2);
  });

  test("waits in the box while another question is answered", async () => {
    mocked.mockReset();
    pending();
    const { rerender } = render(<Ask onShowJournal={() => {}} />);
    await act(async () => ask("What did I do yesterday?"));
    rerender(<Ask onShowJournal={() => {}} request={{ question: "Brief me on Bo", n: 1 }} />);
    await act(async () => {});
    expect(mocked).toHaveBeenCalledTimes(1);
    const box = screen.getByRole("textbox", { name: "Question" }) as HTMLTextAreaElement;
    expect(box.value).toBe("Brief me on Bo");
  });
});
