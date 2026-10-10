import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Viewer } from "./Viewer";
// The app icon is the one source of the mascot (docs/BRAND.md).
import laxIcon from "../src-tauri/icons/app-icon.svg";

// Must match CHANGED_EVENT in src-tauri/src/accessibility.rs.
const ACCESSIBILITY_CHANGED = "accessibility-changed";

/** `null` while loading; "unknown" if the check itself failed. */
type Access = boolean | null | "unknown";

function App() {
  const [granted, setGranted] = useState<Access>(null);
  // Without the permission, onboarding is shown, but what was captured
  // before can still be seen.
  const [showViewer, setShowViewer] = useState(false);

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
  let content;
  if (granted === "unknown") {
    content = <CheckFailed />;
  } else if (granted || showViewer) {
    content = <Viewer />;
  } else {
    content = <Onboarding onShowCaptures={() => setShowViewer(true)} />;
  }
  return (
    <>
      <FileVaultWarning />
      {content}
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

function Onboarding({ onShowCaptures }: { onShowCaptures: () => void }) {
  const [error, setError] = useState(false);

  const openSettings = () => {
    setError(false);
    invoke("open_accessibility_settings").catch(() => setError(true));
  };

  return (
    <main className="container onboarding">
      <header className="hero">
        <img src={laxIcon} alt="" />
        <div>
          <h1>Let Lax read your screen</h1>
          <p>A private memory of what you work on. Everything stays on this Mac.</p>
        </div>
      </header>

      <p>
        To remember what you do, Lax needs the macOS <strong>Accessibility</strong> permission.
        Here is exactly what that means.
      </p>

      <div className="cards">
        <section className="card reads">
          <h2>What Lax reads</h2>
          <p>
            Every few seconds: the app you're using, its window title, and the text shown in
            that window. For websites, only the site's name, never the full address.
          </p>
        </section>
        <section className="card never">
          <h2>What Lax never reads</h2>
          <ul>
            <li>Password fields</li>
            <li>Private and incognito browser windows</li>
            <li>Password managers</li>
            <li>Anything at all while capture is paused</li>
          </ul>
        </section>
        <section className="card">
          <h2>Where it goes</h2>
          <p>
            Nowhere. It stays on this Mac, in a folder only you can open. Nothing is sent
            anywhere unless you turn on a remote AI model yourself.
          </p>
        </section>
        <section className="card">
          <h2>What you decide</h2>
          <p>
            Banking and health are excluded from the start. You can exclude any app, website
            or window, pause from the menu bar, and forget anything.
          </p>
        </section>
      </div>

      <section className="steps">
        <h2>Turn it on</h2>
        <ol>
          <li>Open System Settings with the button below.</li>
          <li>
            In <strong>Privacy &amp; Security → Accessibility</strong>, turn on Lax.
          </li>
          <li>Come back here. This window changes as soon as access is on.</li>
        </ol>
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
      </section>

      <p className="muted">
        macOS uses the same permission for reading the screen and for controlling other apps.
        Lax only reads, and its source code is open, so anyone can check. You can turn access
        off at any time.{" "}
        <button type="button" className="link" onClick={onShowCaptures}>
          See what Lax has already captured
        </button>
      </p>
    </main>
  );
}

export default App;
