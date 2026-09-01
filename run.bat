@echo off
setlocal
pushd "%~dp0"
set "PATH=%CD%;%PATH%"
set "APP=%CD%\dlss5-tauri.exe"
if exist "%CD%\src-tauri\target\release\dlss5-tauri.exe" set "APP=%CD%\src-tauri\target\release\dlss5-tauri.exe"
if not exist "%APP%" (
  if not exist "%CD%\src-tauri\Cargo.toml" (
    echo Release executable is missing from this folder.
    pause
    popd
    exit /b 1
  )
  where cargo >nul 2>&1
  if errorlevel 1 (
    echo Build missing and Cargo was not found in PATH.
    echo Install Rust, then double-click run.bat again.
    pause
    popd
    exit /b 1
  )
  echo Build missing. Building release executable...
  cargo build --release --manifest-path src-tauri\Cargo.toml
  if errorlevel 1 (
    echo Build failed. See the output above for details.
    pause
    popd
    exit /b 1
  )
  copy "%CD%\src-tauri\target\release\dlss5-tauri.exe" "%APP%"
)
if not exist "%APP%" (
  echo Build completed but the executable was not found:
  echo %APP%
  pause
  popd
  exit /b 1
)
start "" "%APP%"
popd
