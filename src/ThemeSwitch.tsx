import { useState } from "react";
import { applyTheme, storedTheme, type Theme } from "./theme";

const CHOICES: { theme: Theme; label: string; icon: React.ReactNode }[] = [
  {
    theme: "system",
    label: "As macOS",
    icon: (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <rect x="3" y="4" width="18" height="12" rx="2" />
        <path d="M8 20h8M12 16v4" />
      </svg>
    ),
  },
  {
    theme: "light",
    label: "Light",
    icon: (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="12" cy="12" r="4.5" />
        <path d="M12 1.5v3M12 19.5v3M1.5 12h3M19.5 12h3M4.6 4.6l2.1 2.1M17.3 17.3l2.1 2.1M4.6 19.4l2.1-2.1M17.3 6.7l2.1-2.1" />
      </svg>
    ),
  },
  {
    theme: "dark",
    label: "Dark",
    icon: (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11Z" />
      </svg>
    ),
  },
];

/** Light, dark or as macOS, for Lax's window. */
export function ThemeSwitch() {
  const [theme, setTheme] = useState<Theme>(storedTheme);
  return (
    <div className="theme-switch" role="group" aria-label="Appearance">
      {CHOICES.map((choice) => (
        <button
          key={choice.theme}
          type="button"
          title={choice.label}
          aria-label={choice.label}
          aria-pressed={theme === choice.theme}
          onClick={() => {
            setTheme(choice.theme);
            applyTheme(choice.theme);
          }}
        >
          {choice.icon}
        </button>
      ))}
    </div>
  );
}
