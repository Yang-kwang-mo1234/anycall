//! NLLB-200 distilled 600M (CTranslate2, Intel은 int8·그 밖은 float32)으로 영어·일본어를 한국어로 번역한다.

use crate::Lang;
use ct2rs::{ComputeType, Config, TranslationOptions};
use std::path::Path;

pub const NLLB_DIR: &str = "models/nllb-200-distilled-600M-ct2-int8";

pub struct Translator {
    inner: ct2rs::Translator<ct2rs::tokenizers::auto::Tokenizer>,
    options: TranslationOptions<String, String>,
}

impl Translator {
    pub fn load() -> Result<Self, String> {
        if !Path::new(NLLB_DIR).join("model.bin").is_file() {
            return Err(format!("번역 모델 파일이 없습니다: {NLLB_DIR}/model.bin"));
        }
        let config = Config {
            compute_type: select_compute_type(),
            num_threads_per_replica: 1,
            ..Config::default()
        };
        let inner = ct2rs::Translator::new(NLLB_DIR, &config)
            .map_err(|e| format!("번역 모델을 불러오지 못했습니다: {e}"))?;
        let options = TranslationOptions {
            beam_size: 1,
            max_decoding_length: 128,
            repetition_penalty: 1.1,
            ..TranslationOptions::default()
        };
        Ok(Self { inner, options })
    }

    /// `cancel`이 true를 돌려주면 생성 도중에 멈추고 `Ok(None)`을 준다.
    pub fn translate(&self, lang: Lang, text: &str, cancel: &dyn Fn() -> bool) -> Result<Option<String>, String> {
        let source = match lang {
            Lang::En => "eng_Latn",
            Lang::Ja => "jpn_Jpan",
        };
        let mut cancelled = false;
        let mut on_step = |_: ct2rs::GenerationStepResult| {
            cancelled = cancel();
            if cancelled {
                anyhow::bail!("cancelled");
            }
            Ok(())
        };
        let res = self.inner.translate_batch_with_source_and_target_prefix(
            &[text],
            &[vec![source]],
            &[vec!["kor_Hang"]],
            &self.options,
            Some(&mut on_step),
        );
        match res {
            Err(_) if cancelled => Ok(None),
            Err(e) => Err(format!("번역 실패: {e}")),
            // 모르는 토큰 표시(<unk>)는 자막에 보이지 않게 뺀다.
            Ok(r) => Ok(Some(r.into_iter().next().map(|(t, _)| t.replace("<unk>", "").trim().to_string()).unwrap_or_default())),
        }
    }
}

/// CPU에 맞는 계산 방식을 고른다.
/// - Intel: MKL int8 (가장 빠름).
/// - 그 밖(AMD 등): CTranslate2가 Intel이 아니면 MKL을 끄고, 그러면 int8을 지원하지 않는다.
///   MKL을 켜도 int8 GEMM은 Intel 아닌 CPU에서 수십 배 느리다(문장당 수십 초).
///   그래서 MKL을 켜고 float32로 쓴다(int8 모델을 불러올 때 float32로 풀린다. 메모리는 더 쓴다).
///
/// `ANYCALL_CT2_COMPUTE`(int8 | int8_float32 | float32)로 강제로 고를 수 있다.
fn select_compute_type() -> ComputeType {
    let forced = match std::env::var("ANYCALL_CT2_COMPUTE").as_deref() {
        Ok("int8") => Some(ComputeType::INT8),
        Ok("int8_float32") => Some(ComputeType::INT8_FLOAT32),
        Ok("float32") => Some(ComputeType::FLOAT32),
        _ => None,
    };
    forced.unwrap_or(if cpu_is_intel() { ComputeType::INT8 } else { ComputeType::FLOAT32 })
}

fn cpu_is_intel() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let r = std::arch::x86_64::__cpuid(0);
        let mut v = [0u8; 12];
        v[..4].copy_from_slice(&r.ebx.to_le_bytes());
        v[4..8].copy_from_slice(&r.edx.to_le_bytes());
        v[8..].copy_from_slice(&r.ecx.to_le_bytes());
        &v == b"GenuineIntel"
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Intel이 아닌 CPU에서 CTranslate2가 MKL을 쓰도록 CT2_USE_MKL=1을 준다. main 맨 앞에서 부른다.
/// CTranslate2는 이 값을 main보다 먼저(정적 초기화 때) 읽어 두므로 실행 중에 바꿔서는 소용없다.
/// 그래서 값이 없으면 같은 인자로 자기 자신을 CT2_USE_MKL=1과 함께 다시 실행하고 그 종료 코드로 끝난다.
/// 이미 값이 있으면(직접 준 경우 포함) 그대로 둔다.
pub fn prepare_backend() {
    if cpu_is_intel() || std::env::var_os("CT2_USE_MKL").is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let status = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("CT2_USE_MKL", "1")
        .status();
    if let Ok(status) = status {
        std::process::exit(status.code().unwrap_or(1));
    }
    // 다시 실행하지 못하면 이대로 계속한다(번역 모델을 불러올 때 오류가 난다).
}
