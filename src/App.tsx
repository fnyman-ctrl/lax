import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Must match CHANGED_EVENT in src-tauri/src/accessibility.rs.
const ACCESSIBILITY_CHANGED = "accessibility-changed";

/** `null` while loading; "unknown" if the check itself failed. */
type Access = boolean | null | "unknown";

function App() {
  const [granted, setGranted] = useState<Access>(null);

  const refresh = useCallback(() => {
    invoke<boolean>("accessibility_status").then(setGranted, () => setGranted("unknown"));
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = listen<boolean>(ACCESSIBILITY_CHANGED, (event) => setGranted(event.payload));
    window.addEventListener("focus", refresh);
    return () => {
      unlisten.then((stop) => stop());
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  if (granted === null) {
    return null;
  }
  return (
    <>
      <FileVaultWarning />
      {granted === "unknown" ? <CheckFailed /> : granted ? <Ready /> : <Onboarding />}
    </>
  );
}

/**
 * Warns when FileVault is off (GUARDRAILS §6). If the status can't be read,
 * says so mildly instead of staying silent. Checked again whenever the window
 * gets focus, so the warning goes away once FileVault is turned on.
 */
function FileVaultWarning() {
  // undefined while loading, null if the status couldn't be read.
  const [enabled, setEnabled] = useState<boolean | null | undefined>(undefined);

  useEffect(() => {
    const check = () => {
      invoke<boolean | null>("filevault_enabled").then(setEnabled, () => setEnabled(null));
    };
    check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, []);

  if (enabled === undefined || enabled === true) {
    return null;
  }
  if (enabled === null) {
    return (
      <aside className="notice">
        Lax couldn't check whether FileVault is on. Lax relies on FileVault to encrypt what it
        stores. You can check in System Settings → Privacy &amp; Security → FileVault.
      </aside>
    );
  }
  return (
    <aside className="warning" role="alert">
      <strong>FileVault is off.</strong> Lax keeps what it captures on this Mac's disk. Without
      FileVault, anyone who gets hold of the disk can read it. Turn FileVault on in System
      Settings → Privacy &amp; Security → FileVault.
    </aside>
  );
}

function CheckFailed() {
  return (
    <main className="container">
      <h1>Lax</h1>
      <p className="error" role="alert">
        Lax couldn't check whether it has Accessibility access.
      </p>
      <p className="muted">
        You can see and change it in System Settings → Privacy &amp; Security → Accessibility.
        Lax checks again when you come back to this window.
      </p>
    </main>
  );
}

function Onboarding() {
  const [error, setError] = useState(false);

  const openSettings = () => {
    setError(false);
    invoke("open_accessibility_settings").catch(() => setError(true));
  };

  return (
    <main className="container">
      <h1>Let Lax read your screen</h1>
      <p>
        Lax builds a private memory of what you work on. To do that it needs the macOS{" "}
        <strong>Accessibility</strong> permission.
      </p>

      <h2>What Lax reads</h2>
      <p>
        Every few seconds: the name of the app you're using, its window title, and the text
        shown in that window.
      </p>

      <h2>What Lax never reads</h2>
      <ul>
        <li>Password fields</li>
        <li>Private and incognito browser windows</li>
        <li>Password managers</li>
        <li>Anything at all while capture is paused</li>
      </ul>
      <p>
        Banking and health apps are excluded by default, and you can exclude any other app or
        website.
      </p>

      <h2>Where it goes</h2>
      <p>
        Everything stays on this Mac. Nothing is sent anywhere unless you turn on a remote AI
        model yourself.
      </p>

      <h2>About the permission</h2>
      <p>
        macOS uses the same permission for reading what's on screen and for controlling other
        apps. Lax only reads. Its source code is open, so anyone can check that.
      </p>

      <p className="actions">
        <button type="button" onClick={openSettings}>
          Open System Settings
        </button>
      </p>
      {error && (
        <p className="error" role="alert">
          System Settings didn't open. Open it yourself and go to Privacy &amp; Security →
          Accessibility.
        </p>
      )}
      <p className="muted">
        In Privacy &amp; Security → Accessibility, turn on Lax. This window updates as soon as
        access is granted. You can turn it off there at any time.
      </p>
    </main>
  );
}

function Ready() {
  return (
    <main className="container">
      <h1>Lax</h1>
      <p>A private, fully local memory for your Mac.</p>
      <p>✓ Accessibility access is on.</p>
      <p className="muted">
        Nothing is captured yet. You can turn access off at any time in System Settings →
        Privacy &amp; Security → Accessibility.
      </p>
    </main>
  );
}

export default App;
