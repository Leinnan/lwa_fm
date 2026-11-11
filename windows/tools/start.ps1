# Starts a DirFleet.exe to drive (drive.ps1), with a state file of its own under target\drive so
# the real one is neither read nor changed, and remembers its process id there. Stops the one it
# started before; any other DirFleet is left alone.
#
#   cargo build -p dirfleet_windows --target-dir target\drive\build
#   windows\tools\start.ps1 -Folder C:\some\scratch\folder
param(
    # The folder to open; without it the app starts at home.
    [string]$Folder,
    # The build to start. One built into a folder of its own can be rebuilt while the app a person
    # runs from target\debug stays open.
    [string]$Exe,
    [int]$Wait = 6
)
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$driveDir = Join-Path $repo 'target\drive'
if (-not $Exe) { $Exe = Join-Path $driveDir 'build\debug\DirFleet.exe' }
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "No build at $Exe (cargo build -p dirfleet_windows --target-dir target\drive\build)"
}
New-Item -ItemType Directory -Force $driveDir | Out-Null
$pidFile = Join-Path $driveDir 'pid'
$old = Get-Content $pidFile -ErrorAction SilentlyContinue
if ($old) {
    Get-Process -Id ([int]$old) -ErrorAction SilentlyContinue |
        Where-Object { $_.ProcessName -eq 'DirFleet' } | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}
$state = Join-Path $driveDir 'state'
if (Test-Path -LiteralPath $state) { Remove-Item -LiteralPath $state -Recurse -Force }
New-Item -ItemType Directory -Force $state | Out-Null
$env:DIRFLEET_TRACE = '1'
$arguments = @('--state-file', "`"$(Join-Path $state 'state.json')`"")
if ($Folder) { $arguments += @('--folder', "`"$Folder`"") }
$process = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru `
    -RedirectStandardError (Join-Path $driveDir 'stderr.txt')
Set-Content $pidFile $process.Id
Start-Sleep -Seconds $Wait
$process.Refresh()
if ($process.HasExited) { "ended with code $($process.ExitCode)" } else { "started: pid $($process.Id)" }
