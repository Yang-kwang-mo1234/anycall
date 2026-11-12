# anycall

iOS, macOS, Windows에서 함께 쓰는 크로스 플랫폼 프로그램입니다.
마이크로 들은 한국어 음성을 앱의 입력창에 실시간으로 받아 적습니다. 음성인식은 기기 안에서 하므로(sherpa-onnx + Moonshine) 인터넷이 필요 없습니다.

## 지원 플랫폼

- iOS
- macOS
- Windows (현재 구현됨)

## 준비

- Rust stable, Node.js LTS
- Windows: MSVC 빌드 도구(Visual Studio Build Tools, C++ 워크로드), WebView2 런타임
- iOS/macOS: Mac과 Xcode

## 모델 받기

모델은 저장소에 넣지 않습니다. `src-tauri/models/`에 받습니다(약 75MB).

```powershell
./scripts/download-model.ps1
```

macOS/Linux에서는 `scripts/download-model.sh`를 실행합니다.

## 실행과 빌드

```bash
npm install
npm run tauri dev
```

설치 파일은 `npm run tauri build`로 만듭니다.

## 모델 라이선스

- `sherpa-onnx-moonshine-tiny-ko-quantized-2026-02-27`: Moonshine AI Community License. 연구·비상업 용도는 무료이고, 상업 용도는 연매출 100만 달러 미만이면 무료지만 moonshine.ai에 등록해야 합니다. 원문은 모델 폴더의 `LICENSE`에 있습니다.
- `silero_vad.onnx`: MIT License.
