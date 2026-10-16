import { describe, expect, test, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { PowerNote } from "./Journal";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("the journal's power note", () => {
  test("on battery it says the journal writes about once an hour", () => {
    render(<PowerNote power="battery" />);
    expect(screen.getByRole("status").textContent).toContain("about once an hour");
  });

  test("on a low battery it says the journal waits for power", () => {
    render(<PowerNote power="lowBattery" />);
    expect(screen.getByRole("status").textContent).toContain("waits for power");
  });

  test("on power it says nothing", () => {
    const { container } = render(<PowerNote power="mains" />);
    expect(container.textContent).toBe("");
  });
});
