import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { AutosyncPrompt } from "./components/AutosyncPrompt";
import "./styles.css";

const inTauri = "__TAURI_INTERNALS__" in window;
if (!inTauri) {
  await import("./devMock");
}

// Inside Tauri the window label picks the view; in the browser, ?view=autosync-prompt does.
const PROMPT_LABEL = "autosync-prompt";
const view = inTauri ? getCurrentWindow().label : new URLSearchParams(window.location.search).get("view");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{view === PROMPT_LABEL ? <AutosyncPrompt /> : <App />}</React.StrictMode>
);
