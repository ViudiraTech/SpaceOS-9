/*
 *
 *       user/spime/src/punct.rs
 *       Smart Chinese punctuation pairing, decimal '.' disambiguation,
 *       and quick date/time shortcut candidates.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

/// Stateful per-context Chinese punctuation converter.
#[derive(Clone, Debug, Default)]
pub struct PunctState {
    double_quote_open: bool,
    single_quote_open: bool,
    prev_was_ascii_digit: bool,
}

impl PunctState {
    pub fn reset(&mut self) {
        self.double_quote_open = false;
        self.single_quote_open = false;
        self.prev_was_ascii_digit = false;
    }

    pub fn note_committed(&mut self, text: &str) {
        self.prev_was_ascii_digit = text
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_digit());
    }

    /// Map an ASCII character typed in Chinese mode when smart CN punctuation is enabled.
    ///
    /// Returns `Some(mapped)` for punctuation characters (including paired `“”`/`‘’`
    /// and decimal `.` preserved after a digit), updating internal pairing state.
    pub fn map_char(&mut self, ch: char, cn_punct: bool) -> Option<&'static str> {
        if ch.is_ascii_digit() {
            self.prev_was_ascii_digit = true;
            return None;
        }
        if ch == '.' && self.prev_was_ascii_digit && cn_punct {
            self.prev_was_ascii_digit = false;
            return Some(".");
        }
        self.prev_was_ascii_digit = false;
        if !cn_punct {
            return None;
        }
        match ch {
            ',' => Some("，"),
            '.' => Some("。"),
            '?' => Some("？"),
            '!' => Some("！"),
            ':' => Some("："),
            ';' => Some("；"),
            '(' => Some("（"),
            ')' => Some("）"),
            '[' => Some("【"),
            ']' => Some("】"),
            '<' => Some("《"),
            '>' => Some("》"),
            '\\' => Some("、"),
            '^' => Some("……"),
            '_' => Some("——"),
            '~' => Some("～"),
            '`' => Some("·"),
            '$' => Some("￥"),
            '"' => {
                let s = if self.double_quote_open { "”" } else { "“" };
                self.double_quote_open = !self.double_quote_open;
                Some(s)
            }
            '\'' => {
                let s = if self.single_quote_open { "’" } else { "‘" };
                self.single_quote_open = !self.single_quote_open;
                Some(s)
            }
            _ => None,
        }
    }
}

/// Generate quick local date/time candidates for `"rq"` / `"sj"` inputs.
pub fn special_shortcut_candidates(raw: &str) -> Vec<String> {
    if raw != "rq" && raw != "sj" {
        return Vec::new();
    }
    let (year, mon, day, hour, min, sec) = local_ymdhms();
    if raw == "rq" {
        vec![
            format!("{year}年{mon}月{day}日"),
            format!("{year}-{mon:02}-{day:02}"),
            format!("{year}/{mon:02}/{day:02}"),
        ]
    } else {
        vec![
            format!("{hour:02}:{min:02}"),
            format!("{hour:02}:{min:02}:{sec:02}"),
            format!("{hour}时{min:02}分"),
        ]
    }
}

fn local_ymdhms() -> (i32, i32, i32, i32, i32, i32) {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (2026, 1, 1, 0, 0, 0);
        }
        (
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_dot_after_digit_stays_ascii_period() {
        let mut st = PunctState::default();
        assert_eq!(st.map_char('3', true), None);
        assert_eq!(st.map_char('.', true), Some("."));
        assert_eq!(st.map_char('1', true), None);
        assert_eq!(st.map_char(',', true), Some("，"));
        assert_eq!(st.map_char('.', true), Some("。"));
    }

    #[test]
    fn quotes_alternate_open_and_close_pairs() {
        let mut st = PunctState::default();
        assert_eq!(st.map_char('"', true), Some("“"));
        assert_eq!(st.map_char('"', true), Some("”"));
        assert_eq!(st.map_char('\'', true), Some("‘"));
        assert_eq!(st.map_char('\'', true), Some("’"));
    }

    #[test]
    fn shortcuts_produce_formatted_date_and_time() {
        let rq = special_shortcut_candidates("rq");
        assert_eq!(rq.len(), 3);
        assert!(rq[0].contains('年') && rq[0].contains('月') && rq[0].contains('日'));
        let sj = special_shortcut_candidates("sj");
        assert_eq!(sj.len(), 3);
        assert!(sj[0].contains(':'));
    }
}
