@echo off
rem One-click build or package script for the harness workspace on Windows.
rem Plain cargo build fails with: error calling dlltool dlltool.exe program not found.
rem Fix: build with the MSVC target. VS Build Tools provides link.exe and the SDK.
rem Usage:
rem   scripts\build.bat                            (cargo build)
rem   scripts\build.bat check -p harness-bin
rem   scripts\build.bat package                    (release build + dist)
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "CARGO_BUILD_TARGET=x86_64-pc-windows-msvc"
set "RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc"
cd /d "%~dp0.."
set "VSINSTALL=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools"
if not exist "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" (
  set "VSINSTALL=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
)
if not exist "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" (
  set "VSINSTALL=C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools"
)
if not exist "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" (
  echo [build] ERROR: vcvars64.bat not found under VS Build Tools.
  exit /b 1
)
call "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
echo [build] target=%CARGO_BUILD_TARGET%
echo [build] VS=%VSINSTALL%

if "%~1"=="package" (
  rem Ship all optional M2-M7 capabilities in the product binary.
  cargo build --release --all-features > build.log 2>&1
  if errorlevel 1 (
    echo [build] FAILED - see build.log
    exit /b 1
  )
  if not exist dist mkdir dist
  copy /Y "target\%CARGO_BUILD_TARGET%\release\aidops-desktop.exe" dist\ >nul
  if errorlevel 1 (
    echo [build] FAILED: dist\aidops-desktop.exe is locked or not writable. Close the running app and retry.
    exit /b 1
  )
  if exist config\default.toml copy /Y config\default.toml dist\ >nul
  if errorlevel 1 (
    echo [build] FAILED: could not copy config\default.toml to dist.
    exit /b 1
  )
  if exist extensions\EXTENSION-COOKBOOK.md copy /Y extensions\EXTENSION-COOKBOOK.md dist\ >nul
  echo [build] packaged -^> dist\aidops-desktop.exe
  echo [build] full log: build.log
  exit /b 0
)

if "%~1"=="" (
  cargo build > build.log 2>&1
) else (
  cargo %* > build.log 2>&1
)
if errorlevel 1 (
  echo [build] FAILED - see build.log
  exit /b 1
)
echo [build] OK - full log: build.log
