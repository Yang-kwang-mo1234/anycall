#!/usr/bin/env bash
# 한국어 Moonshine 모델과 Silero VAD 모델을 src-tauri/models/ 에 받는다.
set -euo pipefail
BASE=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models
NAME=sherpa-onnx-moonshine-tiny-ko-quantized-2026-02-27
DIR="$(cd "$(dirname "$0")/.." && pwd)/src-tauri/models"
mkdir -p "$DIR"
cd "$DIR"
if [ ! -f "$NAME/encoder_model.ort" ]; then
  curl -fSL -O "$BASE/$NAME.tar.bz2"
  tar xjf "$NAME.tar.bz2"
  rm "$NAME.tar.bz2"
fi
[ -f silero_vad.onnx ] || curl -fSL -O "$BASE/silero_vad.onnx"
echo "완료: $DIR"
