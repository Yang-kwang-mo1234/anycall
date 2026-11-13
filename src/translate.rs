//! Hy-MT2-1.8B (Tencent, GGUF Q4_K_M)을 llama.cpp로 돌려 영어·일본어를 한국어로 번역한다.
//!
//! 디코더만 있는 LLM이라 모델 카드의 번역 프롬프트를 채팅 형식으로 넣고, 답을 그리디로 생성한다.
//! 자막은 같은 원문에 같은 번역이 나와야 해서 샘플링(temperature)은 쓰지 않는다.

use crate::Lang;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::OnceLock;

pub const MODEL_FILE: &str = "models/Hy-MT2-1.8B-GGUF/Hy-MT2-1.8B-Q4_K_M.gguf";

/// 원문 + 프롬프트 + 번역이 들어갈 문맥 길이. 자막 구간은 길어야 몇십 단어다.
const N_CTX: u32 = 1024;
/// 번역문 최대 토큰 수(이전 NLLB 설정과 같다).
const MAX_NEW_TOKENS: usize = 128;
/// 모델 카드 권장값(1.8B): repetition_penalty 1.05.
const REPEAT_PENALTY: f32 = 1.05;

pub struct Translator {
    model: LlamaModel,
    threads: i32,
}

/// llama.cpp 백엔드는 프로세스에 하나만 만들 수 있다.
fn backend() -> Result<&'static LlamaBackend, String> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let mut b = LlamaBackend::init().map_err(|e| format!("llama.cpp 초기화 실패: {e}"))?;
            if std::env::var_os("ANYCALL_LLAMA_LOG").is_none() {
                b.void_logs();
            }
            Ok(b)
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl Translator {
    pub fn load() -> Result<Self, String> {
        if !Path::new(MODEL_FILE).is_file() {
            return Err(format!("번역 모델 파일이 없습니다: {MODEL_FILE}"));
        }
        let model = LlamaModel::load_from_file(backend()?, MODEL_FILE, &LlamaModelParams::default())
            .map_err(|e| format!("번역 모델을 불러오지 못했습니다: {e}"))?;
        Ok(Self { model, threads: thread_count() })
    }

    /// `cancel`이 true를 돌려주면 생성 도중에 멈추고 `Ok(None)`을 준다.
    pub fn translate(&self, _lang: Lang, text: &str, cancel: &dyn Fn() -> bool) -> Result<Option<String>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Some(String::new()));
        }
        let vocab = self.model.vocab();
        let tokens = vocab.tokenize(prompt(text).as_bytes(), false, true);
        if tokens.len() + MAX_NEW_TOKENS > N_CTX as usize {
            return Err(format!("원문이 너무 깁니다({}토큰)", tokens.len()));
        }

        // 문맥은 번역할 때마다 새로 만든다(KV 캐시 약 64MB, 만드는 데 몇 ms). 모델은 공유한다.
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_CTX)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        let mut ctx = self.model.new_context(backend()?, params).map_err(|e| format!("번역 문맥 생성 실패: {e}"))?;

        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() as i32 - 1;
        for (i, &t) in tokens.iter().enumerate() {
            batch.add(t, i as i32, &[0], i as i32 == last).map_err(|e| e.to_string())?;
        }
        ctx.decode(&mut batch).map_err(|e| format!("번역 실패: {e}"))?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(self.model.n_vocab(), 64, REPEAT_PENALTY, 0.0, 0.0),
            LlamaSampler::greedy(),
        ]);
        let mut out = Vec::new();
        let mut pos = tokens.len() as i32;
        for _ in 0..MAX_NEW_TOKENS {
            if cancel() {
                return Ok(None);
            }
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if vocab.is_eog(token) {
                break;
            }
            vocab.token_to_piece_into(token, &mut out, false, None);
            batch.clear();
            batch.add(token, pos, &[0], true).map_err(|e| e.to_string())?;
            pos += 1;
            ctx.decode(&mut batch).map_err(|e| format!("번역 실패: {e}"))?;
        }
        Ok(Some(String::from_utf8_lossy(&out).trim().to_string()))
    }
}

/// 모델 카드의 기본 번역 프롬프트(영어판)를 채팅 형식(chat_template.jinja)으로 감싼다. 시스템 프롬프트는 없다.
fn prompt(text: &str) -> String {
    format!(
        "<｜hy_begin▁of▁sentence｜><｜hy_User｜>Translate the following text into Korean. \
         Note that you should only output the translated result without any additional explanation:\n\n\
         {text}<｜hy_Assistant｜>"
    )
}

/// 인식·동영상 디코딩과 CPU를 나눠 쓰므로 논리 코어의 절반을 쓴다. `ANYCALL_LLM_THREADS`로 바꿀 수 있다.
fn thread_count() -> i32 {
    if let Some(n) = std::env::var("ANYCALL_LLM_THREADS").ok().and_then(|v| v.parse().ok()).filter(|&n: &i32| n > 0) {
        return n;
    }
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as i32);
    (cores / 2).max(1)
}
