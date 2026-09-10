@echo off
rem ============================================================
rem  workbuddy-switch dev launcher (double-click to run)
rem  debug exe loads frontend from devUrl http://localhost:1420,
rem  so vite MUST be running before the app starts.
rem ============================================================
setlocal
cd /d "%~dp0.."

echo === workbuddy-switch dev launcher ===

rem 1) ensure vite is listening on 1420
netstat -ano | findstr ":1420" | findstr "LISTENING" >nul 2>&1
if errorlevel 1 (
  echo [1/2] starting vite dev server ...
  start "vite-wb-switch" /min cmd /c "npm run dev"
  echo       waiting 10s for vite to be ready ...
  timeout /t 10 /nobreak >nul
) else (
  echo [1/2] vite already running on port 1420
)

rem 2) launch the desktop app
echo [2/2] launching target\debug\wb-switch-rust.exe ...
start "" "%~dp0..\target\debug\wb-switch-rust.exe"

echo done. close the vite window to stop the dev server.
endlocal
