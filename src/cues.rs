//! 자막 큐 저장소. 자막 파일에서 읽은 큐와 AI가 만든 큐를 같은 형식으로 담는다.
//! 화면은 재생 위치로 지금 보여 줄 큐를 찾는다.

pub use crate::analyze::Cue;

/// 짧은 AI 큐는 읽기 전에 사라지지 않도록 최소한 이만큼 보여 준다(다음 큐가 오면 거기서 끝).
const MIN_SHOW: f64 = 1.5;
/// AI 큐는 말이 끝난 뒤에도 이만큼 더 보여 준다.
const HOLD: f64 = 0.6;

#[derive(Default)]
pub struct CueStore {
    /// 시작 시각 순
    cues: Vec<Cue>,
    /// AI 분석을 마친 구간(겹치지 않게 합쳐 둔다)
    done: Vec<(f64, f64)>,
    /// AI 큐면 HOLD·MIN_SHOW를 적용한다. 자막 파일 큐는 적힌 시각 그대로다.
    pub generated: bool,
}

impl CueStore {
    pub fn from_cues(mut cues: Vec<Cue>) -> Self {
        cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        Self { cues, done: Vec::new(), generated: false }
    }

    pub fn generated() -> Self {
        Self { generated: true, ..Default::default() }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// 큐를 넣는다. 탐색 뒤 다시 분석해 같은 구간이 또 오면(시작이 0.3초 안) 버린다.
    pub fn insert(&mut self, cue: Cue) {
        let i = self.cues.partition_point(|c| c.start < cue.start);
        let dup = |c: &Cue| (c.start - cue.start).abs() < 0.3;
        if self.cues.get(i).is_some_and(dup) || i > 0 && dup(&self.cues[i - 1]) {
            return;
        }
        self.cues.insert(i, cue);
    }

    /// 분석을 마친 구간을 기록한다.
    pub fn mark_done(&mut self, from: f64, to: f64) {
        if to <= from {
            return;
        }
        self.done.push((from, to));
        self.done.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for &(a, b) in &self.done {
            match merged.last_mut() {
                Some(last) if a <= last.1 + 0.05 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        self.done = merged;
    }

    /// `t`초 위치를 분석했나
    pub fn is_done(&self, t: f64) -> bool {
        self.done.iter().any(|&(a, b)| a <= t && t < b)
    }

    /// `t`초부터 분석을 마친 구간이 이어지는 끝. 분석하지 않은 위치면 None
    pub fn done_until(&self, t: f64) -> Option<f64> {
        self.done.iter().find(|&&(a, b)| a <= t && t < b).map(|&(_, b)| b)
    }

    /// `t`초에 보여 줄 큐(시작 순)
    pub fn active(&self, t: f64) -> Vec<&Cue> {
        let end = self.cues.partition_point(|c| c.start <= t);
        let mut out = Vec::new();
        // 앞쪽으로 거슬러 가며 아직 끝나지 않은 큐를 모은다(자막 파일은 큐가 겹칠 수 있다).
        for i in (0..end).rev() {
            let c = &self.cues[i];
            if self.shown_until(i) > t {
                out.push(c);
            }
            if t - c.start > 30.0 {
                break;
            }
        }
        out.reverse();
        if self.generated && out.len() > 2 {
            out.drain(..out.len() - 2); // AI 자막은 최근 두 줄만
        }
        out
    }

    fn shown_until(&self, i: usize) -> f64 {
        let c = &self.cues[i];
        if !self.generated {
            return c.end;
        }
        let until = (c.end + HOLD).max(c.start + MIN_SHOW);
        match self.cues.get(i + 1) {
            // 다음 큐가 나오면 이전 큐는 한 줄 위로 잠깐 남았다가(HOLD) 사라진다.
            Some(next) => until.min(next.start.max(c.end) + HOLD),
            None => until,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64) -> Cue {
        Cue { start, end, src: String::new(), ko: format!("{start}") }
    }

    #[test]
    fn file_cues_follow_their_times() {
        let s = CueStore::from_cues(vec![cue(5.0, 7.0), cue(1.0, 3.0)]);
        assert!(s.active(0.5).is_empty());
        assert_eq!(s.active(2.0).len(), 1);
        assert!(s.active(3.5).is_empty());
        assert_eq!(s.active(6.9)[0].start, 5.0);
    }

    #[test]
    fn generated_cues_hold_and_dedupe() {
        let mut s = CueStore::generated();
        s.insert(cue(1.0, 1.4));
        s.insert(cue(1.1, 1.5)); // 같은 구간을 다시 분석한 것
        assert_eq!(s.len(), 1);
        assert_eq!(s.active(2.3).len(), 1); // 최소 1.5초 보여 준다
        assert!(s.active(2.6).is_empty());
    }

    #[test]
    fn done_ranges_merge() {
        let mut s = CueStore::generated();
        s.mark_done(0.0, 10.0);
        s.mark_done(30.0, 40.0);
        s.mark_done(9.0, 31.0);
        assert!(s.is_done(20.0));
        assert_eq!(s.done_until(5.0), Some(40.0));
        assert_eq!(s.done_until(45.0), None);
    }
}
