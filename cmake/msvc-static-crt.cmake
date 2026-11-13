# llama.cpp(llama-cpp-sys-2)를 정적 C 런타임(/MT)으로 빌드한다. sherpa-onnx 정적 라이브러리와 맞춘다.
# llama.cpp는 CMP0091(NEW)을 써서 CMAKE_MSVC_RUNTIME_LIBRARY가 컴파일 옵션의 -MT보다 우선한다.
# .cargo/config.toml의 CMAKE_TOOLCHAIN_FILE 환경 변수로 넘긴다(CMake 3.21 이상).
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
