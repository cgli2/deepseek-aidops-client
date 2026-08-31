@echo off
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "CARGO_BUILD_TARGET=x86_64-pc-windows-msvc"
set "RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc"
cd /d "%~dp0.."
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
cargo %* > build.log 2>&1
echo CARGO_EXIT=%ERRORLEVEL%
