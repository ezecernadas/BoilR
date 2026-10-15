import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { PromptInfo } from "../types";
import { errorMessage } from "../lib/format";

/** How long a load failure stays readable before the window closes itself. */
const CLOSE_AFTER_ERROR_MS = 5000;

/** Without a prompt to show, the undecorated window has nothing to offer: close it. */
const closeWindow = () => {
  if (!("__TAURI_INTERNALS__" in window)) return; // browser/devMock: nothing to close
  invoke("autosync_close_prompt").catch(() => undefined);
};

type Answer = "autosync_restart_now" | "autosync_not_now";

/** The small always-on-top window autosync opens before restarting Steam. */
export const AutosyncPrompt = () => {
  const [info, setInfo] = useState<PromptInfo | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const load = () =>
      invoke<PromptInfo | null>("autosync_prompt_info")
        .then((loaded) => {
          setInfo(loaded);
          setError(null);
        })
        .catch((err) => setError(errorMessage(err)));
    void load();
    const timer = window.setInterval(() => {
      setNow(Date.now());
      void load();
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  const failed = error !== null && info === null;
  useEffect(() => {
    if (!failed) return;
    const timer = window.setTimeout(closeWindow, CLOSE_AFTER_ERROR_MS);
    return () => window.clearTimeout(timer);
  }, [failed]);

  const answer = async (command: Answer) => {
    setBusy(true);
    try {
      await invoke(command);
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };

  if (!info) {
    return (
      <div className="h-screen border-3 border-harbour bg-deep p-4 text-foam">
        {error ? <p className="text-sm text-ember">{error}</p> : null}
      </div>
    );
  }
  const seconds = Math.max(0, Math.ceil((info.deadline_ms - now) / 1000));

  return (
    <div className="flex h-screen flex-col justify-between border-3 border-harbour bg-deep p-4 text-foam">
      <div>
        <p className="font-bold">{info.summary}.</p>
        <p className="text-mauve">Steam restarts in {seconds} s</p>
        {error ? <p className="mt-1 text-sm text-ember">{error}</p> : null}
      </div>
      <div className="flex gap-3">
        <button type="button" className="btn-primary" disabled={busy} onClick={() => void answer("autosync_restart_now")}>
          Restart now
        </button>
        <button type="button" className="btn-quiet" disabled={busy} onClick={() => void answer("autosync_not_now")}>
          Not now
        </button>
      </div>
    </div>
  );
};
