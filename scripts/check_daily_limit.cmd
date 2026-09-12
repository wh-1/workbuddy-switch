@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
set "PY=C:\Users\WH\.workbuddy\binaries\python\versions\3.13.12\python.exe"
if not exist "%PY%" set "PY=python"
"%PY%" "analysis\model_daily_limit_check.py"
echo.
pause
endlocal
