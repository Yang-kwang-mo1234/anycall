# anycall: 실시간 음성→텍스트 앱 (iOS / macOS / Windows)

## 목표
마이크로 들은 음성을 **앱 자체 입력창(textarea)**에 실시간으로 문자로 보여 주는 앱을 만든다. iOS, macOS, Windows에서 동작해야 하고, 기본 인식 언어는 한국어다.

## 결정 사항
- **프레임워크: Tauri v2.** Rust 백엔드에 시스템 WebView UI를 붙이는 방식이고, 하나의 프로젝트로 세 OS를 모두 빌드한다.
- **음성인식 엔진: sherpa-onnx (온디바이스).** 세 OS에서 같은 엔진과 같은 Rust 코드를 쓴다. 인터넷, API 키, 사용량 제한이 없다.
  - 공식 Rust crate `sherpa-onnx`를 쓴다. 빌드할 때 미리 빌드된 라이브러리를 받아 정적으로 링크하고, iOS용 xcframework도 제공한다.
  - sherpa-onnx 저장소에 Tauri v2 예제(`tauri-examples`)가 있다. 구현할 때 참고한다.
- **모델: 한국어 스트리밍 Zipformer**(`sherpa-onnx-streaming-zipformer-korean-2024-06-16`, int8 약 126MB)를 기본으로 한다. 0단계에서 더 가벼운 Moonshine 한국어 모델과 비교해 확정한다.
- **프론트엔드: Vite + vanilla TypeScript.** 화면은 textarea 하나와 시작/정지 버튼 하나다.

## 엔진을 이렇게 정한 이유
| 후보 | 장점 | 문제 |
|---|---|---|
| **sherpa-onnx (선택)** | 세 OS 코드 하나, 오프라인, 무료·무제한, 음성이 기기 밖으로 안 나감 | 앱 용량이 모델만큼 커짐. 한국어 정확도는 직접 확인해야 함 |
| OS 내장 엔진 | 추가 용량 없음. Apple 쪽 품질이 좋음 | OS마다 코드가 따로 필요. Windows의 한국어 받아쓰기 지원이 확인되지 않음. Apple 서버 인식은 약 1분 제한 |
| 무료 클라우드 API | 정확도가 가장 높을 가능성 | 계속 무료인 스트리밍 한도가 작음(Azure 월 5시간, Google 월 60분). Deepgram은 가입 크레딧 방식. 앱에 API 키를 넣어야 하고 인터넷이 필요함 |
| WebView의 Web Speech API | 코드가 가장 적음 | WKWebView(iOS/macOS)에서 동작하지 않음 |

클라우드 API는 0단계에서 온디바이스 정확도가 부족하다고 판단될 때만 다시 검토한다.

## 0단계: 모델 정확도 확인 (앱을 만들기 전에)
한국어 모델의 공개된 정확도 수치를 찾지 못했다. 그래서 앱 구조를 만들기 전에 직접 들어 보고 결정한다.

1. 데스크톱(Windows 또는 macOS)에서 `sherpa-onnx` crate의 마이크 스트리밍 예제(`rust-api-examples`)를 한국어 모델로 실행한다.
2. 후보 두 개를 같은 문장으로 비교한다.

| 모델 | 방식 | 크기 | 비고 |
|---|---|---|---|
| 스트리밍 Zipformer 한국어 | 말하는 도중 글자가 바로 나오는 진짜 스트리밍 | int8 인코더 약 121MB + 디코더·조이너 약 5MB | 한국어 전용 |
| Moonshine tiny 한국어 (`sherpa-onnx-moonshine-tiny-ko-quantized-2026-02-27`) | 발화 구간(VAD) 단위로 인식 | Zipformer보다 작음(정확한 크기는 받아서 확인) | 비영어 모델은 Moonshine Community License(비상업 또는 소규모 상업용)라는 제3자 설명이 있어 라이선스 확인 필요 |

3. 판단 기준
   - 일상 문장을 읽었을 때 뜻이 통하게 받아 적는가.
   - 말하는 도중에 글자가 1초 안에 따라 나오는가.
   - 문장부호와 띄어쓰기가 쓸 만한가.
4. 결과에 따른 진행
   - 둘 중 하나가 쓸 만하면 그 모델로 진행한다. 둘 다 쓸 만하면 실시간성이 좋은 Zipformer를 쓴다.
   - 둘 다 부족하면 구현을 멈추고 엔진을 다시 정한다(Apple 기기는 OS 내장 엔진, Windows는 클라우드 API 등).

## 구조
### 명령 (프론트 → Rust)
- `start_listening()`: 마이크를 열고 인식을 시작한다.
- `stop_listening()`: 인식을 멈추고 마이크를 놓는다.

### 이벤트 (Rust → 프론트)
| 이벤트 | 내용 |
|---|---|
| `stt-partial` | 지금 말하는 중인 텍스트 |
| `stt-final` | 확정된 문장 |
| `stt-state` | 듣는 중인지 여부 |
| `stt-error` | 오류 코드와 메시지 (마이크 권한 거부, 마이크 없음, 모델 파일 없음 등) |

### 처리 흐름
1. `cpal`로 마이크 입력을 받는다(Windows는 WASAPI, Apple은 CoreAudio).
2. 오디오 콜백에서 받은 샘플을 채널로 인식 스레드에 넘긴다.
3. 인식 스레드가 샘플을 `OnlineRecognizer` 스트림에 넣고 디코딩한다. 16kHz 변환은 sherpa-onnx가 한다.
4. 결과 텍스트가 바뀌면 `stt-partial`을 보낸다.
5. sherpa-onnx의 끝점 감지(침묵)가 문장 끝을 알리면 `stt-final`을 보내고 스트림을 초기화한다.

### 화면 동작
- 확정 텍스트 뒤에 부분 텍스트를 붙여 실시간으로 갱신한다. 문장이 확정되면 부분 텍스트를 확정 텍스트로 옮긴다.
- 듣는 동안 textarea는 읽기 전용이다. 정지하면 편집할 수 있다.
- `stt-error`를 받으면 버튼 옆에 메시지를 보여 주고 정지 상태로 돌아간다.

## 파일 구성
- `package.json`, `vite.config.ts`, `index.html`, `src/main.ts`: 화면, 명령 호출, 이벤트 수신
- `src-tauri/Cargo.toml`: `tauri`, `sherpa-onnx`, `cpal`
- `src-tauri/tauri.conf.json`: `bundle.resources`에 모델 폴더를 넣는다.
- `src-tauri/capabilities/default.json`
- `src-tauri/src/lib.rs`: 명령 등록, 상태 관리
- `src-tauri/src/stt.rs`: 마이크 입력, 인식 스레드, 이벤트 전송. 세 OS 공용이다.
  - iOS 전용 부분(`#[cfg(target_os = "ios")]`)만 따로 둔다. 녹음 전에 `AVAudioSession`을 녹음용으로 설정하고 활성화한다(`objc2-avf-audio`).
- `scripts/download-model.sh`, `scripts/download-model.ps1`: 모델을 `src-tauri/models/`에 받는다. 모델은 저장소에 넣지 않는다.
- 권한 설정
  - macOS: `src-tauri/Info.plist`에 `NSMicrophoneUsageDescription`. entitlements에 `com.apple.security.device.audio-input`.
  - iOS: `src-tauri/gen/apple` 아래 iOS `Info.plist`에 `NSMicrophoneUsageDescription`.
  - 음성인식 권한(`NSSpeechRecognitionUsageDescription`)은 필요 없다.
- `.gitignore`: `node_modules`, `dist`, `src-tauri/target`, `src-tauri/models`, `src-tauri/gen/schemas`, `src-tauri/gen/apple/build`
  - `src-tauri/gen/apple`의 나머지(Xcode 프로젝트, `Info.plist`)는 커밋한다.
- `README.md`: 모델 받기, 빌드·실행 방법, 플랫폼별 요구사항, 모델 라이선스

## 개발 환경
- 공통: Rust stable, Node.js LTS
- Windows: WebView2 런타임, MSVC 빌드 도구
- iOS/macOS: Mac과 Xcode가 필요하다. 다른 OS에서는 빌드할 수 없다.
- iOS 실기기 실행에는 Apple ID로 서명해야 한다.

## 확인이 필요한 위험
- **iOS 마이크 입력**: `cpal`의 iOS 지원이 Tauri 앱 안에서 문제없이 동작하는지 확인되지 않았다. 안 되면 iOS만 `AVAudioEngine`으로 입력을 받는다.
- **iOS 링크**: `sherpa-onnx` crate가 iOS를 지원한다고 하지만 Tauri iOS 빌드에서의 설정 절차는 찾지 못했다. 데스크톱이 동작한 직후에 iOS 빌드부터 확인한다.
- **앱 용량**: 모델 때문에 설치 용량이 130MB 안팎 늘어난다.
- **모델 라이선스**: 배포하기 전에 선택한 모델의 라이선스를 확인한다.

## 범위 밖
- 다른 앱의 입력창에 직접 입력하는 기능
- 한국어 외 언어, 언어 선택 UI
- 인식 결과 저장, 내보내기
- 클라우드 API 연동

## 검증
1. 0단계 결과를 기록한다(선택한 모델과 이유).
2. `cargo check`, `cargo clippy`를 각 타깃에서 통과시킨다.
3. 플랫폼별로 앱을 띄운다.
   - Windows, macOS: `npm run tauri dev`
   - iOS: `npm run tauri ios dev` (시뮬레이터 또는 실기기)
4. 플랫폼마다 아래를 확인한다.
   - 인터넷을 끊은 상태에서 시작을 누르고 한국어로 말하면 말하는 도중의 글자가 바로 나타나고, 문장이 끝나면 확정된다.
   - 5분 넘게 계속 써도 끊기거나 느려지지 않는다.
   - 10초 이상 침묵한 뒤 다시 말해도 인식이 이어진다.
   - 마이크 권한을 거부하면 오류 메시지가 보이고 앱이 멈추지 않는다.
   - 정지를 누르면 마이크 사용 표시가 꺼지고 textarea를 편집할 수 있다.

## 참고
- sherpa-onnx: https://github.com/k2-fsa/sherpa-onnx
- Rust crate 문서: https://docs.rs/sherpa-onnx
- 한국어 스트리밍 Zipformer 모델: https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html
- Moonshine 모델: https://k2-fsa.github.io/sherpa/onnx/moonshine/index.html
