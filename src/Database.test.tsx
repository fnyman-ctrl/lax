import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { DatabaseGate } from "./Database";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const mocked = vi.mocked(invoke);

function withState(state: string) {
  mocked.mockReset();
  mocked.mockImplementation(async (command: string) =>
    command === "database_status" ? { state } : null,
  );
}

async function show(state: string) {
  withState(state);
  await act(async () =>
    render(
      <DatabaseGate>
        <p>The app</p>
      </DatabaseGate>,
    ),
  );
}

describe("opening the database", () => {
  test("the app shows once the database is open", async () => {
    await show("open");
    expect(screen.getByText("The app")).toBeTruthy();
  });

  test("nothing of the app shows while it opens, and the wait is said only if it lasts", async () => {
    vi.useFakeTimers();
    try {
      await show("opening");
      expect(screen.queryByText("The app")).toBeNull();
      expect(screen.queryByText("Opening the database…")).toBeNull();
      await act(async () => vi.advanceTimersByTime(500));
      expect(screen.getByText("Opening the database…")).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  test("after an update, the key is read only after Continue", async () => {
    await show("update");
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByText(/macOS will ask once/).textContent).toContain("Always Allow");
    expect(mocked).not.toHaveBeenCalledWith("continue_opening");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Continue" })));
    expect(mocked).toHaveBeenCalledWith("continue_opening");
  });

  test("starting over is asked first, and can be called off", async () => {
    await show("keyMissing");
    expect(screen.getByRole("heading").textContent).toBe("Lax can't find its key");
    fireEvent.click(screen.getByRole("button", { name: "Start over…" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mocked).not.toHaveBeenCalledWith("start_over");
    fireEvent.click(screen.getByRole("button", { name: "Start over…" }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Start over" })));
    expect(mocked).toHaveBeenCalledWith("start_over");
    // Start over asks for Touch ID itself, in Rust (ADR 0012): no unlock
    // first, so the user sees one dialog, not two.
    expect(mocked).not.toHaveBeenCalledWith("unlock");
  });

  test("a cancelled Touch ID for starting over says nothing", async () => {
    await show("cannotUnlock");
    mocked.mockImplementation(async (command: string) => {
      if (command === "start_over") {
        throw null;
      }
      return command === "database_status" ? { state: "cannotUnlock" } : null;
    });
    fireEvent.click(screen.getByRole("button", { name: "Start over…" }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Start over" })));
    expect(mocked).toHaveBeenCalledWith("start_over");
    expect(screen.queryByRole("alert")).toBeNull();
    // The screen is as it was, with Start over still offered.
    expect(screen.getByRole("button", { name: "Start over…" })).toBeTruthy();
  });

  test("a failed Touch ID for starting over says why", async () => {
    await show("cannotUnlock");
    mocked.mockImplementation(async (command: string) => {
      if (command === "start_over") {
        throw "Touch ID or the password wasn't accepted.";
      }
      return command === "database_status" ? { state: "cannotUnlock" } : null;
    });
    fireEvent.click(screen.getByRole("button", { name: "Start over…" }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Start over" })));
    expect(screen.getByRole("alert").textContent).toBe(
      "Touch ID or the password wasn't accepted.",
    );
  });

  test("a locked keychain offers only to try again", async () => {
    await show("keyUnavailable");
    expect(screen.queryByRole("button", { name: "Start over…" })).toBeNull();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Try again" })));
    expect(mocked).toHaveBeenCalledWith("retry_opening");
  });

  test("a plaintext database names the version that converts it, and nothing acts on it", async () => {
    await show("plaintext");
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByRole("heading").textContent).toBe("Lax can't open its database");
    expect(screen.getByText(/isn't encrypted/).textContent).toBe(
      "This database was made by a Lax from before 0.1 and isn't encrypted. Lax 0.1 can " +
        "convert it: build v0.1.0 once, open it, then update.",
    );
    // No button: nothing here opens, converts or moves the file.
    expect(screen.queryAllByRole("button")).toEqual([]);
    expect(mocked.mock.calls.every(([command]) => command === "database_status")).toBe(true);
  });
});
