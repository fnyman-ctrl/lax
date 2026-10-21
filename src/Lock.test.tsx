import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import { LockGate } from "./Lock";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const mocked = vi.mocked(invoke);
const listened = vi.mocked(listen);

/** The window is locked until `unlock` answers with `answer`: nothing for
 * success, else the rejection Rust sends. */
function lockedUntil(answer?: { reject: string | null }) {
  let locked = true;
  mocked.mockReset();
  mocked.mockImplementation(async (command: string) => {
    if (command === "lock_status") {
      return { locked };
    }
    if (command === "unlock") {
      if (answer) {
        throw answer.reject;
      }
      locked = false;
      return null;
    }
    return null;
  });
}

async function show() {
  await act(async () =>
    render(
      <LockGate>
        <p>The app</p>
      </LockGate>,
    ),
  );
}

const unlockButton = () => screen.getByRole("button", { name: "Unlock" });

describe("the lock screen", () => {
  test("locked, only the lock screen shows, and nothing asks on its own", async () => {
    lockedUntil();
    await show();
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByRole("heading").textContent).toBe("Lax is locked");
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["Unlock"]);
    expect(mocked).not.toHaveBeenCalledWith("unlock");
  });

  test("unlocked, the app shows", async () => {
    lockedUntil();
    mocked.mockImplementation(async () => ({ locked: false }));
    await show();
    expect(screen.getByText("The app")).toBeTruthy();
    expect(screen.queryByText("Lax is locked")).toBeNull();
  });

  test("Unlock calls unlock, and then the app shows", async () => {
    lockedUntil();
    await show();
    await act(async () => fireEvent.click(unlockButton()));
    expect(mocked).toHaveBeenCalledWith("unlock");
    expect(screen.getByText("The app")).toBeTruthy();
  });

  test("a cancel shows no message, and the lock screen stays", async () => {
    lockedUntil({ reject: null });
    await show();
    await act(async () => fireEvent.click(unlockButton()));
    expect(mocked).toHaveBeenCalledWith("unlock");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("The app")).toBeNull();
    expect(unlockButton()).toBeTruthy();
  });

  test("a failure shows its message, and the lock screen stays", async () => {
    lockedUntil({ reject: "Touch ID or the password wasn't accepted." });
    await show();
    await act(async () => fireEvent.click(unlockButton()));
    expect(screen.getByRole("alert").textContent).toBe(
      "Touch ID or the password wasn't accepted.",
    );
    expect(screen.queryByText("The app")).toBeNull();
    // Trying again clears it.
    lockedUntil();
    await act(async () => fireEvent.click(unlockButton()));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("The app")).toBeTruthy();
  });

  test("a status that can't be read counts as locked", async () => {
    mocked.mockReset();
    mocked.mockImplementation(async () => {
      throw "no";
    });
    await show();
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByText("Lax is locked")).toBeTruthy();
  });

  test("when Lax locks, the app goes at once", async () => {
    let handler: ((event: { payload: unknown }) => void) | undefined;
    listened.mockImplementation(async (_event, h) => {
      handler = h as typeof handler;
      return () => {};
    });
    lockedUntil();
    mocked.mockImplementation(async () => ({ locked: false }));
    await show();
    expect(screen.getByText("The app")).toBeTruthy();
    // Before the status is read again, the event alone hides the app.
    mocked.mockImplementation(() => new Promise(() => {}));
    await act(async () => handler?.({ payload: { locked: true, message: null } }));
    expect(screen.queryByText("The app")).toBeNull();
    expect(screen.getByText("Lax is locked")).toBeTruthy();
  });

  test("an unlock asked when the window opened tells why it failed", async () => {
    let handler: ((event: { payload: unknown }) => void) | undefined;
    listened.mockImplementation(async (_event, h) => {
      handler = h as typeof handler;
      return () => {};
    });
    lockedUntil();
    await show();
    await act(async () =>
      handler?.({ payload: { locked: true, message: "macOS closed the dialog. Try again." } }),
    );
    expect(screen.getByRole("alert").textContent).toBe("macOS closed the dialog. Try again.");
  });
});

describe("the window", () => {
  test("locked, it reads nothing but whether the database is open and the lock", async () => {
    lockedUntil();
    const status = mocked.getMockImplementation()!;
    mocked.mockImplementation(async (command: string, args?: unknown) =>
      command === "database_status" ? { state: "open" } : status(command, args as never),
    );
    await act(async () => render(<App />));
    expect(screen.getByText("Lax is locked")).toBeTruthy();
    const commands = new Set(mocked.mock.calls.map(([command]) => command));
    expect([...commands].sort()).toEqual(["database_status", "lock_status"]);
  });
});
