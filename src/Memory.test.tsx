import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import {
  type EntityItem,
  MergeSuggestions,
  NewNames,
  ObviousPairs,
  OldPageFiles,
  type PairItem,
  PartOfControls,
  SameAsPicker,
  type SuggestionItem,
  withParts,
} from "./Memory";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

function show(kept: boolean) {
  mocked.mockReset();
  mocked.mockImplementation(async (command: string) =>
    command === "delete_old_page_files" ? { deleted: 2, changed: ["about-me.md"] } : null,
  );
  const run = vi.fn((work: Promise<string>) => work.then(() => true));
  const onKept = vi.fn();
  render(<OldPageFiles files={{ count: 2, kept }} busy={false} run={run} onKept={onKept} />);
  return { run, onKept };
}

describe("the old page files after the move", () => {
  test("are asked about once, with Delete as the default", async () => {
    show(false);
    expect(screen.getByRole("alertdialog").textContent).toContain("doesn't reliably erase");
    const remove = screen.getByRole("button", { name: "Delete the old files" });
    expect(document.activeElement).toBe(remove);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Keep them" })));
    expect(mocked).toHaveBeenCalledWith("keep_old_page_files");
    expect(mocked).not.toHaveBeenCalledWith("delete_old_page_files");
  });

  test("deleting says what was kept", async () => {
    const { run } = show(false);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Delete the old files" })));
    expect(mocked).toHaveBeenCalledWith("delete_old_page_files");
    const message = await run.mock.calls[0][0];
    expect(message).toBe("Deleted 2 old files. Kept about-me.md: changed after the move.");
  });

  test("after Keep them, a quiet line still offers it, asked once more", async () => {
    show(true);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Delete the old files" }));
    expect(screen.getByRole("alertdialog")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mocked).not.toHaveBeenCalledWith("delete_old_page_files");
  });
});

function entity(id: number, name: string, isNew: boolean, mentions = 1): EntityItem {
  return {
    id,
    kind: "person",
    name,
    aliases: [],
    mentions,
    days: 1,
    lastSeen: 0,
    isNew,
    partOf: null,
    page: null,
  };
}

describe("new names", () => {
  const row = (e: EntityItem) => <li key={e.id}>{e.name}</li>;

  test("are folded away, counted, in the order they come", () => {
    render(
      <NewNames
        entities={[
          entity(1, "Anna Example", true, 3),
          entity(2, "Bo Exempel", false, 9),
          entity(3, "Cecilia Example", true, 1),
        ]}
        row={row}
      />,
    );
    const folded = document.querySelector("details.new-names") as HTMLDetailsElement;
    expect(folded.open).toBe(false);
    expect(folded.querySelector("summary")?.textContent).toContain("Lax is watching 2 new names.");
    expect([...folded.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      "Anna Example",
      "Cecilia Example",
    ]);
  });

  test("one name reads as one", () => {
    render(<NewNames entities={[entity(1, "Anna Example", true)]} row={row} />);
    expect(screen.getByText(/Lax is watching one new name\./)).toBeTruthy();
  });

  test("with nothing new, there is no New section", () => {
    const { container } = render(
      <NewNames entities={[entity(1, "Anna Example", false)]} row={row} />,
    );
    expect(container.textContent).toBe("");
  });
});

describe("obvious pairs", () => {
  const pair = (fromId: number, from: string, intoId: number, into: string): PairItem => ({
    from: { id: fromId, name: from },
    into: { id: intoId, name: into },
  });

  function show(pairs: PairItem[]) {
    mocked.mockReset();
    mocked.mockImplementation(async (command: string) =>
      command === "merge_obvious_pairs" ? 2 : null,
    );
    const run = vi.fn((work: Promise<string>) => work.then(() => true));
    render(<ObviousPairs pairs={pairs} busy={false} run={run} />);
    return run;
  }

  test("merge one into the name that stays", async () => {
    const run = show([pair(2, "Exempelbolaget", 1, "Exempelbolaget AB")]);
    await act(async () =>
      fireEvent.click(
        screen.getByRole("button", { name: "Merge Exempelbolaget into Exempelbolaget AB" }),
      ),
    );
    expect(mocked).toHaveBeenCalledWith("merge_entities", { from: 2, into: 1 });
    expect(await run.mock.calls[0][0]).toBe("Exempelbolaget is now part of Exempelbolaget AB.");
    // One pair: no Merge all.
    expect(screen.queryByRole("button", { name: "Merge all" })).toBeNull();
  });

  test("not the same is sent for the pair", async () => {
    show([pair(2, "Exempelbolaget", 1, "Exempelbolaget AB")]);
    await act(async () =>
      fireEvent.click(
        screen.getByRole("button", {
          name: "Exempelbolaget and Exempelbolaget AB are not the same",
        }),
      ),
    );
    expect(mocked).toHaveBeenCalledWith("not_same", { a: 2, b: 1 });
  });

  test("merge all sends exactly the pairs shown", async () => {
    const run = show([
      pair(2, "Exempelbolaget", 1, "Exempelbolaget AB"),
      pair(4, "Example Labs", 3, "Example Labs Inc"),
    ]);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Merge all" })));
    expect(mocked).toHaveBeenCalledWith("merge_obvious_pairs", {
      pairs: [
        [2, 1],
        [4, 3],
      ],
    });
    expect(await run.mock.calls[0][0]).toBe("Merged 2 pairs.");
  });

  test("nothing to suggest, nothing shown", () => {
    const { container } = render(<ObviousPairs pairs={[]} busy={false} run={vi.fn()} />);
    expect(container.textContent).toBe("");
  });
});

describe("merge suggestions", () => {
  const suggestion = (
    fromId: number,
    from: string,
    intoId: number,
    into: string,
    reason = "Same team.",
  ): SuggestionItem => ({
    kind: "person",
    from: { id: fromId, name: from },
    into: { id: intoId, name: into },
    reason,
  });

  function show(suggestions: SuggestionItem[]) {
    mocked.mockReset();
    mocked.mockImplementation(async () => null);
    const run = vi.fn((work: Promise<string>) => work.then(() => true));
    render(<MergeSuggestions suggestions={suggestions} busy={false} run={run} />);
    return run;
  }

  test("a pair shows the model's reason as text, with Merge and Not the same", async () => {
    show([suggestion(2, "Anna", 1, "Anna Example", "<b>Same</b> team.")]);
    expect(screen.getByText(/look like the same person/)).toBeTruthy();
    // Shown as text, never as markup.
    expect(screen.getByText("<b>Same</b> team.")).toBeTruthy();
    expect(document.querySelector(".suggestions b")).toBeNull();
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Merge Anna into Anna Example" })),
    );
    expect(mocked).toHaveBeenCalledWith("merge_entities", { from: 2, into: 1 });
  });

  test("a short name that fits several is asked about once", async () => {
    const run = show([
      suggestion(2, "Anna", 1, "Anna Example"),
      suggestion(2, "Anna", 3, "Anna Other"),
    ]);
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByRole("listitem").textContent).toContain(
      "Anna could be Anna Example or Anna Other.",
    );
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Same as Anna Other" })),
    );
    expect(mocked).toHaveBeenCalledWith("merge_entities", { from: 2, into: 3 });
    mocked.mockClear();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "None of them" })));
    expect(mocked).toHaveBeenCalledWith("not_same", { a: 2, b: 1 });
    expect(mocked).toHaveBeenCalledWith("not_same", { a: 2, b: 3 });
    expect(await run.mock.calls[1][0]).toBe("Lax won't suggest them again.");
  });

  test("nothing suggested, nothing shown", () => {
    const { container } = render(
      <MergeSuggestions suggestions={[]} busy={false} run={vi.fn()} />,
    );
    expect(container.textContent).toBe("");
  });
});

describe("the Same as list", () => {
  const named = (id: number, name: string, aliases: string[] = []): EntityItem => ({
    ...entity(id, name, false),
    aliases,
  });

  function pick(others: EntityItem[], likely: number[], value = "") {
    const onChange = vi.fn();
    render(
      <SameAsPicker id="same" others={others} likely={likely} value={value} onChange={onChange} />,
    );
    return onChange;
  }

  const options = () =>
    [...document.querySelectorAll("optgroup, option")].map((o) =>
      o.tagName === "OPTGROUP" ? `[${o.getAttribute("label")}]` : o.textContent,
    );

  test("likely names come first, in their order, then all the others", () => {
    pick([named(1, "Anna Other"), named(2, "Bo Exempel"), named(3, "Anna Example")], [3, 1]);
    expect(options()).toEqual([
      "Choose…",
      "[Likely]",
      "Anna Example",
      "Anna Other",
      "[All names]",
      "Bo Exempel",
    ]);
  });

  test("with nothing likely, a plain list", () => {
    pick([named(1, "Anna Other"), named(2, "Bo Exempel")], []);
    expect(options()).toEqual(["Choose…", "Anna Other", "Bo Exempel"]);
    expect(screen.queryByRole("searchbox")).toBeNull();
  });

  test("many names can be searched, by any name they go by", () => {
    const many = Array.from({ length: 16 }, (_, i) => named(i + 1, `Example ${i + 1}`));
    many.push(named(99, "Cecilia Example", ["C. Exempel"]));
    const onChange = pick(many, [99], "5");
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "exempel" } });
    expect(options()).toEqual(["Choose…", "[Likely]", "Cecilia Example"]);
    // The chosen name was filtered away: the choice is cleared.
    expect(onChange).toHaveBeenCalledWith("");
  });
});

describe("projects in parts", () => {
  const project = (id: number, name: string, partOf: number | null = null): EntityItem => ({
    ...entity(id, name, false),
    kind: "project",
    partOf,
  });

  test("parts follow their whole; a part whose whole isn't listed stays put", () => {
    const list = [
      project(3, "Export", 1),
      project(1, "Exempelappen"),
      project(2, "Other"),
      project(4, "Drafts", 99),
    ];
    expect(withParts(list).map((e) => e.name)).toEqual([
      "Exempelappen",
      "Export",
      "Other",
      "Drafts",
    ]);
  });

  function controls(of: EntityItem, all: EntityItem[]) {
    mocked.mockReset();
    mocked.mockImplementation(async () => null);
    const run = vi.fn((work: Promise<string>) => work.then(() => true));
    render(<PartOfControls entity={of} all={all} busy={false} run={run} />);
    return run;
  }

  test("a project can be made part of a project that isn't a part", async () => {
    const all = [project(1, "Exempelappen"), project(2, "Export"), project(3, "Drafts", 1)];
    const run = controls(all[1], all);
    const options = [...document.querySelectorAll("option")].map((o) => o.textContent);
    // Drafts is a part: it can't be a whole.
    expect(options).toEqual(["Choose…", "Exempelappen"]);
    fireEvent.change(screen.getByLabelText("Part of"), { target: { value: "1" } });
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Make it a part" })));
    expect(mocked).toHaveBeenCalledWith("set_part_of", { part: 2, whole: 1 });
    expect(await run.mock.calls[0][0]).toBe("Export is now part of Exempelappen.");
  });

  test("a part can be taken out", async () => {
    const all = [project(1, "Exempelappen"), project(2, "Export", 1)];
    controls(all[1], all);
    expect(screen.getByText("Exempelappen")).toBeTruthy();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Take out" })));
    expect(mocked).toHaveBeenCalledWith("take_out_part", { part: 2 });
  });

  test("a project with parts can't be a part", () => {
    const all = [project(1, "Exempelappen"), project(2, "Export", 1), project(3, "Other")];
    controls(all[0], all);
    expect(screen.queryByLabelText("Part of")).toBeNull();
    expect(screen.getByText(/has parts, so it can't be a part itself/)).toBeTruthy();
  });
});
