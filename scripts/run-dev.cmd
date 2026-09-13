@echo off
rem ============================================================
rem  workbuddy-switch dev launcher (double-click to run)
rem  debug exe loads frontend from devUrl http://localhost:1420,
rem  so vite MUST be listening before the app starts.
rem  v2: poll port up to 90s (vite cold start can take ~60s now),
rem      log vite output to target\vite-dev.log for diagnosis.
rem ============================================================
setlocal enabledelayedexpansion
cd /d "%~dp0.."

echo === workbuddy-switch dev launcher ===

rem 1) ensure vite is listening on 1420
netstat -ano | findstr ":1420" | findstr "LISTENING" >nul 2>&1
if errorlevel 1 (
  echo [1/2] starting vite dev server ...
  if not exist target mkdir target
  start "vite-wb-switch" /min cmd /c "node node_modules\vite\bin\vite.js > target\vite-dev.log 2>&1"
  set /a tries=0
  :waitloop
  timeout /t 3 /nobreak >nul
  netstat -ano | findstr ":1420" | findstr "LISTENING" >nul 2>&1
  if errorlevel 1 (
    set /a tries+=1
    if !tries! geq 30 (
      echo       ERROR: vite not ready after 90s. See target\vite-dev.log
      pause
      exit /b 1
    )
    echo       waiting for vite ... !tries!x3s
    goto waitloop
  )
)
echo [1/2] vite is listening on 1420

rem 2) launch the desktop app
echo [2/2] launching target\debug\wb-switch-rust.exe ...
start "" "%~dp0..\target\debug\wb-switch-rust.exe"

echo done. close the vite window to stop the dev server.
endlocal
