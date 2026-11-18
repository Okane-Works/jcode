//! Shape bash output before it enters model context.
//!
//! Most command output is noise to an agent: ANSI color codes, carriage-return
//! progress bars, repeated lines, and the middle of long logs. This module
//! applies the conventions most agent harnesses converge on:
//!
//! 1. Strip ANSI escape sequences and collapse `\r` progress redraws.
//! 2. Fold runs of identical lines into one line plus a repeat count.
//! 3. When the result is still over budget, keep the head and the (larger)
//!    tail, since failures and summaries usually print last, and surface
//!    error-looking lines from the elided middle.
//! 4. Spill the full raw output to a scratch file and name its path so the
//!    agent can `rg`/`sed -n` into it instead of re-running the command.
//!
//! Budget defaults to [`DEFAULT_MAX_CHARS`] and can be tuned per process with
//! `JCODE_BASH_OUTPUT_MAX_CHARS` (0 restores legacy head-only truncation at
//! the old 30k cap).

use regex::Regex;
use std::sync::LazyLock;

pub const DEFAULT_MAX_CHARS: usize = 12_000;
const LEGACY_MAX_CHARS: usize = 30_000;
const MIN_MAX_CHARS: usize = 2_000;
const MAX_MIDDLE_SIGNAL_LINES: usize = 20;
const MAX_SIGNAL_LINE_CHARS: usize = 300;
const REPEAT_FOLD_MIN: usize = 3;

static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    // CSI sequences, OSC sequences (BEL or ST terminated), and lone 2-byte escapes.
    Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
        .expect("ansi regex")
});

static SIGNAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(error|failed|failure|panic(ked)?|fatal|exception|traceback|warning|denied|not found|assert)")
        .expect("signal regex")
});

/// Configured character budget, or `None` for legacy behavior.
pub fn configured_max_chars() -> Option<usize> {
    match std::env::var("JCODE_BASH_OUTPUT_MAX_CHARS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        Some(0) => None,
        Some(n) => Some(n.max(MIN_MAX_CHARS)),
        None => Some(DEFAULT_MAX_CHARS),
    }
}

/// Remove ANSI escapes and resolve carriage-return redraws per line.
pub fn clean(raw: &str) -> String {
    let stripped = ANSI_RE.replace_all(raw, "");
    let mut out = String::with_capacity(stripped.len());
    for (i, line) in stripped.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // A trailing \r (CRLF) is not a redraw.
        let line = line.strip_suffix('\r').unwrap_or(line);
        let visible = line.rsplit('\r').find(|seg| !seg.is_empty()).unwrap_or("");
        out.push_str(visible);
    }
    out
}

/// Fold runs of `REPEAT_FOLD_MIN`+ identical consecutive lines.
pub fn fold_repeats(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j] == lines[i] {
            j += 1;
        }
        let run = j - i;
        out.push(lines[i].to_string());
        if run >= REPEAT_FOLD_MIN && !lines[i].trim().is_empty() {
            out.push(format!("... (previous line repeated {} more times)", run - 1));
        } else {
            for _ in 1..run {
                out.push(lines[i].to_string());
            }
        }
        i = j;
    }
    out.join("\n")
}

fn floor_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Result of shaping: the text to show the model.
pub struct Shaped {
    pub text: String,
    pub elided: bool,
}

/// Shape `raw` to `max_chars`. `spill` is called with the raw output when
/// elision happens and returns a path the full output was saved to.
pub fn shape(raw: &str, max_chars: usize, spill: impl FnOnce(&str) -> Option<String>) -> Shaped {
    let cleaned = fold_repeats(&clean(raw));
    if cleaned.len() <= max_chars {
        return Shaped {
            text: cleaned,
            elided: false,
        };
    }

    let head_budget = max_chars * 3 / 10;
    let tail_budget = max_chars * 6 / 10;
    // Cut on line boundaries where possible.
    let head_end = {
        let b = floor_boundary(&cleaned, head_budget);
        cleaned[..b].rfind('\n').map(|p| p + 1).filter(|p| *p > head_budget / 2).unwrap_or(b)
    };
    let tail_start = {
        let b = ceil_boundary(&cleaned, cleaned.len().saturating_sub(tail_budget));
        cleaned[b..]
            .find('\n')
            .map(|p| b + p + 1)
            .filter(|p| cleaned.len() - *p > tail_budget / 2)
            .unwrap_or(b)
    };
    let middle = &cleaned[head_end..tail_start.max(head_end)];
    let middle_lines = middle.lines().count();

    let signals: Vec<String> = middle
        .lines()
        .filter(|l| SIGNAL_RE.is_match(l))
        .take(MAX_MIDDLE_SIGNAL_LINES)
        .map(|l| {
            let end = floor_boundary(l, MAX_SIGNAL_LINE_CHARS);
            if end < l.len() { format!("{}…", &l[..end]) } else { l.to_string() }
        })
        .collect();

    let path = spill(raw);
    let mut text = String::with_capacity(max_chars + 512);
    text.push_str(&cleaned[..head_end]);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "\n... [{} lines / {} chars elided",
        middle_lines,
        middle.len()
    ));
    match &path {
        Some(p) => text.push_str(&format!("; full output saved to {p} (use rg/sed -n/tail on it instead of re-running)]\n")),
        None => text.push_str("]\n"),
    }
    if !signals.is_empty() {
        text.push_str("... [error/warning lines from the elided section:]\n");
        for s in &signals {
            text.push_str(s);
            text.push('\n');
        }
    }
    text.push('\n');
    text.push_str(&cleaned[tail_start.max(head_end)..]);
    Shaped { text, elided: true }
}

/// Legacy head-only cut, used when shaping is disabled.
pub fn legacy_truncate(mut output: String) -> String {
    if output.len() > LEGACY_MAX_CHARS {
        let end = floor_boundary(&output, LEGACY_MAX_CHARS);
        output.truncate(end);
        output.push_str("\n... (output truncated)");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_progress_redraws() {
        let raw = "\x1b[32mok\x1b[0m\n 10%\r 50%\r100% done\r\nnext";
        assert_eq!(clean(raw), "ok\n100% done\nnext");
    }

    #[test]
    fn folds_repeated_lines() {
        let raw = "a\nb\nb\nb\nb\nc\nd\nd";
        assert_eq!(fold_repeats(raw), "a\nb\n... (previous line repeated 3 more times)\nc\nd\nd");
    }

    #[test]
    fn small_output_untouched() {
        let s = shape("hello\nworld", 2000, |_| panic!("no spill"));
        assert!(!s.elided);
        assert_eq!(s.text, "hello\nworld");
    }

    #[test]
    fn large_output_keeps_head_tail_signals_and_spills() {
        let mut raw = String::from("HEADLINE\n");
        for i in 0..2000 {
            raw.push_str(&format!("compiling crate_{i} v1.0\n"));
            if i == 1000 {
                raw.push_str("error[E0308]: mismatched types in middle\n");
            }
        }
        raw.push_str("FINAL: test result: FAILED");
        let s = shape(&raw, 4000, |full| {
            assert!(full.contains("crate_1500"));
            Some("/tmp/x.log".into())
        });
        assert!(s.elided);
        assert!(s.text.starts_with("HEADLINE"));
        assert!(s.text.ends_with("FINAL: test result: FAILED"));
        assert!(s.text.contains("mismatched types in middle"));
        assert!(s.text.contains("/tmp/x.log"));
        assert!(s.text.len() < 4000 + 800, "len {}", s.text.len());
    }

    #[test]
    fn utf8_safe_cuts() {
        let raw = "é".repeat(10_000);
        let s = shape(&raw, 2000, |_| None);
        assert!(s.elided);
        assert!(s.text.chars().count() > 0);
    }
}
