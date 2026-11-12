# 한국어 Moonshine 모델과 Silero VAD 모델을 src-tauri/models/ 에 받는다.
$ErrorActionPreference = "Stop"
$Base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"
$Name = "sherpa-onnx-moonshine-tiny-ko-quantized-2026-02-27"
$Dir = Join-Path $PSScriptRoot "..\src-tauri\models"
New-Item -ItemType Directory -Force $Dir | Out-Null
if (-not (Test-Path (Join-Path $Dir "$Name\encoder_model.ort"))) {
    $Archive = Join-Path $Dir "$Name.tar.bz2"
    Invoke-WebRequest "$Base/$Name.tar.bz2" -OutFile $Archive
    tar -xjf $Archive -C $Dir   # Windows 10 이상에 들어 있는 bsdtar
    Remove-Item $Archive
}
$Vad = Join-Path $Dir "silero_vad.onnx"
if (-not (Test-Path $Vad)) { Invoke-WebRequest "$Base/silero_vad.onnx" -OutFile $Vad }
Write-Host "완료: $Dir"
