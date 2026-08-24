import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { OverlayApp } from "./OverlayApp";
import "./styles.css";

const overlay = new URLSearchParams(window.location.search).get("window") === "overlay";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{overlay ? <OverlayApp /> : <App />}</React.StrictMode>,
);
