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
    command === "database_status" ? { state, postponed: null } : null,
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
  });

  test("a locked keychain offers only to try again", async () => {
    await show("keyUnavailable");
    expect(screen.queryByRole("button", { name: "Start over…" })).toBeNull();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Try again" })));
    expect(mocked).toHaveBeenCalledWith("retry_opening");
  });

  test("a plaintext database is encrypted after the notice", async () => {
    await show("convert");
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByText(/one-way/)).toBeTruthy();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Encrypt" })));
    expect(mocked).toHaveBeenCalledWith("continue_opening");
  });

  test("a postponed conversion says so above the app", async () => {
    mocked.mockReset();
    mocked.mockImplementation(async (command: string) =>
      command === "database_status" ? { state: "open", postponed: "lowSpace" } : null,
    );
    await act(async () =>
      render(
        <DatabaseGate>
          <p>The app</p>
        </DatabaseGate>,
      ),
    );
    expect(screen.getByText("The app")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("free space");
  });
});
