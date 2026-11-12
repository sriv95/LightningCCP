//! Pure timing rules of the ranklist prefetch (the loop in `crate::prefetch` applies them).

pub const WINDOW: i64 = 7 * 86400;
const FAST: i64 = 30 * 60;
const DAY: i64 = 86400;

/// Standings are fetched once, when the contest is FINISHED, not done yet, and ended within the last 7 days.
pub fn prefetch_due(now: i64, end: i64, phase: &str, done: bool) -> bool {
    phase == "FINISHED" && !done && now - end <= WINDOW
}

/// Rating sweep: every 30 min from `end` until found or day 3, then daily until day 7, then never.
/// A `last_sweep` before `end` (contest rescheduled later) counts as no sweep.
pub fn rating_sweep_due(now: i64, end: i64, ratings_found: bool, last_sweep: Option<i64>) -> bool {
    let age = now - end;
    if !(0..=WINDOW).contains(&age) {
        return false;
    }
    let step = if ratings_found || age >= 3 * DAY { DAY } else { FAST };
    last_sweep.filter(|&l| l >= end).is_none_or(|l| now - l >= step)
}

#[cfg(test)]
mod tests {
    use super::*;
    const END: i64 = 1_000_000;

    #[test]
    fn prefetch_rules() {
        assert!(prefetch_due(END + 60, END, "FINISHED", false));
        assert!(!prefetch_due(END + 60, END, "FINISHED", true));
        for p in ["CODING", "PENDING_SYSTEM_TEST", "SYSTEM_TEST", "BEFORE"] {
            assert!(!prefetch_due(END + 60, END, p, false), "{p}");
        }
        // finished long before the bot started, but inside the window: due on the first tick
        assert!(prefetch_due(END + 6 * DAY, END, "FINISHED", false));
        assert!(!prefetch_due(END + 7 * DAY + 1, END, "FINISHED", false));
    }

    #[test]
    fn sweep_cadence() {
        assert!(!rating_sweep_due(END - 1, END, false, None)); // rescheduled/not ended: nothing before the (new) end
        assert!(rating_sweep_due(END, END, false, None));
        assert!(!rating_sweep_due(END + 1799, END, false, Some(END)));
        assert!(rating_sweep_due(END + 1800, END, false, Some(END)));
        // found: daily
        assert!(!rating_sweep_due(END + 3600 + 1800, END, true, Some(END + 3600)));
        assert!(rating_sweep_due(END + 3600 + DAY, END, true, Some(END + 3600)));
        // day 3 without a find: daily
        assert!(!rating_sweep_due(END + 3 * DAY + 1800, END, false, Some(END + 3 * DAY)));
        assert!(rating_sweep_due(END + 4 * DAY, END, false, Some(END + 3 * DAY)));
        // stop after day 7
        assert!(rating_sweep_due(END + 7 * DAY, END, false, Some(END + 6 * DAY)));
        assert!(!rating_sweep_due(END + 7 * DAY + 1, END, false, None));
    }

    #[test]
    fn rescheduled_contest_ignores_old_sweeps() {
        // swept at old end, then the end moved 2 h later: first sweep at the new end is due at once
        assert!(rating_sweep_due(END + 7200, END + 7200, false, Some(END + 600)));
    }
}
