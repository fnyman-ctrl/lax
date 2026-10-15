import { afterEach, describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { PageEditor } from "./PageEditor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

const PAGE = "# Anna Example\n\n<!-- lax:begin x -->\n- Leads the Q3 launch.\n<!-- lax:end -->\n";

afterEach(() => vi.useRealTimers());

function saves() {
  return mocked.mock.calls.filter(([command]) => command === "save_page").map(([, args]) => args);
}

async function open(saveResult: (args: unknown) => unknown = () => ({ saved: true, current: null })) {
  vi.useFakeTimers();
  mocked.mockReset();
  mocked.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "page_text") return PAGE;
    if (command === "save_page") return saveResult(args);
    return null;
  });
  const view = render(<PageEditor entity={7} onClose={() => {}} />);
  await act(async () => {});
  return view;
}

async function type(text: string) {
  const box = screen.getByRole("textbox", { name: "Page text" });
  await act(async () => fireEvent.change(box, { target: { value: text } }));
  await act(async () => vi.advanceTimersByTime(2000));
}

describe("the page editor", () => {
  test("shows the page as plain text, never as HTML", async () => {
    await open();
    const box = screen.getByRole("textbox", { name: "Page text" }) as HTMLTextAreaElement;
    expect(box.value).toBe(PAGE);
    expect(mocked).toHaveBeenCalledWith("page_text", { entity: 7 });
  });

  test("saves over what it loaded, a new version only on the first save", async () => {
    await open();
    await type(`${PAGE}My note.\n`);
    await type(`${PAGE}My note.\nAnother.\n`);
    expect(saves()).toEqual([
      { entity: 7, loaded: PAGE, text: `${PAGE}My note.\n`, first: true },
      { entity: 7, loaded: `${PAGE}My note.\n`, text: `${PAGE}My note.\nAnother.\n`, first: false },
    ]);
  });

  test("a page changed meanwhile shows both texts, and the user's can be kept", async () => {
    let calls = 0;
    await open(() => (++calls === 1 ? { saved: false, current: "Changed by Lax" } : { saved: true, current: null }));
    await type(`${PAGE}My note.\n`);
    expect(screen.getByRole("alertdialog", { name: "The page changed" })).toBeTruthy();
    expect((screen.getByRole("textbox", { name: "The page's text now" }) as HTMLTextAreaElement).value).toBe(
      "Changed by Lax",
    );
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Keep my text" })));
    expect(saves()[1]).toEqual({ entity: 7, loaded: "Changed by Lax", text: `${PAGE}My note.\n`, first: true });
  });

  test("closing saves what is left before Lax may write the page", async () => {
    const view = await open();
    const box = screen.getByRole("textbox", { name: "Page text" });
    await act(async () => fireEvent.change(box, { target: { value: `${PAGE}Unsaved.\n` } }));
    await act(async () => view.unmount());
    const order = mocked.mock.calls.map(([command]) => command);
    expect(order.slice(-2)).toEqual(["save_page", "close_page"]);
    expect(mocked).toHaveBeenLastCalledWith("close_page", { entity: 7 });
  });

  test("a page that is gone says so, and the user's text stays", async () => {
    await open(() => ({ saved: false, current: null }));
    await type(`${PAGE}Mine.\n`);
    expect(screen.getByText("The page is gone")).toBeTruthy();
    expect((screen.getByRole("textbox", { name: "Page text" }) as HTMLTextAreaElement).value).toBe(
      `${PAGE}Mine.\n`,
    );
    expect(screen.queryByRole("button", { name: "Use the page's text" })).toBeNull();
  });
});
