import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { SetupGate } from "./onboarding/SetupGate";
import { parseRoute } from "./lib/router";
import { AssistantOverlay } from "./views/AssistantOverlay";
import "./index.css";

// The assistant overlay is a second window pointed at the same frontend, so the choice is made
// here rather than inside `App`. Two reasons: a floating panel with a sidebar in it is not a
// floating panel, and the setup wizard must not appear in it — somebody who pressed the assistant
// hotkey asked a question, not to configure a model.
const overlay = parseRoute(window.location.hash).name === "overlay";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {overlay ? (
      <AssistantOverlay />
    ) : (
      // The gate sits at the root rather than inside `App`: whether there is a usable engine to
      // render an app against is a question that precedes the app, and answering it here keeps the
      // wizard out of a component already busy with recording, transcripts, and export.
      <SetupGate>
        <App />
      </SetupGate>
    )}
  </React.StrictMode>,
);
