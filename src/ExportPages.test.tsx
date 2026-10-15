import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ExportPages } from "./ExportPages";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

function picks(folder: { name: string; synced: string | null } | null) {
  mocked.mockReset();
  mocked.mockImplementation(async (command: string) => {
    if (command === "pick_export_folder") return folder;
    if (command === "export_pages") return { pages: 3, folder: "Lax pages 2026-10-15" };
    return null;
  });
}

async function choose() {
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Choose a folder…" })));
}

describe("exporting pages", () => {
  test("says first that the copy isn't encrypted, and writes nothing before a folder", async () => {
    picks(null);
    render(<ExportPages scope="all" onClose={() => {}} />);
    expect(screen.getByRole("alertdialog").textContent).toContain("not encrypted");
    await choose();
    expect(mocked).toHaveBeenCalledWith("pick_export_folder");
    expect(mocked).not.toHaveBeenCalledWith("export_pages", expect.anything());
    expect(screen.getByRole("button", { name: "Choose a folder…" })).toBeTruthy();
  });

  test("writes into an ordinary folder and says where", async () => {
    picks({ name: "Documents", synced: null });
    render(<ExportPages scope="all" onClose={() => {}} />);
    await choose();
    expect(mocked).toHaveBeenCalledWith("export_pages", {
      entity: null,
      all: true,
      syncedConfirmed: false,
    });
    expect(screen.getByRole("status").textContent).toBe(
      "Exported 3 pages to “Lax pages 2026-10-15” in Documents.",
    );
  });

  test("asks again before a folder that is synced off this Mac", async () => {
    picks({ name: "Export", synced: "iCloud Drive" });
    render(<ExportPages scope={{ entity: 7 }} onClose={() => {}} />);
    await choose();
    expect(mocked).not.toHaveBeenCalledWith("export_pages", expect.anything());
    expect(screen.getByRole("heading").textContent).toBe("“Export” is in iCloud Drive");
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Export there anyway" })),
    );
    expect(mocked).toHaveBeenCalledWith("export_pages", {
      entity: 7,
      all: false,
      syncedConfirmed: true,
    });
  });
});
