@echo off
rem ============================================================
rem  主力模型每日用量 vs 峰值基线 一键检查（双击运行）
rem  运行 scripts/analysis/model_daily_limit_check.py
rem  今日用量超峰值会自动更新 ~/.wb-switch/model_daily_peaks.json
rem ============================================================
setlocal
cd /d "%~dp0"

set "PY=C:\Users\WH\.workbuddy\binaries\python\versions\3.13.12\python.exe"
if not exist "%PY%" set "PY=python"

echo === 主力模型每日用量检查 ===
"%PY%" "analysis\model_daily_limit_check.py"
echo.
pause
endlocal
