// Light or dark: the system's choice unless the visitor picked one with the
// button. Loaded in <head> so the page is drawn in the right colours at once.
// The choice is kept in this browser only (localStorage), nowhere else.
(() => {
  const root = document.documentElement;
  const stored = () => {
    try {
      return localStorage.getItem("theme");
    } catch {
      return null;
    }
  };
  const system = () => (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const current = () => root.dataset.theme || system();

  const saved = stored();
  if (saved === "light" || saved === "dark") {
    root.dataset.theme = saved;
  }

  document.addEventListener("DOMContentLoaded", () => {
    const button = document.getElementById("theme");
    if (!button) {
      return;
    }
    const label = () => {
      const next = current() === "dark" ? "light" : "dark";
      button.setAttribute("aria-label", `Switch to ${next} mode`);
      button.title = `Switch to ${next} mode`;
    };
    label();
    button.addEventListener("click", () => {
      const next = current() === "dark" ? "light" : "dark";
      root.dataset.theme = next;
      try {
        localStorage.setItem("theme", next);
      } catch {
        // Private browsing: the choice lasts for this page only.
      }
      label();
    });
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", label);
  });
})();
