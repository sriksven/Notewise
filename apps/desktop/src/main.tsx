import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { SetupGate } from "./onboarding/SetupGate";
import "./index.css";

// The gate sits at the root rather than inside `App`: whether there is a usable engine to
// render an app against is a question that precedes the app, and answering it here keeps the
// wizard out of a component already busy with recording, transcripts, and export.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <SetupGate>
      <App />
    </SetupGate>
  </React.StrictMode>,
);
