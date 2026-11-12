//! Pure timing rules of the ranklist prefetch (the loop in `crate::prefetch` applies them).

pub const WINDOW: i64 = 7 * 86400;
const FAST: i64 = 30 * 60;
const DAY: i64 = 86400;

/// Results-fetch steps (bits of the persisted `results_done` mask).
pub const STEP_END: i64 = 1;
pub const STEP_FINISHED: i64 = 2;
pub const STEP_RATINGS: i64 = 4;
pub const STEP_DAY3: i64 = 8;

/// Steps due now (0 = no fetch): at the end, when FINISHED, when ratings are first found, and at day 3 if they never were.
/// One fetch covers every due step, so the caller ORs the result into `results_done`. Only contests ended within the window.
pub fn results_steps_due(now: i64, end: i64, phase: &str, ratings_found: bool, done: i64) -> i64 {
    let age = now - end;
    if !(0..=WINDOW).contains(&age) {
        return 0;
    }
    let mut due = STEP_END;
    if phase == "FINISHED" {
        due |= STEP_FINISHED;
    }
    if ratings_found {
        due |= STEP_RATINGS;
    } else if age >= 3 * DAY {
        due |= STEP_DAY3;
    }
    due & !done
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
    fn results_step_rules() {
        let due = |dt: i64, ph: &str, rf: bool, done: i64| results_steps_due(END + dt, END, ph, rf, done);
        assert_eq!(due(-1, "CODING", false, 0), 0); // before end (also a rescheduled contest)
        assert_eq!(due(0, "CODING", false, 0), STEP_END);
        assert_eq!(due(60, "SYSTEM_TEST", false, STEP_END), 0);
        // end and FINISHED in one tick: one fetch marks both
        assert_eq!(due(0, "FINISHED", false, 0), STEP_END | STEP_FINISHED);
        assert_eq!(due(60, "FINISHED", false, STEP_END | STEP_FINISHED), 0);
        // FINISHED later: second fetch
        assert_eq!(due(3600, "FINISHED", false, STEP_END), STEP_FINISHED);
        // restart catch-up: FINISHED at startup, nothing done -> one fetch, all due steps
        assert_eq!(due(DAY, "FINISHED", false, 0), STEP_END | STEP_FINISHED);
        assert_eq!(due(4 * DAY, "FINISHED", false, 0), STEP_END | STEP_FINISHED | STEP_DAY3);
        // ratings found: once
        assert_eq!(due(3600, "FINISHED", true, STEP_END | STEP_FINISHED), STEP_RATINGS);
        assert_eq!(due(3700, "FINISHED", true, STEP_END | STEP_FINISHED | STEP_RATINGS), 0);
        // day 3 only without ratings, once
        assert_eq!(due(3 * DAY - 1, "FINISHED", false, STEP_END | STEP_FINISHED), 0);
        assert_eq!(due(3 * DAY, "FINISHED", false, STEP_END | STEP_FINISHED), STEP_DAY3);
        assert_eq!(due(3 * DAY + 60, "FINISHED", false, 15), 0);
        assert_eq!(due(3 * DAY, "FINISHED", true, STEP_END | STEP_FINISHED | STEP_RATINGS), 0);
        // outside the 7-day window
        assert_eq!(due(7 * DAY + 1, "FINISHED", false, 0), 0);
        assert_eq!(due(7 * DAY, "FINISHED", false, 0), STEP_END | STEP_FINISHED | STEP_DAY3);
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
