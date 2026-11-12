# 인식·번역 모델을 models/ 에 받는다. 이미 있는 것은 건너뛴다.
#   -Base 를 주면 더 큰 Moonshine base 모델도 받는다(anycall --model base).
param([switch]$Base)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # 진행률 표시가 다운로드를 크게 늦춘다.
$Dir = Join-Path $PSScriptRoot "..\models"
New-Item -ItemType Directory -Force $Dir | Out-Null

$Asr = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"
function Get-Archive($Name, $Check) {
    if (Test-Path (Join-Path $Dir "$Name\$Check")) { return }
    Write-Host "받는 중: $Name"
    $Archive = Join-Path $Dir "$Name.tar.bz2"
    Invoke-WebRequest "$Asr/$Name.tar.bz2" -OutFile $Archive
    # Windows 10 이상에 들어 있는 bsdtar. PATH에 Git의 GNU tar가 먼저 있으면 D: 경로를 원격 호스트로 읽는다.
    & "$env:SystemRoot\System32\tar.exe" -xjf $Archive -C $Dir
    if ($LASTEXITCODE -ne 0) { throw "압축 풀기 실패: $Archive" }
    Remove-Item $Archive
}

# 음성 구간 검출
$Vad = Join-Path $Dir "silero_vad.onnx"
if (-not (Test-Path $Vad)) { Invoke-WebRequest "$Asr/silero_vad.onnx" -OutFile $Vad }

# 음성 인식: Moonshine tiny (영어, 일본어 각각)
$Sizes = @("tiny")
if ($Base) { $Sizes += "base" }
foreach ($Size in $Sizes) {
    foreach ($Lang in "en", "ja") {
        Get-Archive "sherpa-onnx-moonshine-$Size-$Lang-quantized-2026-02-27" "tokens.txt"
    }
}

# 번역: NLLB-200 distilled 600M, CTranslate2 int8 변환본
$Nllb = Join-Path $Dir "nllb-200-distilled-600M-ct2-int8"
$Hf = "https://huggingface.co/JustFrederik/nllb-200-distilled-600M-ct2-int8/resolve/main"
New-Item -ItemType Directory -Force $Nllb | Out-Null
foreach ($f in "config.json", "shared_vocabulary.txt", "sentencepiece.bpe.model", "tokenizer.json", "special_tokens_map.json", "tokenizer_config.json", "model.bin") {
    $Out = Join-Path $Nllb $f
    if (-not (Test-Path $Out)) {
        Write-Host "받는 중: nllb/$f"
        Invoke-WebRequest "$Hf/$f" -OutFile $Out
    }
}
Write-Host "완료: $((Resolve-Path $Dir).Path)"
