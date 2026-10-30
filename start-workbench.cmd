@echo off
setlocal
set "PYTHON=%~dp0vendor\python\python.exe"
set "PYTHONW=%~dp0vendor\python\pythonw.exe"
if exist "%~dp0portable.json" goto check
if exist "%~dp0vendor\" goto check
rem Source-tree diagnostics only. Portable packages never fall back to global tools.
set "PYTHON=%LOCALAPPDATA%\Python\pythoncore-3.14-64\python.exe"
set "PYTHONW=%LOCALAPPDATA%\Python\pythoncore-3.14-64\pythonw.exe"
if exist "%PYTHON%" goto check
for /f "delims=" %%P in ('where python.exe 2^>nul') do if not defined FOUND_PYTHON set "FOUND_PYTHON=%%P"
if not defined FOUND_PYTHON goto missing
set "PYTHON=%FOUND_PYTHON%"
for %%P in ("%FOUND_PYTHON%") do set "PYTHONW=%%~dpPpythonw.exe"
:check
if not exist "%PYTHON%" goto missing
if not exist "%PYTHONW%" goto missing
"%PYTHON%" -I -X utf8 -c "import sqlite3,ctypes,_winapi,http.server,ssl" >nul 2>nul
if errorlevel 1 goto missing
if "%~1"=="" goto background
if /i "%~1"=="--background" goto background
"%PYTHON%" -I -X utf8 "%~dp0p1\bootstrap.py" %*
exit /b %errorlevel%
:background
start "" "%PYTHONW%" -I -X utf8 "%~dp0p1\bootstrap.py" %*
if errorlevel 1 goto missing
exit /b 0
:missing
echo Workbench runtime missing or unable to load. Extract the complete Windows x64 package to a writable local folder.
echo No software will be installed and no existing instance will be stopped.
if "%~1"=="" pause
exit /b 1
