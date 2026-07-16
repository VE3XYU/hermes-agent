@echo off
REM bin/hermes.cmd — Windows in-repo launcher stub for ejected mode.
REM
REM See docs/updater-world.md §2.5.1. Same logic as bin/hermes (POSIX).
REM
REM Cwd guard: if invoked from inside a git checkout, require --dev or --global.

setlocal enabledelayedexpansion

REM --- env hygiene ---
set PYTHONPATH=
set PYTHONHOME=
set UV_NO_CONFIG=1

set DIR=%~dp0
set REPO_ROOT=%DIR%..

REM --- cwd guard ---
set HAS_DEV=0
set HAS_GLOBAL=0
for %%a in (%*) do (
    if "%%~a"=="--dev" set HAS_DEV=1
    if "%%~a"=="--global" set HAS_GLOBAL=1
)

if "!HAS_DEV!"=="1" if "!HAS_GLOBAL!"=="1" (
    echo hermes: --dev and --global are contradictory — pick one. 1>&2
    exit /b 2
)

REM Walk up from cwd to find an enclosing hermes-agent checkout
set CWD=%CD%
set FOUND_CHECKOUT=
:find_checkout_loop
if exist "%CWD%\pyproject.toml" (
    findstr /c:"hermes-agent" "%CWD%\pyproject.toml" >nul 2>&1
    if !errorlevel! equ 0 (
        set FOUND_CHECKOUT=%CWD%
        goto :found_checkout
    )
)
for %%i in ("%CWD%\..") do set CWD=%%~fi
if not "%CWD%"=="%CWD:~0,3%" goto :find_checkout_loop

:found_checkout
if defined FOUND_CHECKOUT (
    if "!HAS_DEV!"=="0" if "!HAS_GLOBAL!"=="0" (
        echo hermes: you are inside a hermes-agent checkout (!FOUND_CHECKOUT!). 1>&2
        echo say which hermes you mean: 1>&2
        echo   hermes --dev       run THIS checkout's ./bin/hermes 1>&2
        echo   hermes --global    run the installed hermes (managed or PATH target) 1>&2
        exit /b 2
    )
)

REM Strip --dev and --global flags
set FORWARD_ARGS=
for %%a in (%*) do (
    if not "%%~a"=="--dev" if not "%%~a"=="--global" (
        set FORWARD_ARGS=!FORWARD_ARGS! %%~a
    )
)

REM --- try native launcher ---
if exist "%REPO_ROOT%\.hermes-launcher\hermes.exe" (
    "%REPO_ROOT%\.hermes-launcher\hermes.exe" %FORWARD_ARGS%
    exit /b %ERRORLEVEL%
)

REM --- fallback: exec venv python ---
if exist "%REPO_ROOT%\.venv\Scripts\python.exe" (
    set VENV_PYTHON=%REPO_ROOT%\.venv\Scripts\python.exe
) else if exist "%REPO_ROOT%\venv\Scripts\python.exe" (
    set VENV_PYTHON=%REPO_ROOT%\venv\Scripts\python.exe
) else (
    echo hermes: this tree's virtualenv is missing or broken. 1>&2
    echo   tree: %REPO_ROOT% 1>&2
    echo   fix:  hermes dev sync        (source checkout) 1>&2
    exit /b 3
)

"%VENV_PYTHON%" -m hermes_cli.main %FORWARD_ARGS%
exit /b %ERRORLEVEL%
