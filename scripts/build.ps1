# CMake(Visual Studio에 들어 있는 것)와 nuget.exe를 PATH에 넣고 cargo를 실행한다.
#   CTranslate2를 소스로 빌드하는 데 CMake가, Intel MKL을 받는 데 nuget.exe가 필요하다.
#   예) .\scripts\build.ps1                 → cargo build --release
#       .\scripts\build.ps1 clippy --release
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")

if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $Vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $Vs = & $Vswhere -latest -products * -property installationPath
    $CmakeBin = Join-Path $Vs "Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
    if (-not (Test-Path "$CmakeBin\cmake.exe")) { throw "CMake를 찾을 수 없습니다. Visual Studio Build Tools의 'C++ CMake tools'를 설치하세요." }
    $env:PATH = "$env:PATH;$CmakeBin"
}

if (-not (Get-Command nuget -ErrorAction SilentlyContinue)) {
    $Tools = Join-Path $Root "target\tools"
    $Nuget = Join-Path $Tools "nuget.exe"
    if (-not (Test-Path $Nuget)) {
        New-Item -ItemType Directory -Force $Tools | Out-Null
        Invoke-WebRequest "https://dist.nuget.org/win-x86-commandline/latest/nuget.exe" -OutFile $Nuget
    }
    $env:PATH = "$env:PATH;$Tools"
}

if ($args.Count -eq 0) { $args = @("build", "--release") }
Push-Location $Root
try { & cargo @args; exit $LASTEXITCODE } finally { Pop-Location }
