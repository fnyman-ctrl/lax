import { describe, expect, test, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { OneCapture } from "./Viewer";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

describe("a capture opened from Ask", () => {
  test("is read by its id and shown as text, with a way back", async () => {
    mocked.mockResolvedValueOnce({
      id: 7,
      capturedAt: new Date(2026, 9, 9, 16, 5).getTime(),
      appName: "Example Browser",
      windowTitle: "<b>SQLCipher</b> – example.com",
      site: "example.com",
      text: "SQLCipher encrypts the whole database file.",
    });
    const onBack = vi.fn();
    render(<OneCapture id={7} onBack={onBack} />);
    await act(async () => {});
    expect(mocked).toHaveBeenCalledWith("capture_detail", { id: 7 });
    expect(screen.getByRole("heading", { name: "<b>SQLCipher</b> – example.com" })).toBeTruthy();
    expect(document.querySelector(".one-capture b")).toBeNull();
    expect(screen.getByText("SQLCipher encrypts the whole database file.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "All captures" }));
    expect(onBack).toHaveBeenCalled();
  });

  test("says so when the capture is gone", async () => {
    mocked.mockResolvedValueOnce(null);
    render(<OneCapture id={8} onBack={() => {}} />);
    await act(async () => {});
    expect(screen.getByText(/This capture is gone/)).toBeTruthy();
  });
});
