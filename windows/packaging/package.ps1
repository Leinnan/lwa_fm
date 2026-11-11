# Builds the native Windows shell's installer:
#   target\windows-package\installer\DirFleet-native-<version>-windows-x64.msi
#
# The release build links the C runtime statically and carries its own copy of
# the Windows App Runtime (windows-reactor-setup stages it beside the exe), so
# the installed app needs nothing else on the machine. After building, the MSI
# is validated, unpacked the way an administrator would, and the unpacked app
# is started to see that it opens its window.
#
# Needs WiX 5 (`dotnet tool install --global wix --version 5.0.2`).
[CmdletBinding()]
param(
    # Skip starting the unpacked app (it opens a window for a few seconds).
    [switch]$NoSmoke,
    # Package the release build already in target\windows-package, to work on
    # the installer without waiting for the build.
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$work = Join-Path $repo "target\windows-package"
$release = Join-Path $work "release"
$stage = Join-Path $work "stage"
$exeStage = Join-Path $work "stage-exe"
$installers = Join-Path $work "installer"
$exeName = "DirFleet.exe"

function Reset-Directory([string]$Path) {
    # Only ever folders of this script's own, under target\windows-package.
    if (-not $Path.StartsWith($work, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clear ${Path}: it is outside $work"
    }
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Recurse -Force }
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
}

function Invoke-Checked([string]$What, [scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

$metadata = cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $repo "Cargo.toml") | ConvertFrom-Json
$version = ($metadata.packages | Where-Object { $_.name -eq "dirfleet_windows" }).version
if (-not $version) { throw "Could not read the version of dirfleet_windows" }
Write-Host "==> DirFleet $version"

if (-not $SkipBuild) {
Write-Host "==> Building $exeName (x64 release, static C runtime)"
# What is left beside the exe from an earlier build could be from another
# revision of the runtime; clear it and have the build script stage it again.
if (Test-Path -LiteralPath $release) {
    Get-ChildItem -LiteralPath $release -Force |
        Where-Object { $_.Name -notin @(".fingerprint", "build", "deps", "examples", "incremental") } |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
}
(Get-Item -LiteralPath (Join-Path $repo "windows\build.rs")).LastWriteTime = Get-Date
$oldFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = "-C target-feature=+crt-static"
try {
    Push-Location $repo
    Invoke-Checked "The release build" {
        cargo build --locked --package dirfleet_windows --release --target-dir $work
    }
} finally {
    Pop-Location
    $env:RUSTFLAGS = $oldFlags
}
}
$exe = Join-Path $release $exeName
foreach ($required in @($exeName, "Microsoft.WindowsAppRuntime.dll", "Microsoft.ui.xaml.dll")) {
    if (-not (Test-Path -LiteralPath (Join-Path $release $required) -PathType Leaf)) {
        throw "The build is missing $required beside the exe"
    }
}

Write-Host "==> Staging"
Reset-Directory $stage
Reset-Directory $exeStage
Copy-Item -LiteralPath $exe -Destination $exeStage
$cargoOwn = @(".fingerprint", "build", "deps", "examples", "incremental")
Get-ChildItem -LiteralPath $release -Force | Where-Object {
    $_.Name -notin $cargoOwn -and
    $_.Name -ne $exeName -and
    $_.Name -ne ".cargo-lock" -and
    $_.Extension -notin @(".pdb", ".d", ".rlib", ".exe")
} | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $stage -Recurse }
$staged = @(Get-ChildItem -LiteralPath $stage -Recurse -File)
$megabytes = [math]::Round(($staged | Measure-Object Length -Sum).Sum / 1MB)
Write-Host "    $($staged.Count) runtime files, $megabytes MB"

Write-Host "==> Building the MSI"
New-Item -ItemType Directory -Path $installers -Force | Out-Null
$msi = Join-Path $installers "DirFleet-native-$version-windows-x64.msi"
Remove-Item -LiteralPath $msi -Force -ErrorAction SilentlyContinue
Invoke-Checked "wix build" {
    wix build (Join-Path $PSScriptRoot "dirfleet.wxs") -o $msi -arch x64 `
        -d "Version=$version" -d "StageDir=$stage" -d "ExeStageDir=$exeStage"
}
# WiX reads each file's language from its version resource. The runtime's
# files name dozens of languages there, or ones the installer does not know,
# and the File table holds one language id: validation fails on them. Nothing
# here is installed by language, so every file is marked neutral.
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($msi, 1)
try {
    $view = $database.OpenView('UPDATE `File` SET `Language` = 0')
    try { $view.Execute() | Out-Null } finally {
        $view.Close() | Out-Null
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    }
    $database.Commit()
} finally {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($database)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($installer)
}

Write-Host "==> Checking the MSI"
# ICE61: upgrades to the same version are meant (see MajorUpgrade in the source).
# ICE40: so is REINSTALLMODE.
Invoke-Checked "wix msi validate" { wix msi validate -sice ICE61 -sice ICE40 $msi }
$extract = Join-Path $work "extract"
Reset-Directory $extract
$unpack = Start-Process msiexec.exe -ArgumentList "/a `"$msi`" /qn TARGETDIR=`"$extract`"" -Wait -PassThru
if ($unpack.ExitCode -ne 0) { throw "Unpacking the MSI failed with exit code $($unpack.ExitCode)" }
$installed = Join-Path $extract "PFiles64\DirFleet"
foreach ($required in @($exeName, "Microsoft.WindowsAppRuntime.dll", "Microsoft.ui.xaml.dll")) {
    if (-not (Test-Path -LiteralPath (Join-Path $installed $required) -PathType Leaf)) {
        throw "The unpacked MSI is missing $required"
    }
}

if (-not $NoSmoke) {
    Write-Host "==> Starting the unpacked app"
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class DirFleetSmoke {
    delegate bool EnumProc(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr data);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int length);
    public static bool HasWindow(uint process) {
        bool found = false;
        EnumWindows((window, data) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == process && IsWindowVisible(window)) {
                var name = new StringBuilder(256); GetClassName(window, name, 256);
                if (name.ToString() == "WinUIDesktopWin32WindowClass") { found = true; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
    # Its own state file, so the check neither reads nor changes the user's.
    $state = Join-Path $work "smoke-state\state.json"
    Reset-Directory (Split-Path -Parent $state)
    # What the app says on its way out is the only account of a failed start.
    $said = Join-Path (Split-Path -Parent $state) "stderr.txt"
    $said_now = { (Get-Content -LiteralPath $said -Raw -ErrorAction SilentlyContinue) }
    $oldTrace = $env:DIRFLEET_TRACE
    $env:DIRFLEET_TRACE = "1"
    $app = Start-Process -FilePath (Join-Path $installed $exeName) -WorkingDirectory $installed `
        -ArgumentList @("--state-file", "`"$state`"") -PassThru -RedirectStandardError $said
    $env:DIRFLEET_TRACE = $oldTrace
    try {
        $deadline = (Get-Date).AddSeconds(30)
        while (-not [DirFleetSmoke]::HasWindow([uint32]$app.Id)) {
            if ($app.HasExited) { throw "The unpacked app ended while starting, with code $($app.ExitCode):`n$(& $said_now)" }
            if ((Get-Date) -gt $deadline) { throw "The unpacked app opened no window within 30 seconds:`n$(& $said_now)" }
            Start-Sleep -Milliseconds 250
        }
        if ($app.WaitForExit(3000)) { throw "The unpacked app ended after opening its window, with code $($app.ExitCode):`n$(& $said_now)" }
    } finally {
        if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force; $app.WaitForExit() }
    }
}

$hash = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash.ToLowerInvariant()
$size = [math]::Round((Get-Item -LiteralPath $msi).Length / 1MB)
Write-Host "==> $msi ($size MB, sha256 $hash)"
