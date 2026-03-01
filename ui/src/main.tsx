import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { RecordingBarView } from "./components/RecordingBarView";
import "./index.css";

const params = new URLSearchParams(window.location.search);
const view = params.get("view");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {view === "recording-bar" ? <RecordingBarView /> : <App />}
  </React.StrictMode>,
);
