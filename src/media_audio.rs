//! 동영상 파일의 소리를 Media Foundation Source Reader로 읽어 16kHz 모노 f32로 넘긴다.
//! 재생과 따로 같은 파일을 한 번 더 열기 때문에 재생 위치보다 앞서 읽을 수 있다.

use crate::audio::SAMPLE_RATE;
use std::sync::Once;
use windows::core::HSTRING;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
/// Media Foundation 시각 단위(100ns)
const HNS: f64 = 10_000_000.0;

/// 이 스레드에서 Media Foundation을 쓸 수 있게 한다. 여러 번 불러도 된다.
pub fn init_thread() {
    static STARTUP: Once = Once::new();
    // SAFETY: COM 초기화와 MFStartup은 어느 스레드에서 불러도 된다. 이미 초기화된 경우의 오류는 무시한다.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    STARTUP.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    });
}

pub struct MediaAudio {
    reader: IMFSourceReader,
    /// 파일 길이(초). 알 수 없으면 None
    pub duration: Option<f64>,
    channels: usize,
    rate: u32,
    resampler: Option<sherpa_onnx::LinearResampler>,
    /// 원래 소리 형식(진단용)
    pub native_format: String,
    /// 탐색한 위치. MKV 등은 그보다 앞에서 읽기 시작하므로 이 위치 전의 샘플은 버린다.
    seek_to: Option<f64>,
}

/// 읽은 소리 한 덩어리. `start`는 파일 안의 시각(초).
pub struct Chunk {
    pub start: f64,
    pub samples: Vec<f32>,
}

fn mf_err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what} ({e})")
}

impl MediaAudio {
    pub fn open(path: &str) -> Result<Self, String> {
        init_thread();
        // SAFETY: Media Foundation 호출. 인자는 모두 이 함수 안에서 만든 올바른 값이다.
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), None)
                .map_err(mf_err("파일을 열 수 없습니다"))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(mf_err("스트림 선택 실패"))?;
            reader.SetStreamSelection(AUDIO_STREAM, true).map_err(mf_err("소리 트랙이 없습니다"))?;

            let native_format = reader
                .GetNativeMediaType(AUDIO_STREAM, 0)
                .ok()
                .and_then(|t| t.GetGUID(&MF_MT_SUBTYPE).ok())
                .map(audio_subtype_name)
                .unwrap_or_else(|| "알 수 없음".into());

            // 16kHz 모노 float를 먼저 요청하고, 안 되면 원래 채널·샘플레이트의 float로 받아 직접 바꾼다.
            let want = |rate: Option<u32>, ch: Option<u32>| -> windows::core::Result<()> {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
                if let Some(r) = rate {
                    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, r)?;
                }
                if let Some(c) = ch {
                    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, c)?;
                }
                reader.SetCurrentMediaType(AUDIO_STREAM, None, &t)
            };
            if want(Some(SAMPLE_RATE), Some(1)).is_err() {
                want(None, None).map_err(|_| format!("소리 형식({native_format})을 디코딩할 수 없어 AI 자막을 만들 수 없습니다"))?;
            }
            let cur = reader.GetCurrentMediaType(AUDIO_STREAM).map_err(mf_err("소리 형식을 알 수 없습니다"))?;
            let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(mf_err("샘플레이트를 알 수 없습니다"))?;
            let channels = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(mf_err("채널 수를 알 수 없습니다"))? as usize;
            let resampler = if rate != SAMPLE_RATE {
                Some(sherpa_onnx::LinearResampler::create(rate as i32, SAMPLE_RATE as i32).ok_or("리샘플러를 만들 수 없습니다")?)
            } else {
                None
            };

            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|v| u64::try_from(&v).ok())
                .map(|d| d as f64 / HNS);

            Ok(Self { reader, duration, channels, rate, resampler, native_format, seek_to: None })
        }
    }

    /// 읽을 위치를 옮긴다(초). 다음 `read`는 이 위치(또는 바로 뒤)부터 샘플을 준다.
    pub fn seek(&mut self, secs: f64) -> Result<(), String> {
        if self.duration.is_some_and(|d| secs >= d) {
            return Err("파일 길이를 넘는 위치입니다".into());
        }
        self.seek_to = Some(secs.max(0.0));
        let pos = PROPVARIANT::from((secs.max(0.0) * HNS) as i64);
        // SAFETY: GUID_NULL은 100ns 단위 시각을 뜻한다. pos는 이 호출 동안 살아 있다.
        unsafe {
            self.reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &pos)
                .map_err(mf_err("탐색 실패"))?;
        }
        if self.resampler.is_some() {
            // 리샘플러에 남은 이전 위치의 샘플을 버린다.
            self.resampler =
                Some(sherpa_onnx::LinearResampler::create(self.rate as i32, SAMPLE_RATE as i32).ok_or("리샘플러를 만들 수 없습니다")?);
        }
        Ok(())
    }

    /// 다음 덩어리를 읽는다. 파일 끝이면 None.
    pub fn read(&mut self) -> Result<Option<Chunk>, String> {
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: 출력 포인터는 모두 이 함수의 지역 변수다.
            unsafe {
                self.reader
                    .ReadSample(AUDIO_STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))
                    .map_err(mf_err("소리 읽기 실패"))?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            let Some(sample) = sample else { continue }; // 형식 변경 알림 등
            let raw = unsafe { read_floats(&sample) }?;
            let mono: Vec<f32> = if self.channels <= 1 {
                raw
            } else {
                raw.chunks_exact(self.channels).map(|f| f.iter().sum::<f32>() / self.channels as f32).collect()
            };
            let mut samples = match &self.resampler {
                Some(r) => r.resample(&mono, false),
                None => mono,
            };
            let mut start = ts as f64 / HNS;
            if let Some(target) = self.seek_to {
                let skip = ((target - start) * SAMPLE_RATE as f64).round();
                if skip >= samples.len() as f64 {
                    continue; // 이 덩어리는 통째로 탐색 위치 앞이다.
                }
                if skip > 0.0 {
                    samples.drain(..skip as usize);
                    start = target;
                }
                self.seek_to = None;
            }
            return Ok(Some(Chunk { start, samples }));
        }
    }
}

unsafe fn read_floats(sample: &IMFSample) -> Result<Vec<f32>, String> {
    let buf = sample.ConvertToContiguousBuffer().map_err(mf_err("버퍼 읽기 실패"))?;
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    buf.Lock(&mut ptr, None, Some(&mut len)).map_err(mf_err("버퍼 잠금 실패"))?;
    let bytes = std::slice::from_raw_parts(ptr, len as usize);
    let out = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let _ = buf.Unlock();
    Ok(out)
}

fn audio_subtype_name(g: windows::core::GUID) -> String {
    let known = [
        (MFAudioFormat_AAC, "AAC"),
        (MFAudioFormat_MP3, "MP3"),
        (MFAudioFormat_Dolby_AC3, "AC-3"),
        (MFAudioFormat_Dolby_DDPlus, "E-AC-3"),
        (MFAudioFormat_DTS, "DTS"),
        (MFAudioFormat_DTS_HD, "DTS"),
        (MFAudioFormat_DTS_RAW, "DTS"),
        (MFAudioFormat_Opus, "Opus"),
        (MFAudioFormat_FLAC, "FLAC"),
        (MFAudioFormat_Vorbis, "Vorbis"),
        (MFAudioFormat_PCM, "PCM"),
        (MFAudioFormat_Float, "Float"),
        (MFAudioFormat_ALAC, "ALAC"),
    ];
    known
        .iter()
        .find(|(k, _)| *k == g)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("{g:?}"))
}

/// 파일의 영상·소리 형식(재생 오류를 설명할 때 쓴다)
#[derive(Debug, Default)]
pub struct Formats {
    pub video: Option<String>,
    pub audio: Option<String>,
    /// H.264 High 10 등 10비트 영상
    pub ten_bit: bool,
}

/// 첫 영상·소리 트랙의 형식을 읽는다. 디코딩은 하지 않는다.
pub fn probe(path: &str) -> Formats {
    init_thread();
    let mut f = Formats::default();
    // SAFETY: Media Foundation 호출. 인자는 이 함수 안에서 만든 값이다.
    unsafe {
        let Ok(reader) = MFCreateSourceReaderFromURL(&HSTRING::from(path), None) else { return f };
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        if let Ok(t) = reader.GetNativeMediaType(video, 0) {
            if let Ok(g) = t.GetGUID(&MF_MT_SUBTYPE) {
                f.video = Some(video_subtype_name(g));
                // H.264 프로필 110(High 10) 이상은 10비트다.
                f.ten_bit = g == MFVideoFormat_H264 && t.GetUINT32(&MF_MT_VIDEO_PROFILE).is_ok_and(|p| p >= 110);
            }
        }
        if let Ok(t) = reader.GetNativeMediaType(AUDIO_STREAM, 0) {
            f.audio = t.GetGUID(&MF_MT_SUBTYPE).ok().map(audio_subtype_name);
        }
    }
    f
}

fn video_subtype_name(g: windows::core::GUID) -> String {
    let known = [
        (MFVideoFormat_H264, "H.264"),
        (MFVideoFormat_HEVC, "HEVC(H.265)"),
        (MFVideoFormat_H265, "HEVC(H.265)"),
        (MFVideoFormat_VP90, "VP9"),
        (MFVideoFormat_VP80, "VP8"),
        (MFVideoFormat_AV1, "AV1"),
        (MFVideoFormat_MPEG2, "MPEG-2"),
        (MFVideoFormat_MP4V, "MPEG-4"),
        (MFVideoFormat_WVC1, "VC-1"),
        (MFVideoFormat_MJPG, "MJPEG"),
    ];
    if let Some((_, n)) = known.iter().find(|(k, _)| *k == g) {
        return (*n).to_string();
    }
    // FourCC 형식이면 네 글자로 보여 준다.
    let b = g.data1.to_le_bytes();
    if b.iter().all(|c| c.is_ascii_alphanumeric()) {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        format!("{g:?}")
    }
}

/// 재생 오류를 사람이 알아들을 말로 바꾼다.
pub fn explain_playback_error(path: &str, raw: &str) -> String {
    let f = probe(path);
    if f.audio.as_deref() == Some("DTS") {
        return "이 파일의 소리 형식(DTS)은 Windows 내장 디코더로 재생할 수 없습니다.".into();
    }
    match f.video.as_deref() {
        Some("HEVC(H.265)") => {
            "영상 형식 HEVC(H.265)를 재생할 코덱이 없습니다. Microsoft Store에서 'HEVC 비디오 확장'을 설치하면 재생됩니다.".into()
        }
        Some("H.264") if f.ten_bit => "10비트 H.264 영상은 Windows 내장 디코더로 재생할 수 없습니다.".into(),
        Some("MPEG-2") => "영상 형식 MPEG-2를 재생할 코덱이 없습니다. Microsoft Store에서 'MPEG-2 비디오 확장'을 설치하세요.".into(),
        Some("AV1") => "영상 형식 AV1을 재생할 코덱이 없습니다. Microsoft Store에서 'AV1 비디오 확장'을 설치하세요.".into(),
        Some(v) => format!("재생할 수 없습니다(영상 {v}, 소리 {}): {raw}", f.audio.as_deref().unwrap_or("없음")),
        None => raw.to_string(),
    }
}
