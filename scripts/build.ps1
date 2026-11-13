# CMake(Visual Studio에 들어 있는 것)를 PATH에 넣고 libclang 위치를 잡은 뒤 cargo를 실행한다.
#   llama.cpp를 소스로 빌드하는 데 CMake가, Rust 바인딩(bindgen)을 만드는 데 libclang이 필요하다.
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

# libclang: LLVM이 없으면 pip의 libclang 휠(libclang.dll 포함)을 target\tools 에 받아 쓴다.
if (-not $env:LIBCLANG_PATH) {
    $ClangDir = Join-Path $Root "target\tools\libclang"
    $Native = Join-Path $ClangDir "clang\native"
    if (-not (Test-Path "$Native\libclang.dll")) {
        & python -m pip install --quiet --target $ClangDir libclang
        if ($LASTEXITCODE -ne 0) { throw "libclang을 받지 못했습니다. LLVM을 설치하고 LIBCLANG_PATH를 지정하세요." }
    }
    $env:LIBCLANG_PATH = $Native
}

if ($args.Count -eq 0) { $args = @("build", "--release") }
Push-Location $Root
try { & cargo @args; exit $LASTEXITCODE } finally { Pop-Location }
