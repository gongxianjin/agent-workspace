@echo off
setlocal
rem Use installed MinGW SQLite; do not compile the SQLite amalgamation.
if not defined MINGW_HOME set "MINGW_HOME=C:\msys64\mingw64"
set "RUST_BIN=%USERPROFILE%\.rustup\toolchains\1.99.1-x86_64-pc-windows-gnu\bin"
if not exist "%MINGW_HOME%\lib\libsqlite3.dll.a" goto missing
if not exist "%MINGW_HOME%\include\sqlite3.h" goto missing
if not exist "%MINGW_HOME%\bin\libsqlite3-0.dll" goto missing
if not exist "%RUST_BIN%\cargo.exe" goto missing
set "PATH=%MINGW_HOME%\bin;%RUST_BIN%;%PATH%"
set "RUSTC=%RUST_BIN%\rustc.exe"
set "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=%MINGW_HOME%\bin\gcc.exe"
set "CC=%MINGW_HOME%\bin\gcc.exe"
set "AR=%MINGW_HOME%\bin\ar.exe"
set "SQLITE3_INCLUDE_DIR=%MINGW_HOME%\include"
set "SQLITE3_LIB_DIR=%MINGW_HOME%\lib"
set "SQLITE3_STATIC=0"
rem Force the documented build_linked fallback, not pkg-config's implicit mode.
set "SQLITE3_NO_PKG_CONFIG=1"
"%RUST_BIN%\cargo.exe" %*
exit /b %ERRORLEVEL%
:missing
>&2 echo Installed Rust 1.99.1 or MinGW SQLite headers/import library/runtime missing.
exit /b 2
