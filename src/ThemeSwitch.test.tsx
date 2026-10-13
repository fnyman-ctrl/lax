import { describe, expect, test, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ThemeSwitch } from "./ThemeSwitch";

const setTheme = vi.fn(() => Promise.resolve());
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ setTheme }) }));

describe("the appearance switch", () => {
  test("picks light or dark for the window and remembers it; As macOS undoes it", () => {
    localStorage.clear();
    render(<ThemeSwitch />);
    expect(screen.getByRole("button", { name: "As macOS" }).getAttribute("aria-pressed")).toBe(
      "true",
    );

    fireEvent.click(screen.getByRole("button", { name: "Dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(setTheme).toHaveBeenLastCalledWith("dark");
    expect(localStorage.getItem("theme")).toBe("dark");

    fireEvent.click(screen.getByRole("button", { name: "As macOS" }));
    expect(document.documentElement.dataset.theme).toBeUndefined();
    expect(setTheme).toHaveBeenLastCalledWith(null);
  });
});
