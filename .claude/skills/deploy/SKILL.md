---
name: deploy
description: Commit local changes, push, then pull/test/build on the Windows production machine via SSH
allowed-tools: Bash, Read, Grep, Glob
---

# Deploy to Windows

Commit changes on the local dev machine, push to remote, then pull, test, build, and verify on the Windows production machine (`ssh win`).

**Gate rule**: every step is a gate — if it fails, stop and report. Do not continue to subsequent steps.

## Steps

1. **Local compile check**
   - Run `cargo check -p larknotes-provider-cli -p larknotes-sync -p larknotes-core` (the crates that cross-compile; skip desktop which needs webkit2gtk).
   - If this fails, report the error and stop. Do NOT commit broken code.

2. **Local test**
   - Run `cargo test -p larknotes-sync -p larknotes-core -p larknotes-storage` (unit tests only, no network).
   - If any test fails, report and stop.

3. **Review & commit** (local)
   - Run `git status` and `git diff --stat` to review what will be committed.
   - If there are no changes, tell the user and stop.
   - Stage the changed files (prefer specific files over `git add -A`; never stage `.env`, credentials, or large binaries).
   - Create a commit. If the user provided `$ARGUMENTS`, use it as the commit message. Otherwise, draft a concise message from the diff.
   - Always append `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>` to the commit message body.

4. **Push** (local)
   - Run `git push`. If rejected, run `git pull --rebase` first, then push again.

5. **Pull on Windows** (`ssh win`)
   - `ssh win "cd repo\\larknotes && git pull"`
   - If pull fails (merge conflict, etc.), report the error and stop.

6. **Test on Windows** (`ssh win`)
   - `ssh win "cd repo\\larknotes && cargo test -p larknotes-sync -p larknotes-core -p larknotes-storage 2>&1"` (timeout 300s).
   - If any test fails, report the full output and stop. Do NOT build or start a broken binary.

7. **Build on Windows** (`ssh win`)
   - Stop the running app: `ssh win "powershell -Command \"Stop-Process -Name larknotes-desktop -ErrorAction SilentlyContinue; Start-Sleep 1\""`.
   - Build: `ssh win "cd repo\\larknotes && cargo build -p larknotes-desktop 2>&1"` (timeout 300s).
   - If the build fails, report the full error and stop.

8. **Start & smoke check** (`ssh win`)
   - Start the app: `ssh win "powershell -Command \"Start-Process -FilePath 'C:\\Users\\lixinrui\\repo\\larknotes\\target\\debug\\larknotes-desktop.exe' -WorkingDirectory 'C:\\Users\\lixinrui\\repo\\larknotes'\""`.
   - Wait 3 seconds, then verify the process is running: `ssh win "powershell -Command \"Get-Process larknotes-desktop -ErrorAction SilentlyContinue | Select-Object Id, StartTime\""`.
   - If the process is not found, the app crashed on startup — report and stop.

9. **Report**
   - Summarize: commit hash, changed files, test results, build time, app status.
