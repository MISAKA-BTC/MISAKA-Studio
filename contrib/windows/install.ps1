# MISAKA Studio — build and run from source on Windows, in one command.
#
# For the person who wants to BUILD. Anyone who only wants to RUN should take the installer from
# the Releases page instead: it carries a compiled runtime and needs none of this.
#
# What the README asks a Windows reader to do is install three toolchains by hand, remember to
# reopen the shell so PATH applies, and then run bash syntax that PowerShell 5.1 rejects. Every one
# of those is a place to stop. This does the same steps in an order that cannot be got wrong, and
# says which one failed when one does.
#
#   irm https://raw.githubusercontent.com/MISAKA-BTC/MISAKA-Studio/main/contrib/windows/install.ps1 | iex
#
# Or, having cloned already:  .\contrib\windows\install.ps1

[CmdletBinding()]
param(
    # Where to clone. Ignored when run from inside a checkout.
    [string] $Path = "$HOME\MISAKA-Studio",
    # Build only; do not start the app afterwards.
    [switch] $NoRun,
    # Skip the toolchain step for a machine that already has Rust, Node and Git.
    [switch] $SkipPrerequisites
)

$ErrorActionPreference = 'Stop'

function Step([string] $Message) { Write-Host "`n== $Message" -ForegroundColor Cyan }
function Note([string] $Message) { Write-Host "   $Message" -ForegroundColor DarkGray }

function Test-Command([string] $Name) {
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

# winget installs put their binaries on the MACHINE PATH, which this process does not see because
# it inherited its environment at launch. Re-reading both scopes is what makes install-then-use
# work in one session instead of "close the window and start again", which is the instruction
# people miss.
function Update-PathFromRegistry {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user    = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = ($machine, $user | Where-Object { $_ }) -join ';'
    # rustup writes here and adds it to the user PATH; a fresh install may not be in either scope
    # yet on the very first run.
    $cargoBin = "$HOME\.cargo\bin"
    if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) { $env:Path = "$cargoBin;$env:Path" }
}

function Install-Prerequisite([string] $Command, [string] $WingetId, [string] $Human) {
    if (Test-Command $Command) { Note "$Human is already here"; return }
    if (-not (Test-Command 'winget')) {
        throw "$Human is missing and winget is not available to install it. Install $Human by hand, then run this again."
    }
    Step "Installing $Human"
    # --silent so a chain of three installers does not need three clicks; --accept-* because a
    # non-interactive install cannot answer a licence prompt.
    winget install --id $WingetId --silent --accept-package-agreements --accept-source-agreements
    Update-PathFromRegistry
    if (-not (Test-Command $Command)) {
        throw "$Human was installed but '$Command' is still not on PATH. Close this window, open a new PowerShell, and run this script again — the install succeeded and only the environment is stale."
    }
}

Write-Host "MISAKA Studio — Windows setup" -ForegroundColor White

if (-not $SkipPrerequisites) {
    Update-PathFromRegistry
    Install-Prerequisite -Command 'git'   -WingetId 'Git.Git'            -Human 'Git'
    Install-Prerequisite -Command 'node'  -WingetId 'OpenJS.NodeJS.LTS'  -Human 'Node.js'
    Install-Prerequisite -Command 'cargo' -WingetId 'Rustlang.Rustup'    -Human 'Rust'
}

foreach ($tool in 'git', 'node', 'cargo') {
    if (-not (Test-Command $tool)) { throw "'$tool' is not available. Install it and run this again." }
}

# Rust on Windows links with MSVC, and rustup does not bring the linker. Missing it fails deep in
# the build with a `link.exe not found` that reads like a project problem and is not one, so it is
# checked here where the message can say what to do.
$linker = Get-Command 'link.exe' -ErrorAction SilentlyContinue
if (-not $linker) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        Note "The MSVC build tools were not found. Installing them (this is a large download)."
        if (Test-Command 'winget') {
            winget install --id Microsoft.VisualStudio.2022.BuildTools --silent `
                --accept-package-agreements --accept-source-agreements `
                --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
        } else {
            throw "Rust needs the MSVC build tools and winget is unavailable. Install 'Desktop development with C++' from https://visualstudio.microsoft.com/downloads/ and run this again."
        }
    }
}

# Run from inside a checkout if there is one; otherwise clone.
$repoRoot = if (Test-Path (Join-Path $PSScriptRoot '..\..\Cargo.toml')) {
    (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
} else {
    if (-not (Test-Path $Path)) {
        Step "Cloning into $Path"
        git clone https://github.com/MISAKA-BTC/MISAKA-Studio.git $Path
    } else {
        Step "Updating $Path"
        git -C $Path pull --ff-only
    }
    $Path
}
Set-Location $repoRoot
Note "Working in $repoRoot"

Step 'Building the runtime (5-15 minutes the first time)'
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed. The output above says why; the last error line is the one that matters." }

Step 'Building the UI'
npm --prefix ui install
if ($LASTEXITCODE -ne 0) { throw 'npm install failed.' }
npm --prefix ui run build
if ($LASTEXITCODE -ne 0) { throw 'The UI build failed.' }

$exe = Join-Path $repoRoot 'target\release\misaka-studiod.exe'
if (-not (Test-Path $exe)) { throw "The build reported success but $exe is not there." }

Write-Host "`nBuilt." -ForegroundColor Green
Note "Run it later with:  $exe --ui-dir ui\dist"
Note 'Chat needs llama.cpp on PATH (`llama-server`); mining does not.'

if ($NoRun) { return }

Step 'Starting — open http://127.0.0.1:1338 and leave this window open'
& $exe --ui-dir (Join-Path $repoRoot 'ui\dist')
