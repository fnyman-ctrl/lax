import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tests of the window's components (src/**/*.test.tsx), in a simulated DOM.
// The Tauri bridge is mocked in each test; nothing talks to the app.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.tsx"],
    setupFiles: ["src/test-setup.ts"],
    restoreMocks: true,
  },
});
