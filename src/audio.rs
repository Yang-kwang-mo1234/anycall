//! 소리 입력. 기본 출력 장치의 WASAPI loopback, 또는 시험용 wav 파일.
//! 어느 쪽이든 16kHz 모노 f32 샘플을 조각으로 넘긴다.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

pub const SAMPLE_RATE: u32 = 16000;
/// 재생 중인 소리가 없으면 loopback에는 이벤트가 오지 않는다. 이만큼 기다려도 없으면 무음을 넘겨
/// 말하던 구간이 끝나도록 한다.
const IDLE_MS: u32 = 200;

pub fn capture_loopback(stop: &AtomicBool, mut on_samples: impl FnMut(Vec<f32>)) -> Result<(), String> {
    let _ = wasapi::initialize_mta();
    let device = DeviceEnumerator::new()
        .and_then(|e| e.get_default_device(Direction::Render))
        .map_err(|e| format!("출력 장치를 찾을 수 없습니다 ({e})"))?;
    let open_err = |e: wasapi::WasapiError| format!("출력 장치 loopback을 열 수 없습니다 ({e})");
    let mut client = device.get_iaudioclient().map_err(open_err)?;
    // autoconvert로 Windows가 16kHz 모노로 바꿔 준다. 따로 리샘플하지 않는다.
    let format = WaveFormat::new(32, 32, SampleType::Float, SAMPLE_RATE as usize, 1, None);
    let (period, _) = client.get_device_period().map_err(open_err)?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: period,
    };
    // Render 장치를 Capture 방향으로 열면 loopback이 된다.
    client
        .initialize_client(&format, Direction::Capture, mode, None)
        .map_err(open_err)?;
    let event = client.set_get_eventhandle().map_err(open_err)?;
    let capture = client.get_audiocaptureclient().map_err(open_err)?;
    client.start_stream().map_err(open_err)?;

    let mut queue = VecDeque::<u8>::new();
    let read_err = |e: wasapi::WasapiError| format!("소리 입력 오류 ({e})");
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(IDLE_MS).is_err() {
            on_samples(vec![0.0; (SAMPLE_RATE * IDLE_MS / 1000) as usize]);
            continue;
        }
        while capture.get_next_packet_size().map_err(read_err)?.unwrap_or(0) > 0 {
            capture.read_from_device_to_deque(&mut queue).map_err(read_err)?;
        }
        let n = queue.len() / 4 * 4;
        let samples: Vec<f32> = queue
            .drain(..n)
            .collect::<Vec<u8>>()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if !samples.is_empty() {
            on_samples(samples);
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// wav 파일을 실제 재생 속도로 흘려 보낸다(지연 측정용). 끝나면 무음 1초를 더 보낸다.
pub fn play_wav(path: &str, stop: &AtomicBool, mut on_samples: impl FnMut(Vec<f32>)) -> Result<(), String> {
    let wave = sherpa_onnx::Wave::read(path).ok_or_else(|| format!("wav 파일을 읽을 수 없습니다: {path}"))?;
    let mut samples = wave.samples().to_vec();
    if wave.sample_rate() != SAMPLE_RATE as i32 {
        let r = sherpa_onnx::LinearResampler::create(wave.sample_rate(), SAMPLE_RATE as i32)
            .ok_or("리샘플러를 만들 수 없습니다")?;
        samples = r.resample(&samples, true);
    }
    samples.extend(std::iter::repeat_n(0.0, SAMPLE_RATE as usize));
    const CHUNK: usize = SAMPLE_RATE as usize / 50; // 20ms
    let start = Instant::now();
    for (i, chunk) in samples.chunks(CHUNK).enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let due = start + Duration::from_millis(20 * (i as u64 + 1));
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        on_samples(chunk.to_vec());
    }
    Ok(())
}
