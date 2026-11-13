# anycall

**영어·일본어** 동영상에 **한국어 자막**을 붙여 보여 주는 Windows 프로그램입니다. 인식과 번역은 모두 PC 안에서 하므로 인터넷이 필요 없습니다.

두 가지 방식으로 쓸 수 있습니다.

- **플레이어(기본)**: 동영상 파일을 열어 재생합니다. 같은 이름의 자막 파일이 있으면 그것을 보여 주고, 없으면 음성 언어를 판별해 영어·일본어는 한국어 AI 자막을 만들고 한국어는 번역하지 않습니다. 소리를 재생보다 앞서 읽어 분석하므로 자막이 말과 같은 때에 나옵니다.
- **시스템 소리 오버레이**(`--overlay`): 브라우저든 다른 플레이어든 재생 중인 소리를 받아 화면 위에 실시간 자막을 띄웁니다.

구성
- 재생: Media Foundation Media Engine(Windows 내장 디코더)
- 음성 구간 검출: Silero VAD
- 음성 언어 판별: sherpa-onnx + Whisper tiny 다국어
- 음성 인식: sherpa-onnx + Moonshine tiny (영어, 일본어 각각)
- 번역: llama.cpp + Hy-MT2-1.8B (Tencent, GGUF Q4_K_M)
- 화면: egui(OpenGL) 투명 창을 영상 창 위에 겹친다

설계와 측정 결과는 [docs/PLAYER_PLAN.md](docs/PLAYER_PLAN.md)(플레이어)와 [docs/PLAN.md](docs/PLAN.md)(오버레이)에 있습니다.

## 준비

- Windows 10 2004 이상, x64
- Rust stable
- Visual Studio Build Tools 2022, "C++를 사용한 데스크톱 개발" 워크로드(MSVC, **C++ CMake tools** 포함)

## 모델 받기

모델은 저장소에 넣지 않습니다. `models/`에 받습니다(약 1.4GB, 대부분 번역 모델).

```powershell
.\scripts\download-models.ps1
```

`-Base`를 주면 더 큰 Moonshine base 모델도 받습니다(`--model base`로 사용).

## 빌드

llama.cpp를 C++ 소스부터 빌드하므로 첫 빌드는 몇 분 걸립니다. 스크립트가 Visual Studio의 CMake를 PATH에 넣고, Rust 바인딩 생성에 필요한 libclang을 잡은 뒤(`LIBCLANG_PATH`가 없으면 pip의 `libclang` 휠을 `target	ools`에 받습니다) cargo를 실행합니다.

```powershell
.\scripts\build.ps1
```

다른 cargo 명령도 같은 방법으로 실행합니다: `.\scripts\build.ps1 clippy --release`, `.\scripts\build.ps1 test --release`

실행 파일 아이콘은 `assets/anycall.ico`이고 `build.rs`가 실행 파일에 넣습니다. "연결 프로그램"으로 anycall에 연결한 동영상 파일도 이 아이콘으로 보입니다. 아이콘을 바꾸려면 `scripts/make-icon.py`를 고쳐 `python scripts/make-icon.py`로 다시 만듭니다.

## 플레이어

```powershell
.\target\release\anycall.exe                    # 빈 창. 파일을 끌어다 놓거나 O 키로 연다
.\target\release\anycall.exe 영화.mp4           # 바로 연다
.\target\release\anycall.exe 영화.mp4 --lang ja # AI 자막 언어를 고정(auto | en | ja | off)
```

자막 고르기
- 동영상과 같은 폴더의 같은 이름 자막 파일을 찾습니다. 순서는 `영화.ko.srt`처럼 한국어 표시가 있는 것 → `영화.srt`(표시 없음) → `영화.en.srt` 같은 다른 언어입니다.
- 형식은 SRT, SMI, VTT, ASS/SSA입니다. 글자 인코딩은 BOM이 있으면 그대로, 없으면 UTF-8로 읽어 보고 안 되면 CP949로 읽습니다. SMI에 언어가 여럿이면 한국어(KRCC)를 고릅니다.
- 자막 파일이 없으면 AI 자막을 만듭니다. 음성 언어는 파일의 세 지점(처음, 30%, 60%)에서 판별해 다수결로 정합니다. 한국어면 번역하지 않고, 영어·일본어가 아닌 언어나 말소리가 없으면 자막을 만들지 않고 화면 왼쪽 위에 알립니다.
- 조작 막대의 자막 메뉴에서 파일 자막 / AI 자막(자동·영어·일본어) / 끄기를 바꿀 수 있습니다. 자막 파일을 창에 끌어다 놓아도 됩니다.
- AI 자막은 처음에 모델을 불러오느라 10초쯤 걸리고, 그 뒤로는 재생 속도의 약 3배로 분석합니다(Ryzen 5 7530U). 분석을 마친 구간은 탐색 막대에 파란색으로 표시됩니다. 분석하지 않은 곳으로 건너뛰면 그 위치부터 다시 분석합니다.

키보드
| 키 | 동작 |
|---|---|
| Space, 영상 클릭 | 재생 / 일시정지 |
| ← / → | 5초 뒤로 / 앞으로 |
| ↑ / ↓ | 볼륨 |
| F, Enter, 영상 두 번 클릭 | 전체 화면 (Esc로 나옴) |
| `[` / `]` | 자막을 0.5초 앞당기기 / 늦추기 (Shift는 0.1초) |
| S | AI 자막의 원문 함께 보기 |
| O | 파일 열기 |

재생할 수 없는 파일
- Windows 내장 디코더를 쓰므로 PC에 설치된 코덱에 따라 재생 가능한 형식이 다릅니다. HEVC(H.265)는 Microsoft Store의 'HEVC 비디오 확장'이 있어야 하고, **DTS 소리와 10비트 H.264 영상은 재생할 수 없습니다.** 이런 파일은 창 가운데에 원인과 해결 방법을 보여 줍니다.

## 시스템 소리 오버레이

```powershell
.\target\release\anycall.exe --overlay
```

제어 창에서 원문 언어(영어/일본어)를 고르고 **시작**을 누른 뒤 동영상을 재생합니다. 화면 아래쪽 가운데에 자막이 나옵니다.

- 흐린 노란색 줄: 말하는 중인 문장(부분 번역, 번역이 아직 없으면 원문)
- 흰색 줄: 확정된 문장. 5초 뒤 사라집니다.
- **원문 함께 보기**를 켜면 번역 위에 원문을 작게 보여 줍니다.
- 제어 창을 닫으면 프로그램이 끝납니다.
- 스피커가 음소거되어 있으면 소리를 받지 못해 자막이 나오지 않을 수 있습니다.

### 콘솔 명령 (측정·진단)

```powershell
.\target\release\anycall.exe --console en                 # 시스템 소리를 받아 콘솔에 출력, Enter로 끝
.\target\release\anycall.exe --console ja --wav 대사.wav  # wav 파일을 실제 속도로 흘려 측정
.\target\release\anycall.exe --bench-translate            # 번역 시간·부분 번역 흔들림
.\target\release\anycall.exe --file-bench 영화.mp4 --lang en  # 플레이어의 파일 분석 속도
.\target\release\anycall.exe --langid 영화.mp4            # 음성 언어 판별 결과
.\target\release\anycall.exe --audio-info 영화.mkv        # 소리 형식·읽기 속도·탐색 확인
```

릴리스 빌드는 창 프로그램이라 PowerShell에서는 출력이 프롬프트 뒤에 섞여 나올 수 있습니다. `| Out-Host`를 붙이면 끝날 때까지 기다립니다.

### 번역 속도 조절

번역은 CPU 논리 코어의 절반을 씁니다. `ANYCALL_LLM_THREADS`로 스레드 수를 바꿀 수 있습니다. `ANYCALL_LLAMA_LOG=1`을 주면 llama.cpp 로그를 보여 줍니다.

## 알려진 한계

- 플레이어: DTS 소리가 든 파일은 영상도 재생되지 않습니다(소리 트랙만 끄고 재생하는 기능은 아직 없습니다). MKV 안에 들어 있는 자막 트랙은 읽지 않습니다.
- 오버레이: 문장이 끝난 뒤 확정 자막까지 Intel에서 평균 약 1.3초 걸립니다(목표 0.7초). 독점 전체 화면 모드의 플레이어·게임 위에는 오버레이가 보이지 않습니다.

## 모델 라이선스

- Moonshine 영어 모델: MIT License. Moonshine 일본어 모델: Moonshine Community License(비상업 라이선스). 원문은 모델 폴더의 `LICENSE`에 있습니다.
- Hy-MT2-1.8B: Apache License 2.0 (원문: 모델 저장소의 `LICENSE.txt`)
- Whisper: MIT License
- Silero VAD: MIT License
- llama.cpp(빌드 시 정적 링크): MIT License
