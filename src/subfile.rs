//! 자막 파일: 동영상 옆의 자막 파일 찾기, 글자 인코딩 판별, SRT·SMI·VTT·ASS 읽기.

use crate::analyze::Cue;
use std::path::{Path, PathBuf};

const EXTS: &[&str] = &["srt", "smi", "sami", "vtt", "ass", "ssa"];
/// 자막에 맞는 동영상을 찾을 때 보는 확장자
const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "webm", "avi", "mov", "m4v", "wmv", "ts", "m2ts", "mpg", "mpeg", "flv"];
const KOREAN_TAGS: &[&str] = &["ko", "kor", "korean", "kr", "ko-kr", "한국어", "한글"];

/// 찾은 자막 파일
pub struct Found {
    pub path: PathBuf,
    /// 파일 이름의 언어 표시(`영화.en.srt`면 "en"). 없으면 None
    pub tag: Option<String>,
}

impl Found {
    pub fn is_korean(&self) -> bool {
        self.tag.as_deref().is_none_or(|t| KOREAN_TAGS.contains(&t.to_lowercase().as_str()))
    }
}

/// 동영상과 같은 폴더에서 같은 이름의 자막 파일을 찾는다.
/// 순서: 한국어 표시(`.ko.` 등) → 언어 표시 없음 → 그 밖의 언어 표시.
pub fn find(video: &Path) -> Option<Found> {
    let dir = video.parent()?;
    let stem = video.file_stem()?.to_string_lossy().to_lowercase();
    let mut candidates: Vec<(u8, Found)> = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_lowercase()) else { continue };
        if !EXTS.contains(&ext.as_str()) {
            continue;
        }
        let name = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let tag = if name == stem {
            None
        } else if let Some(rest) = name.strip_prefix(&format!("{stem}.")) {
            Some(rest.to_string())
        } else {
            continue;
        };
        let found = Found { path, tag };
        let rank = match &found.tag {
            Some(_) if found.is_korean() => 0,
            None => 1,
            Some(_) => 2,
        };
        // 같은 순위면 SRT·SMI를 ASS보다 앞에 둔다(ASS는 효과 태그를 버리므로).
        let ext_rank = EXTS.iter().position(|e| *e == ext).unwrap_or(9) as u8;
        candidates.push((rank * 10 + ext_rank, found));
    }
    candidates.sort_by_key(|(r, _)| *r);
    candidates.into_iter().next().map(|(_, f)| f)
}

/// 자막 파일을 읽어 큐로 만든다. 글은 `ko` 칸에 넣는다(한국어가 아닌 자막도 그대로 보여 준다).
pub fn load(path: &Path) -> Result<Vec<Cue>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("자막 파일을 읽을 수 없습니다: {e}"))?;
    let text = decode_text(&bytes);
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let lower_head: String = text.chars().take(400).collect::<String>().to_lowercase();
    let cues = if ext == "smi" || ext == "sami" || lower_head.contains("<sami") {
        parse_smi(&text)
    } else if ext == "ass" || ext == "ssa" || lower_head.contains("[script info]") {
        parse_ass(&text)
    } else {
        parse_srt_like(&text) // SRT와 WebVTT
    };
    if cues.is_empty() {
        return Err("자막 파일에서 자막을 찾지 못했습니다".into());
    }
    Ok(cues)
}

/// BOM이 있으면 그대로, 없으면 UTF-8로 읽어 보고 안 되면 CP949로 읽는다.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom_len..]).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::EUC_KR.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

fn cue(start: f64, end: f64, text: String) -> Option<Cue> {
    let text = text.trim().to_string();
    (end > start && !text.is_empty()).then(|| Cue { start, end, src: String::new(), ko: text })
}

/// `00:01:02,345`, `01:02.345`, `0:01:02.34`(ASS) 같은 시각을 초로 바꾼다.
fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (h.trim().parse::<f64>().ok()?, m.trim().parse::<f64>().ok()?, s.trim().parse::<f64>().ok()?),
        [m, s] => (0.0, m.trim().parse::<f64>().ok()?, s.trim().parse::<f64>().ok()?),
        _ => return None,
    };
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// SRT와 WebVTT. 시각 줄(`A --> B`) 다음 빈 줄까지가 글이다.
fn parse_srt_like(text: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some((a, b)) = line.split_once("-->") {
            // VTT는 시각 뒤에 위치 설정이 올 수 있다(`00:01.000 --> 00:02.000 align:start`).
            let b = b.split_whitespace().next().unwrap_or("");
            if let (Some(start), Some(end)) = (parse_time(a), parse_time(b)) {
                let mut body = Vec::new();
                i += 1;
                while i < lines.len() && !lines[i].trim().is_empty() {
                    body.push(strip_tags(lines[i].trim()));
                    i += 1;
                }
                out.extend(cue(start, end, body.join("\n")));
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `<i>`, `<font ...>` 같은 태그와 `{\an8}` 같은 ASS식 표시를 지운다.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth_angle = false;
    let mut depth_brace = false;
    for c in s.chars() {
        match c {
            '<' => depth_angle = true,
            '>' if depth_angle => depth_angle = false,
            '{' => depth_brace = true,
            '}' if depth_brace => depth_brace = false,
            _ if depth_angle || depth_brace => {}
            _ => out.push(c),
        }
    }
    decode_entities(&out)
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// ASS/SSA: [Events]의 Format 줄로 칸 순서를 알고, Dialogue 줄에서 시작·끝·글을 뽑는다.
fn parse_ass(text: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    let mut in_events = false;
    let mut fields: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(f) = line.strip_prefix("Format:") {
            fields = f.split(',').map(|s| s.trim().to_lowercase()).collect();
        } else if let Some(d) = line.strip_prefix("Dialogue:") {
            if fields.is_empty() {
                fields = ["layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
            // 글(마지막 칸)에는 쉼표가 들어갈 수 있으므로 칸 수만큼만 나눈다.
            let parts: Vec<&str> = d.trim_start().splitn(fields.len(), ',').collect();
            let get = |name: &str| fields.iter().position(|f| f == name).and_then(|i| parts.get(i)).copied();
            let (Some(s), Some(e), Some(t)) = (get("start"), get("end"), get("text")) else { continue };
            let (Some(start), Some(end)) = (parse_time(s), parse_time(e)) else { continue };
            let body = t.replace("\\N", "\n").replace("\\n", "\n").replace("\\h", " ");
            out.extend(cue(start, end, strip_tags(&body)));
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// SAMI(SMI): `<SYNC Start=ms><P Class=KRCC>글`. 같은 언어 칸의 다음 SYNC가 오면 앞 자막이 끝난다.
/// 언어 칸이 여럿이면 한국어(KRCC, KOKR 등)를 고른다.
fn parse_smi(text: &str) -> Vec<Cue> {
    let lower = text.to_lowercase();
    // (시작 ms, 칸 이름, 글)
    let mut syncs: Vec<(f64, String, String)> = Vec::new();
    let mut pos = 0;
    while let Some(off) = lower[pos..].find("<sync") {
        let at = pos + off;
        let next = lower[at + 5..].find("<sync").map(|n| at + 5 + n).unwrap_or(lower.len());
        let tag_end = lower[at..next].find('>').map(|n| at + n + 1).unwrap_or(next);
        let start = attr(&lower[at..tag_end], "start").and_then(|v| v.parse::<f64>().ok());
        let body = &text[tag_end..next];
        let body_lower = &lower[tag_end..next];
        let class = body_lower
            .find("<p")
            .and_then(|p| {
                let end = body_lower[p..].find('>').map(|n| p + n)?;
                attr(&body_lower[p..end], "class")
            })
            .unwrap_or_default();
        let content = match body_lower.find("<p") {
            Some(p) => match body_lower[p..].find('>') {
                Some(n) => &body[p + n + 1..],
                None => body,
            },
            None => body,
        };
        let content = content.replace("<br>", "\n").replace("<BR>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
        let content = strip_tags(&content);
        let content: Vec<&str> = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        if let Some(s) = start {
            syncs.push((s / 1000.0, class, content.join("\n")));
        }
        pos = next;
    }
    let classes: Vec<String> = {
        let mut c: Vec<String> = syncs.iter().map(|s| s.1.clone()).collect();
        c.dedup();
        c.sort();
        c.dedup();
        c
    };
    let pick = classes
        .iter()
        .find(|c| c.starts_with("kr") || c.starts_with("ko"))
        .or_else(|| classes.first())
        .cloned()
        .unwrap_or_default();
    let chosen: Vec<&(f64, String, String)> = syncs.iter().filter(|s| s.1 == pick || s.1.is_empty()).collect();
    let mut out = Vec::new();
    for (i, (start, _, text)) in chosen.iter().enumerate() {
        let end = chosen.get(i + 1).map(|n| n.0).unwrap_or(start + 5.0);
        out.extend(cue(*start, end, text.clone()));
    }
    out
}

/// `start=1234`, `class="KRCC"` 같은 속성 값
fn attr(tag: &str, name: &str) -> Option<String> {
    let i = tag.find(&format!("{name}="))? + name.len() + 1;
    let rest = tag[i..].trim_start_matches(['"', '\'']);
    let v: String = rest.chars().take_while(|c| !matches!(c, '"' | '\'' | ' ' | '>')).collect();
    (!v.is_empty()).then_some(v)
}

/// 자막 파일 옆에서 그 자막에 맞는 동영상을 찾는다(자막 파일을 실행하거나 끌어다 놓을 때).
/// `영화.srt`, `영화.ko.srt`, `영화.en.forced.srt`는 모두 `영화.mp4`에 맞는다.
/// 이름이 맞는 동영상이 여럿이면 이름이 가장 긴 것(`영화.part2.mp4`가 `영화.mp4`보다 앞)을 고른다.
pub fn find_video(sub: &Path) -> Option<PathBuf> {
    let dir = sub.parent()?;
    let sub_stem = sub.file_stem()?.to_string_lossy().to_lowercase();
    let videos = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| {
        p.is_file() && p.extension().is_some_and(|e| VIDEO_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
    });
    best_video(&sub_stem, videos)
}

fn best_video(sub_stem: &str, videos: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    videos
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().to_lowercase();
            (sub_stem == stem || sub_stem.starts_with(&format!("{stem}."))).then_some((stem.len(), p))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

/// 자막 파일 확장자인가(끌어다 놓을 때 동영상과 구분한다)
pub fn is_subtitle_ext(ext: &str) -> bool {
    EXTS.contains(&ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_for_subtitle() {
        let vids = || ["영화.mp4", "영화.part2.mkv", "다른.mp4"].into_iter().map(|n| PathBuf::from(format!("d/{n}")));
        assert_eq!(best_video("영화", vids()), Some(PathBuf::from("d/영화.mp4")));
        assert_eq!(best_video("영화.ko", vids()), Some(PathBuf::from("d/영화.mp4")));
        assert_eq!(best_video("영화.part2.ko", vids()), Some(PathBuf::from("d/영화.part2.mkv")));
        assert_eq!(best_video("영화2", vids()), None);
        assert_eq!(best_video("없음", vids()), None);
    }

    #[test]
    fn srt_with_tags() {
        let s = "1\r\n00:00:01,500 --> 00:00:03,000\r\n<i>안녕</i>하세요\r\n두 번째 줄\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,250\r\n{\\an8}위쪽\r\n";
        let c = parse_srt_like(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].start, 1.5);
        assert_eq!(c[0].ko, "안녕하세요\n두 번째 줄");
        assert_eq!(c[1].ko, "위쪽");
        assert_eq!(c[1].end, 5.25);
    }

    #[test]
    fn vtt_short_times_and_settings() {
        let s = "WEBVTT\n\nNOTE 설명\n\n01:02.000 --> 01:04.500 align:start\n안녕\n";
        let c = parse_srt_like(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].start, 62.0);
        assert_eq!(c[0].end, 64.5);
    }

    #[test]
    fn ass_dialogue_with_commas() {
        let s = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:02.50,0:00:04.00,Default,,0,0,0,,{\\i1}하나, 둘\\N셋\n";
        let c = parse_ass(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].start, 2.5);
        assert_eq!(c[0].ko, "하나, 둘\n셋");
    }

    #[test]
    fn smi_picks_korean_class_and_ends_at_next_sync() {
        let s = "<SAMI><BODY>\n<SYNC Start=1000><P Class=ENCC>Hello\n<SYNC Start=1000><P Class=KRCC>안녕<br>하세요\n<SYNC Start=3000><P Class=KRCC>&nbsp;\n<SYNC Start=4000><P Class=KRCC>다음\n</BODY></SAMI>";
        let c = parse_smi(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].ko, "안녕\n하세요");
        assert_eq!((c[0].start, c[0].end), (1.0, 3.0));
        assert_eq!(c[1].start, 4.0);
    }

    #[test]
    fn cp949_and_bom() {
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("안녕하세요");
        assert_eq!(decode_text(&bytes), "안녕하세요");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend("자막".as_bytes());
        assert_eq!(decode_text(&bom), "자막");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("한".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_text(&utf16), "한");
    }

    #[test]
    fn find_prefers_korean() {
        let dir = std::env::temp_dir().join(format!("anycall-subfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["영화.mp4", "영화.en.srt", "영화.srt", "영화.ko.smi", "다른.srt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let f = find(&dir.join("영화.mp4")).unwrap();
        assert_eq!(f.path.file_name().unwrap(), "영화.ko.smi");
        std::fs::remove_file(dir.join("영화.ko.smi")).unwrap();
        assert_eq!(find(&dir.join("영화.mp4")).unwrap().path.file_name().unwrap(), "영화.srt");
        std::fs::remove_file(dir.join("영화.srt")).unwrap();
        let f = find(&dir.join("영화.mp4")).unwrap();
        assert_eq!(f.tag.as_deref(), Some("en"));
        assert!(!f.is_korean());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
