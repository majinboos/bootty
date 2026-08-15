$ErrorActionPreference = "Stop"

$AppName = "Bootty"
$BinaryName = "bootty.exe"
$PackageName = "bootty"
$DaemonBinaryName = "bootty-daemon.exe"
$DistDir = if ($env:BOOTTY_DIST_DIR) { $env:BOOTTY_DIST_DIR } else { "dist" }
$TargetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$DaemonManifest = Join-Path $ScriptDir "bootty-daemon-targets.txt"
$DaemonOutputDir = $env:BOOTTY_DAEMON_OUTPUT_DIR
if (-not $DaemonOutputDir) {
    throw "Windows packaging requires BOOTTY_DAEMON_OUTPUT_DIR with all daemon targets staged"
}
$DaemonTargets = @(Get-Content -LiteralPath $DaemonManifest | ForEach-Object {
    $Target = $_.Trim()
    if ($Target -and -not $Target.StartsWith("#")) { $Target }
})
if ($DaemonTargets.Count -eq 0) {
    throw "Daemon target manifest is empty: $DaemonManifest"
}
foreach ($Target in $DaemonTargets) {
    $DaemonArtifact = Join-Path $DaemonOutputDir "bootty-daemon-$Target"
    if (-not (Test-Path -LiteralPath $DaemonArtifact -PathType Leaf) -or
        (Get-Item -LiteralPath $DaemonArtifact).Length -eq 0) {
        throw "Missing daemon artifact: $DaemonArtifact"
    }
}
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$BundleName = "$AppName-windows-$Arch"
$BundleRoot = Join-Path $DistDir $BundleName

$ProfileName = "release"
$CargoProfileArgs = @("--release")
$Fast = $false
$Linkage = "dynamic"

function Add-RustFlags($Flags) {
    $ExtraFlags = $Flags -join " "
    if ($env:RUSTFLAGS) {
        $env:RUSTFLAGS = "$($env:RUSTFLAGS) $ExtraFlags"
    } else {
        $env:RUSTFLAGS = $ExtraFlags
    }
}

foreach ($Arg in $args) {
    switch ($Arg) {
        "--fast" {
            $Fast = $true
        }
        "--static" {
            $Linkage = "static"
        }
        default {
            throw "Unknown package argument: $Arg"
        }
    }
}
if ($Fast) {
    $ProfileName = "fast-release"
    $CargoProfileArgs = @("--profile", "fast-release")
} elseif ($Linkage -eq "dynamic") {
    $ProfileName = "dynamic-release"
    $CargoProfileArgs = @("--profile", "dynamic-release")
}

$ProfileDir = Join-Path $TargetRoot $ProfileName

if ($Linkage -eq "dynamic") {
    Add-RustFlags @("-C", "prefer-dynamic")
}

if (Test-Path $DistDir) {
    Remove-Item -Recurse -Force $DistDir
}
New-Item -ItemType Directory -Force -Path $BundleRoot | Out-Null

cargo build @CargoProfileArgs -p $PackageName --bin bootty
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$BinaryPath = Join-Path $ProfileDir $BinaryName
if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Expected built binary at $BinaryPath"
}
Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $BundleRoot $BinaryName)
$HostDaemonPath = Join-Path $DaemonOutputDir "bootty-daemon-x86_64-pc-windows-msvc"
Copy-Item -LiteralPath $HostDaemonPath -Destination (Join-Path $BundleRoot $DaemonBinaryName)
$DaemonBundleRoot = Join-Path $BundleRoot "daemons"
New-Item -ItemType Directory -Force -Path $DaemonBundleRoot | Out-Null
foreach ($Target in $DaemonTargets) {
    Copy-Item -LiteralPath (Join-Path $DaemonOutputDir "bootty-daemon-$Target") -Destination $DaemonBundleRoot
}

if ($Linkage -eq "dynamic") {
    $DynamicLibraryDirs = @(
        (Join-Path $ProfileDir "deps"),
        ((rustc --print target-libdir) | Select-Object -First 1)
    )
    foreach ($LibraryDir in $DynamicLibraryDirs) {
        if ($LibraryDir -and (Test-Path -LiteralPath $LibraryDir)) {
            Get-ChildItem -LiteralPath $LibraryDir -File -Filter "*.dll" -ErrorAction SilentlyContinue |
                ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $BundleRoot -Force }
        }
    }
}

$RuntimeDlls = @("ghostty-vt.dll")
foreach ($DllName in $RuntimeDlls) {
    $Candidates = @(Get-ChildItem -LiteralPath $ProfileDir -Recurse -File -Filter $DllName -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending)
    if ($Candidates.Count -eq 0) {
        throw "Expected runtime DLL $DllName under $ProfileDir"
    }
    Copy-Item -LiteralPath $Candidates[0].FullName -Destination (Join-Path $BundleRoot $DllName)
}

$ArchivePath = Join-Path $DistDir "$BundleName.zip"
Compress-Archive -Path $BundleRoot -DestinationPath $ArchivePath -Force
Get-ChildItem -Recurse -File $DistDir | ForEach-Object { $_.FullName }
