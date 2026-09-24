//! Progress for a benchmark run, on stderr.
//!
//! A long comparison — ten minutes of cold installs — is otherwise silent
//! until it finishes. On a terminal this draws one bar that redraws in place.
//! Anywhere else (CI logs) it prints a plain line at most every tenth of the
//! way or every 30 seconds, since a carriage-return animation turns into
//! thousands of lines in a log.
//!
//! The time remaining is estimated per subject: each subject's average sample
//! so far, prepare included, times the samples it still has to take. One
//! average over everything would be badly wrong exactly when it matters —
//! when a 20-second cold install shares a run with a 300ms one.

use crate::measure::Observer;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often a non-terminal prints, at most, when no tenth has been crossed.
const LOG_EVERY: Duration = Duration::from_secs(30);

/// How often the ticker wakes. A terminal bar redraws its elapsed time this
/// often while a sample runs; a log checks whether LOG_EVERY has passed.
const TICK: Duration = Duration::from_secs(1);

/// The progress reporter. Samples can run for minutes — a cold install, or a
/// prepare step that re-downloads everything — so a ticker thread keeps the
/// output alive between them rather than only when one finishes.
pub struct Bar {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl Bar {
    pub fn new(label: &str, names: Vec<String>) -> Self {
        let state = Arc::new(Mutex::new(State::new(label, names)));
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let (state, stop) = (Arc::clone(&state), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::park_timeout(TICK);
                    if !stop.load(Ordering::Relaxed) {
                        lock(&state).tick();
                    }
                }
            })
        };
        Bar {
            state,
            stop,
            ticker: Some(ticker),
        }
    }

    /// Stop the ticker and clear the bar so results print on a clean line.
    /// Call once the run is over, whatever its outcome.
    pub fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            t.thread().unpark();
            t.join().ok();
        }
        lock(&self.state).clear();
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.finish();
    }
}

/// A panic elsewhere must not also take the progress output down with it.
fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Observer for Bar {
    fn planned(&mut self, remaining: &[u64]) {
        lock(&self.state).planned(remaining);
    }
    fn started(&mut self, subject: usize) {
        lock(&self.state).started(subject);
    }
    fn finished(&mut self, subject: usize, elapsed: Duration) {
        lock(&self.state).finished(subject, elapsed);
    }
    fn dropped(&mut self, subject: usize) {
        lock(&self.state).dropped(subject);
    }
}

struct State {
    label: String,
    names: Vec<String>,
    remaining: Vec<u64>,
    /// Per subject: total time and count of finished samples.
    spent: Vec<(Duration, u32)>,
    done: u64,
    current: Option<usize>,
    started: Instant,
    tty: bool,
    last_log: Instant,
    last_tenth: u64,
    drawn: bool,
}

impl State {
    fn new(label: &str, names: Vec<String>) -> Self {
        let n = names.len();
        let now = Instant::now();
        State {
            label: label.to_string(),
            names,
            remaining: vec![0; n],
            spent: vec![(Duration::ZERO, 0); n],
            done: 0,
            current: None,
            started: now,
            tty: std::io::stderr().is_terminal(),
            last_log: now,
            last_tenth: 0,
            drawn: false,
        }
    }

    fn total(&self) -> u64 {
        self.done + self.remaining.iter().sum::<u64>()
    }

    /// Estimated time left, or `None` until anything has been timed.
    fn eta(&self) -> Option<Duration> {
        let (all, n) = self
            .spent
            .iter()
            .fold((Duration::ZERO, 0u32), |(t, c), &(d, k)| (t + d, c + k));
        if n == 0 {
            return None;
        }
        // A subject not yet timed is guessed at the average of those that
        // were, which is replaced by its own the moment it has one.
        let fallback = all / n;
        let secs: f64 = self
            .remaining
            .iter()
            .zip(&self.spent)
            .map(|(&left, &(d, k))| {
                let each = if k == 0 { fallback } else { d / k };
                each.as_secs_f64() * left as f64
            })
            .sum();
        Some(Duration::from_secs_f64(secs))
    }

    fn line(&self, width: usize) -> String {
        let total = self.total().max(1);
        let pct = self.done * 100 / total;
        let eta = if self.remaining.iter().all(|&r| r == 0) && self.done > 0 {
            "done".to_string()
        } else {
            self.eta()
                .map_or_else(|| "estimating".to_string(), |d| format!("~{} left", fmt(d)))
        };
        let who = self.current.map_or("", |i| self.names[i].as_str());
        let tail = format!(
            " {}/{} {pct:>3}%  {} elapsed, {eta}  {who}",
            self.done,
            total,
            fmt(self.started.elapsed())
        );
        if !self.tty {
            return format!("  {}:{tail}", self.label);
        }
        // Whatever width is left for the bar itself, within reason.
        let room = width.saturating_sub(self.label.len() + tail.len() + 6);
        let cells = room.clamp(10, 30);
        let filled = (cells as u64 * self.done / total) as usize;
        let line = format!(
            "  {} [{}{}]{tail}",
            self.label,
            "#".repeat(filled),
            "-".repeat(cells - filled)
        );
        line.chars().take(width.saturating_sub(1)).collect()
    }

    fn draw(&mut self, force: bool) {
        if self.tty {
            let width = std::env::var("COLUMNS")
                .ok()
                .and_then(|c| c.parse().ok())
                .unwrap_or(80);
            eprint!("\r\x1b[2K{}", self.line(width));
            std::io::stderr().flush().ok();
            self.drawn = true;
            return;
        }
        let tenth = self.done * 10 / self.total().max(1);
        if force || tenth > self.last_tenth || self.last_log.elapsed() >= LOG_EVERY {
            eprintln!("{}", self.line(usize::MAX));
            self.last_tenth = tenth;
            self.last_log = Instant::now();
        }
    }

    /// Called by the ticker between events: redraw a terminal bar so its
    /// elapsed time keeps moving, and give a log its periodic line even while
    /// one long sample is still running.
    fn tick(&mut self) {
        if self.total() == 0 {
            return;
        }
        if self.tty {
            if self.drawn {
                self.draw(false);
            }
        } else if self.last_log.elapsed() >= LOG_EVERY {
            self.draw(true);
        }
    }

    fn clear(&mut self) {
        if self.tty && self.drawn {
            eprint!("\r\x1b[2K");
            std::io::stderr().flush().ok();
            self.drawn = false;
        }
    }
}

impl Observer for State {
    fn planned(&mut self, remaining: &[u64]) {
        self.remaining = remaining.to_vec();
        self.draw(false);
    }

    fn started(&mut self, subject: usize) {
        self.current = Some(subject);
        if self.tty {
            self.draw(false);
        }
    }

    fn finished(&mut self, subject: usize, elapsed: Duration) {
        self.remaining[subject] = self.remaining[subject].saturating_sub(1);
        let (d, k) = &mut self.spent[subject];
        *d += elapsed;
        *k += 1;
        self.done += 1;
        self.current = None;
        let last = self.remaining.iter().all(|&r| r == 0);
        self.draw(last);
    }

    fn dropped(&mut self, subject: usize) {
        self.remaining[subject] = 0;
        self.current = None;
        self.draw(false);
    }
}

/// `850ms`, `42s`, `3m05s`, `1h02m`.
pub fn fmt(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{}ms", d.as_millis()),
        1..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(names: &[&str]) -> State {
        let mut b = State::new("b", names.iter().map(|s| s.to_string()).collect());
        b.tty = false;
        b.last_log = Instant::now();
        b
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(fmt(Duration::from_millis(850)), "850ms");
        assert_eq!(fmt(Duration::from_secs(42)), "42s");
        assert_eq!(fmt(Duration::from_secs(185)), "3m05s");
        assert_eq!(fmt(Duration::from_secs(3720)), "1h02m");
    }

    /// The estimate prices each subject's remaining samples at its own speed,
    /// so a slow subject's backlog is not averaged away by a fast one.
    #[test]
    fn the_estimate_is_per_subject() {
        let mut b = bar(&["fast", "slow"]);
        b.planned(&[11, 6]);
        b.finished(0, Duration::from_millis(100));
        b.finished(1, Duration::from_secs(20));
        // 10 x 100ms + 5 x 20s
        let eta = b.eta().unwrap().as_secs_f64();
        assert!((eta - 101.0).abs() < 0.01, "{eta}");
    }

    #[test]
    fn an_untimed_subject_is_guessed_at_the_average() {
        let mut b = bar(&["a", "b"]);
        b.planned(&[2, 3]);
        assert_eq!(b.eta(), None, "nothing timed yet");
        b.finished(0, Duration::from_secs(2));
        // 1 x 2s for a, 3 x 2s for b until b has a time of its own
        assert_eq!(b.eta(), Some(Duration::from_secs(8)));
    }

    #[test]
    fn a_dropped_subject_leaves_the_estimate() {
        let mut b = bar(&["a", "b"]);
        b.planned(&[2, 5]);
        b.finished(0, Duration::from_secs(1));
        b.finished(1, Duration::from_secs(10));
        b.dropped(1);
        assert_eq!(b.eta(), Some(Duration::from_secs(1)));
        assert_eq!(b.total(), 3);
    }

    #[test]
    fn the_bar_fits_the_terminal() {
        let mut b = bar(&["aube"]);
        b.tty = true;
        b.planned(&[40]);
        for _ in 0..10 {
            b.finished(0, Duration::from_millis(250));
        }
        let line = b.line(80);
        assert!(line.chars().count() < 80, "{line}");
        assert!(line.contains("10/40") && line.contains("25%"), "{line}");
        assert!(line.contains("~7s left"), "{line}");
    }

    /// A log gets a line every LOG_EVERY even when no sample has finished —
    /// the case of one very long prepare step or sample.
    #[test]
    fn a_log_hears_from_a_long_sample_before_it_finishes() {
        let mut b = bar(&["slow"]);
        b.planned(&[5]);
        b.started(0);
        let before = b.last_log;
        b.tick();
        assert_eq!(b.last_log, before, "too soon to log again");
        b.last_log = Instant::now() - LOG_EVERY;
        b.tick();
        assert!(b.last_log > before, "the ticker logged mid-sample");
    }

    /// The ticker thread starts and stops cleanly, and finishing twice (the
    /// explicit call, then Drop) is harmless.
    #[test]
    fn the_ticker_stops_when_finished() {
        let mut b = Bar::new("b", vec!["a".into()]);
        b.planned(&[1]);
        b.finished(0, Duration::from_millis(1));
        b.finish();
        assert!(b.ticker.is_none());
    }
}
