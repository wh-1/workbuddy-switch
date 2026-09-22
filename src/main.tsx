import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/bricolage-grotesque";
import App from "./App";
import "./index.css";
import { installNotificationArchive } from "./lib/notify";
import { applyTheme, getThemePreference, watchSystemTheme } from "./lib/theme";

// 全局错误显性化：白屏时把错误直接画在窗口里（排障用，正式版可去）。
function showFatal(message: string, detail: string) {
  try {
    document.title = `启动出错：${message.slice(0, 60)}`;
    const box = document.createElement("pre");
    box.style.cssText =
      "position:fixed;inset:auto 12px 12px 12px;z-index:99999;margin:0;padding:12px;" +
      "background:#1a1a1a;color:#ff6b6b;border:1px solid #ff6b6b;border-radius:8px;" +
      "font:12px/1.5 Consolas,monospace;white-space:pre-wrap;max-height:60vh;overflow:auto;";
    box.textContent = `[${new Date().toLocaleTimeString()}] ${message}\n\n${detail}`;
    document.body.appendChild(box);
    localStorage.setItem("wbswitch_last_fatal", `[${new Date().toISOString()}] ${message}\n${detail}`);
  } catch {
    /* 尽力而为 */
  }
}
window.addEventListener("error", (e) =>
  showFatal(e.message || "script error", `${e.filename}:${e.lineno}:${e.colno}\n${e.error?.stack ?? ""}`),
);
window.addEventListener("unhandledrejection", (e) =>
  showFatal("unhandled promise rejection", String((e.reason && (e.reason.stack || e.reason.message)) || e.reason)),
);

applyTheme(getThemePreference());
const stopWatchingSystemTheme = watchSystemTheme();
if (import.meta.hot) import.meta.hot.dispose(stopWatchingSystemTheme);

// 提示存档必须在首个 toast 之前装好（包装 sonner 的四类提示）。
installNotificationArchive();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
