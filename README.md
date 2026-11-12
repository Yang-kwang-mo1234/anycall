# anycall

Windows에서 재생 중인 **영어·일본어** 동영상 소리를 받아 화면 위에 **한국어 번역 자막**을 실시간으로 띄웁니다.
브라우저든 동영상 플레이어든 소리가 나는 프로그램이면 모두 대상입니다. 인식과 번역은 모두 PC 안에서 하므로 인터넷이 필요 없습니다.

- 소리 입력: 기본 출력 장치의 WASAPI loopback (마이크 불필요)
- 음성 구간 검출: Silero VAD
- 음성 인식: sherpa-onnx + Moonshine tiny (영어, 일본어 각각)
- 번역: CTranslate2 + NLLB-200 distilled 600M int8 (Intel MKL)
- 화면: egui 투명 오버레이(항상 위, 마우스 통과) + 시작/정지 제어 창

설계와 측정 결과는 [docs/PLAN.md](docs/PLAN.md)에 있습니다.

## 준비

- Windows 10 2004 이상, x64
- Rust stable
- Visual Studio Build Tools 2022, "C++를 사용한 데스크톱 개발" 워크로드(MSVC, **C++ CMake tools** 포함)

## 모델 받기

모델은 저장소에 넣지 않습니다. `models/`에 받습니다(약 700MB, 대부분 번역 모델).

```powershell
.\scripts\download-models.ps1
```

`-Base`를 주면 더 큰 Moonshine base 모델도 받습니다(`--model base`로 사용).

## 빌드

CTranslate2를 C++ 소스부터 빌드하므로 첫 빌드는 몇 분 걸립니다. 스크립트가 Visual Studio의 CMake와 nuget.exe(Intel MKL 다운로드용)를 PATH에 넣고 cargo를 실행합니다.

```powershell
.\scripts\build.ps1
```

다른 cargo 명령도 같은 방법으로 실행합니다: `.\scripts\build.ps1 clippy --release`

## 실행

```powershell
.\target\release\anycall.exe
```

제어 창에서 원문 언어(영어/일본어)를 고르고 **시작**을 누른 뒤 동영상을 재생합니다. 화면 아래쪽 가운데에 자막이 나옵니다.

- 흐린 노란색 줄: 말하는 중인 문장(부분 번역, 번역이 아직 없으면 원문)
- 흰색 줄: 확정된 문장. 5초 뒤 사라집니다.
- **원문 함께 보기**를 켜면 번역 위에 원문을 작게 보여 줍니다.
- 제어 창을 닫으면 프로그램이 끝납니다.

### 콘솔 모드 (지연 측정)

```powershell
.\target\release\anycall.exe --console en                 # 시스템 소리를 받아 콘솔에 출력, Enter로 끝
.\target\release\anycall.exe --console ja --wav 대사.wav  # wav 파일을 실제 속도로 흘려 측정
.\target\release\anycall.exe --bench-translate            # 번역 시간·부분 번역 흔들림
```

줄마다 말 시작(부분) 또는 말 끝(확정)에서 지난 시간을 찍고, 끝에 지연 요약을 냅니다. 릴리스 빌드는 창 프로그램이라 PowerShell에서는 출력이 프롬프트 뒤에 섞여 나올 수 있습니다. `| Out-Host`를 붙이면 끝날 때까지 기다립니다.

## 알려진 한계

- 문장이 끝난 뒤 확정 자막까지 평균 약 1.3초 걸립니다(목표 0.7초). 긴 문장은 2초까지 걸립니다. NLLB 번역 속도가 병목입니다.
- 독점 전체 화면 모드의 플레이어·게임 위에는 오버레이가 보이지 않습니다. 테두리 없는 전체 화면(브라우저 전체 화면 포함)은 됩니다.
- Windows 11에서 오버레이 창 둘레에 1px 테두리가 희미하게 보입니다.

## 모델 라이선스

- Moonshine 영어 모델: MIT License. Moonshine 일본어 모델: Moonshine Community License(비상업 라이선스). 원문은 모델 폴더의 `LICENSE`에 있습니다.
- NLLB-200: CC-BY-NC 4.0 (비상업 전용)
- Silero VAD: MIT License
- Intel MKL(빌드 시 정적 링크): Intel Simplified Software License
