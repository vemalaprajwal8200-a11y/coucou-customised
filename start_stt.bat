@echo off
setlocal
cd /d "%~dp0"
if exist ".venv\Scripts\python.exe" (
  ".venv\Scripts\python.exe" stt_server.py
  exit /b
)
py -3 stt_server.py
if errorlevel 1 python stt_server.py
