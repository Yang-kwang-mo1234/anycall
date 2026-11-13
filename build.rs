//! 실행 파일에 아이콘을 넣는다. "연결 프로그램"으로 anycall에 연결된 파일과 탐색기·작업 표시줄이 이 아이콘을 쓴다.
//! 아이콘은 scripts/make-icon.py로 만든다.
fn main() {
    println!("cargo:rerun-if-changed=assets/anycall.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/anycall.ico");
        res.compile().expect("아이콘 리소스를 넣지 못했습니다");
    }
}
