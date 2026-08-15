$ErrorActionPreference = "Stop"

$PackageName = "bootty"
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
            throw "Unknown build argument: $Arg"
        }
    }
}

if ($Fast) {
    $CargoProfileArgs = @("--profile", "fast-release")
} elseif ($Linkage -eq "dynamic") {
    $CargoProfileArgs = @("--profile", "dynamic-release")
}

if ($Linkage -eq "dynamic") {
    Add-RustFlags @("-C", "prefer-dynamic")
}

cargo build @CargoProfileArgs -p $PackageName --bin bootty
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}
