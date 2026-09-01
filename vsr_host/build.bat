@echo off
rem Builds rtx_vsr_host.dll (to the repo root) and the self-test executable.
rem Requires the NVIDIA RTX Video SDK unpacked at ..\tools\rtx_video_sdk
setlocal
call "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 (
  echo Visual Studio build tools not found.
  exit /b 1
)
set SDK=..\tools\rtx_video_sdk
set LIBS=%SDK%\lib\Windows\x64\nvsdk_ngx_s.lib d3d12.lib dxgi.lib advapi32.lib user32.lib
cl /nologo /O2 /EHsc /std:c++17 /LD /I%SDK%\include rtx_vsr_host.cpp /link %LIBS% /OUT:..\rtx_vsr_host.dll
if errorlevel 1 exit /b 1
del ..\rtx_vsr_host.lib ..\rtx_vsr_host.exp >nul 2>&1
cl /nologo /O2 /EHsc /std:c++17 /DVSR_HOST_TEST /I%SDK%\include rtx_vsr_host.cpp /link %LIBS% /OUT:vsr_host_test.exe /SUBSYSTEM:CONSOLE
if errorlevel 1 exit /b 1
echo Build OK: ..\rtx_vsr_host.dll + vsr_host_test.exe
