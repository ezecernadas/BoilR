import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { Settings } from "../settings";
import type { AutosyncStatus, GameSignal, SettingsUpdatePayload } from "../types";
import { Section, SettingRow, Toggle } from "./controls";

const SIGNAL_LABELS: Record<GameSignal, string> = {
  game_folder_process: "a program runs from a game folder",
  steam_running_app: "Steam reports a running game",
  full_screen: "Windows reports a full-screen app",
};

const NumberSetting = ({
  title,
  hint,
  value,
  min,
  onSave,
}: {
  title: string;
  hint: string;
  value: number;
  min: number;
  onSave: (value: number) => void;
}) => {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  const commit = () => {
    const parsed = Number.parseInt(draft, 10);
    if (Number.isNaN(parsed) || parsed < min) {
      setDraft(String(value));
      return;
    }
    if (parsed !== value) onSave(parsed);
  };
  return (
    <SettingRow title={title} hint={hint}>
      <input
        type="number"
        min={min}
        value={draft}
        aria-label={title}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
        }}
        className="field w-24 text-right"
      />
    </SettingRow>
  );
};

const yesNo = (value: boolean | null): string => (value === null ? "Unknown" : value ? "Yes" : "No");

/** Hidden where autosync can't run (everything but Windows). */
export const AutosyncSection = ({
  settings,
  onUpdate,
}: {
  settings: Settings;
  onUpdate: (patch: SettingsUpdatePayload) => void;
}) => {
  const [status, setStatus] = useState<AutosyncStatus | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      invoke<AutosyncStatus>("autosync_status")
        .then((next) => alive && setStatus(next))
        .catch(() => alive && setStatus(null));
    void load();
    const timer = window.setInterval(load, 5000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  if (!status?.available) return null;
  const autosync = settings.autosync ?? {};
  const signals = status.game_signals.map((signal) => SIGNAL_LABELS[signal]).join(", ");

  return (
    <Section title="Autosync" hint="Imports new and removed games by itself while BoilR waits in the tray. Steam is never closed while you play.">
      <SettingRow title="Import automatically" hint="Checks your launchers every few minutes.">
        <Toggle checked={Boolean(autosync.enabled)} label="Import automatically" onChange={(v) => onUpdate({ autosync: { enabled: v } })} />
      </SettingRow>
      <NumberSetting
        title="Check every"
        hint="Minutes between two checks."
        value={autosync.interval_minutes ?? 5}
        min={1}
        onSave={(v) => onUpdate({ autosync: { interval_minutes: v } })}
      />
      <NumberSetting
        title="Countdown"
        hint="Seconds the prompt waits before Steam restarts."
        value={autosync.countdown_seconds ?? 60}
        min={10}
        onSave={(v) => onUpdate({ autosync: { countdown_seconds: v } })}
      />
      <SettingRow title="Start with Windows" hint="Starts BoilR hidden in the tray when you sign in.">
        <Toggle
          checked={Boolean(autosync.start_with_windows)}
          label="Start with Windows"
          onChange={(v) => onUpdate({ autosync: { start_with_windows: v } })}
        />
      </SettingRow>
      <SettingRow title="Status">
        <span className="text-right text-peach">{status.summary || "Not checked yet"}</span>
      </SettingRow>
      <SettingRow title="Steam running">
        <span className="text-peach">{yesNo(status.steam_running)}</span>
      </SettingRow>
      <SettingRow title="Game running" hint={signals ? `Because ${signals}.` : undefined}>
        <span className="text-peach">{status.game_signals.length ? "Yes" : "No"}</span>
      </SettingRow>
      <SettingRow title="Last check" hint={status.last_check ?? undefined}>
        <span className="text-peach">
          {status.last_check_ms ? new Date(status.last_check_ms).toLocaleTimeString() : "Not yet"}
        </span>
      </SettingRow>
    </Section>
  );
};
