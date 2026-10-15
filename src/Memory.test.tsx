import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { OldPageFiles } from "./Memory";

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
