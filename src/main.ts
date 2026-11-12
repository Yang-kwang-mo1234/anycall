import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const text = document.querySelector<HTMLTextAreaElement>("#text")!;
const button = document.querySelector<HTMLButtonElement>("#toggle")!;
const error = document.querySelector<HTMLSpanElement>("#error")!;

let listening = false;
let committed = ""; // 확정된 텍스트. 부분 텍스트는 이 뒤에 붙여 보여 준다.

function render(partial: string) {
  text.value = committed + partial;
  text.scrollTop = text.scrollHeight;
}

function setListening(on: boolean) {
  listening = on;
  text.readOnly = on;
  button.textContent = on ? "정지" : "시작";
}

listen<string>("stt-partial", (e) => render(e.payload));

listen<string>("stt-final", (e) => {
  committed += e.payload + "\n";
  render("");
});

listen<boolean>("stt-state", (e) => setListening(e.payload));

listen<{ code: string; message: string }>("stt-error", (e) => {
  error.textContent = e.payload.message;
  setListening(false);
});

button.addEventListener("click", async () => {
  error.textContent = "";
  if (listening) {
    await invoke("stop_listening");
  } else {
    // 정지 중에 사용자가 고친 내용을 이어서 쓴다.
    committed = text.value && !text.value.endsWith("\n") ? text.value + "\n" : text.value;
    await invoke("start_listening");
  }
});
