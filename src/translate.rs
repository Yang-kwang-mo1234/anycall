//! NLLB-200 distilled 600M (CTranslate2 int8)으로 영어·일본어를 한국어로 번역한다.

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
            compute_type: ComputeType::INT8,
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
