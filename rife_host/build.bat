@echo off
rem Builds rife_host.dll (RIFE v4.6 frame interpolation host) into the repo root.
rem Requires:
rem   - Visual Studio 2022/2026 with CMake (adjust the VS path below if needed)
rem   - era ncnn build tree at ..\tools\rife_upstream\build  (built via tools\rife_upstream src)
rem   - Vulkan headers at ..\tools\vulkan\include and import lib ..\tools\vulkan\lib\vulkan-1.lib
setlocal
set "CMAKE=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
if not exist "%CMAKE%" (
  echo CMake not found. Install VS CMake tools or edit this script.
  exit /b 1
)
"%CMAKE%" -S . -B build -G "Visual Studio 18 2026" -A x64 || exit /b 1
"%CMAKE%" --build build --config Release --target rife_host || exit /b 1
copy /y build\Release\rife_host.dll ..\rife_host.dll >nul
echo Build OK: ..\rife_host.dll
