import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { PromptInfo } from "../types";
import { errorMessage } from "../lib/format";

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
        .then(setInfo)
        .catch((err) => setError(errorMessage(err)));
    void load();
    const timer = window.setInterval(() => {
      setNow(Date.now());
      void load();
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  const answer = async (command: Answer) => {
    setBusy(true);
    try {
      await invoke(command);
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };

  if (!info) return <div className="h-screen bg-deep" />;
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
