# anycall: 실시간 음성→텍스트 앱 (iOS / macOS / Windows)

## Context
`D:\kwang`(anycall 저장소)에는 README만 있다. 마이크로 들은 음성을 **앱 자체 입력창(textarea)**에 실시간으로 문자로 보여 주는 앱을 iOS, macOS, Windows에서 동작하게 만든다.
사용자 결정: 출력 대상은 앱 자체 입력창이다. 프레임워크는 Rust 기반으로 가볍고 빠른 것을 쓴다. 엔진은 find-skills로 더 나은 것이 있는지 먼저 보고, 없으면 OS 내장 엔진을 쓴다.

### find-skills 검토 결과
- `npx skills find`로 "speech to text", "whisper", "stt"를 검색했다. 나온 것은 ElevenLabs·Deepgram·OpenRouter·Groq 같은 **유료 클라우드 API 사용법**이나 faster-whisper(Python, 파일 일괄 변환) skill뿐이다.
- Rust 앱 안에 넣어 iOS까지 실시간으로 돌릴 수 있는 더 나은 엔진은 찾지 못했다. → **OS 내장 엔진(Recommended)**을 쓴다.
  - iOS/macOS: Apple Speech(`SFSpeechRecognizer`)
  - Windows: `Windows.Media.SpeechRecognition`
  - 두 엔진 모두 무료이고, 부분 결과(말하는 도중의 텍스트)와 ko-KR을 지원한다.
- 개발 보조용으로 `nodnarbnitram/claude-code-extensions@tauri-v2`(8.4K installs)가 있다. 설치는 선택 사항이며, 설치한다면 구현 전에 사용자 확인을 받는다.

## 선택한 구조
**Tauri v2**를 쓴다. Rust 백엔드에 시스템 WebView UI를 붙이는 방식이고, 하나의 프로젝트로 Windows·macOS·iOS를 모두 빌드할 수 있다. 바이너리 크기는 수 MB 수준이다.
- 프론트엔드: Vite + vanilla TypeScript. 화면은 textarea 하나와 시작/정지 버튼 하나다. 프레임워크는 쓰지 않는다(YAGNI).
- 백엔드: Rust 명령 `start_listening(lang)`, `stop_listening()`.
  - 인식 결과는 이벤트로 프론트에 보낸다. `stt-partial`은 지금 말하는 중인 텍스트이고, `stt-final`은 확정된 문장이다.
- 프론트 표시 규칙: 확정 텍스트 뒤에 부분 텍스트를 붙여 실시간으로 갱신한다. 문장이 확정되면 부분 텍스트를 확정 텍스트로 옮긴다.

## 파일 구성 (신규)
- `package.json`, `vite.config.ts`, `index.html`, `src/main.ts`: textarea, 버튼, 이벤트 수신(`@tauri-apps/api`의 `invoke`/`listen`)
- `src-tauri/Cargo.toml`, `tauri.conf.json`, `capabilities/default.json`
- `src-tauri/src/lib.rs`: 명령 등록, 플랫폼 모듈 선택(`#[cfg(...)]`)
- `src-tauri/src/stt/apple.rs`: iOS와 macOS가 같은 코드를 쓴다.
  - 사용 crate: `objc2-speech`, `objc2-avf-audio`, `block2`
  - 권한 요청: `SFSpeechRecognizer::requestAuthorization`
  - 녹음과 인식: `AVAudioEngine` inputNode에 tap을 걸고, 들어온 버퍼를 `SFSpeechAudioBufferRecognitionRequest`(`shouldReportPartialResults = true`)에 넘긴다.
  - 기기가 지원하면 `requiresOnDeviceRecognition`을 켠다.
  - 서버 인식은 약 1분 제한이 있으므로, 결과가 확정되거나 오류가 나면 세션을 자동으로 다시 시작한다.
- `src-tauri/src/stt/windows.rs`: `windows` crate의 `SpeechRecognizer`를 ko-KR로 쓴다.
  - `ContinuousRecognitionSession`을 쓰고, `HypothesisGenerated` 이벤트를 partial로, `ResultGenerated` 이벤트를 final로 보낸다.
- 권한 설정
  - iOS/macOS `Info.plist`: `NSMicrophoneUsageDescription`, `NSSpeechRecognitionUsageDescription`
  - macOS entitlements: `com.apple.security.device.audio-input`
- `README.md`: 빌드·실행 방법과 플랫폼별 요구사항 추가
- `.gitignore`: `node_modules`, `target`, `src-tauri/gen` 등

## 제약 / 가정
- **iOS와 macOS 빌드에는 Mac(Xcode)이 필요하다.** 이 PC(Windows)에서는 Windows 빌드만 실행해서 검증할 수 있다. Apple 쪽 코드는 작성까지만 하고, 검증은 Mac에서 사용자가 한다.
- Windows에서는 다음이 필요하다.
  - 설정 → 개인 정보 → 음성 인식의 **"온라인 음성 인식"**이 켜져 있어야 한다.
  - 한국어 음성 팩이 설치되어 있어야 한다.
- 기본 인식 언어는 ko-KR이다. 언어 선택 UI는 만들지 않고, 명령 인자로만 받는다.
- 커밋은 하되 push는 사용자가 요청할 때만 한다.

## 검증
1. 사전 확인(Windows): `rustc`, `cargo`, `node`, WebView2가 설치되어 있는지 본다. 없으면 설치 방법을 안내하고, 설치는 사용자가 한다.
2. `npm install` 후 `npm run tauri dev`로 앱을 띄운다.
   - 시작 버튼을 누르고 한국어로 말했을 때 textarea에 말하는 도중의 글자가 바로 나타나고, 문장이 끝나면 확정되는지 확인한다.
3. `cargo check`, `cargo clippy`를 Windows 타깃으로 통과시킨다.
4. Mac에서 사용자가 확인할 명령을 README에 적는다: `npm run tauri dev`(macOS), `npm run tauri ios dev`(iOS 시뮬레이터 또는 기기).
