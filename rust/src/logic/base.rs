//! Pure decision functions, ported from src/logic.py lines 1-470 (mashup and bingo live in `mashup.rs`).
//! Integer division and remainder use `div_euclid`/`rem_euclid` where Python floors (positive divisors only).

use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike};

use crate::cf::models::{Contest, Problem, Submission};
use crate::errors::BotError;

pub const LINK_WINDOW_SECONDS: i64 = 300;
pub const LINK_MAX_RATING: i64 = 1200;
pub const RUNNING_PHASES: [&str; 3] = ["CODING", "PENDING_SYSTEM_TEST", "SYSTEM_TEST"];
pub const MAX_REMINDER_MINUTES: i64 = 10080;
pub const MAX_REMINDER_COUNT: usize = 5;
pub const NEW_CONTEST_SOON_SECONDS: i64 = 48 * 3600;
pub const RECAP_WINDOW_SECONDS: i64 = 86400;
pub const CHOICE_MAX_LEN: usize = 100;
pub const LOBBY_TTL_SECONDS: i64 = 600;
pub const MASHUP_LOBBY_TTL_SECONDS: i64 = 7200;
pub const MATCH_TTL_SECONDS: i64 = 7200;
pub const MATCH_MAX_PLAYERS: usize = 10;
pub const CUSTOM_SCALE: f64 = 1.5;
pub const MASHUP_MAX_PLAYERS: usize = 50;
pub const MASHUP_READS_PER_SECOND: f64 = 0.5;
pub const MASHUP_MIN_INTERVAL: i64 = 10;
pub const MASHUP_MAX_INTERVAL: i64 = 60;
pub const MASHUP_MAX_PROBLEMS: usize = 10;
pub const MASHUP_MAX_TEAMS: usize = 10;
pub const MASHUP_MAX_TEAM_SIZE: usize = 5;
pub const BINGO_MIN_SIZE: usize = 3;
pub const BINGO_MAX_SIZE: usize = 7;
pub const BINGO_REPLACE_STEP: i64 = 100;
pub const MAX_PROBLEM_RATING: i64 = 3500;
pub const START_RATING: i64 = 800;
pub const SOLO_MIN: i64 = 1;
pub const SOLO_EQUAL: i64 = 10;
pub const SOLO_MAX: i64 = 33;
pub const SOLO_RANGE: i64 = 400;
pub const DAILY_MULTIPLIER: i64 = 2;
pub const DAY_SECONDS: i64 = 86400;
pub const STREAK_GAP: i64 = 2;

/// Python `str.strip()` whitespace: Unicode White_Space plus U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

pub fn parse_tags(raw: Option<&str>) -> HashSet<String> {
    raw.unwrap_or("").split(',').map(py_strip).filter(|t| !t.is_empty()).map(str::to_lowercase).collect()
}

/// Canonical stored form of a tag list: sorted, lower case, comma-separated.
pub fn normalize_tags(raw: Option<&str>) -> String {
    let mut tags: Vec<String> = parse_tags(raw).into_iter().collect();
    tags.sort();
    tags.join(",")
}

pub fn tag_suggestions(current: &str, all_tags: &[String], limit: usize) -> Vec<String> {
    let (head, sep, last) = match current.rsplit_once(',') {
        Some((h, l)) => (h, true, l),
        None => ("", false, current),
    };
    let chosen = parse_tags(Some(head));
    let prefix = if sep { format!("{},", py_strip(head)) } else { String::new() };
    let needle = py_strip(last).to_lowercase();
    let mut sorted: Vec<&String> = all_tags.iter().collect();
    sorted.sort();
    let mut out = Vec::new();
    for tag in sorted {
        if chosen.contains(tag) || !tag.contains(&needle) {
            continue;
        }
        let value = format!("{prefix}{tag}");
        if value.chars().count() <= CHOICE_MAX_LEN {
            out.push(value);
        }
        if out.len() == limit {
            break;
        }
    }
    out
}

/// (label, contest id) of the newest finished contests whose problems are known, whose id or name
/// contains what the user typed.
pub fn contest_suggestions(
    contests: &[Contest],
    known_ids: &HashSet<i64>,
    current: &str,
    limit: usize,
) -> Vec<(String, i64)> {
    let needle = py_strip(current).to_lowercase();
    let mut newest: Vec<&Contest> =
        contests.iter().filter(|c| c.phase == "FINISHED" && known_ids.contains(&c.id)).collect();
    // stable, like Python's sorted(reverse=True): equal start times keep input order
    newest.sort_by_key(|c| std::cmp::Reverse(c.start_time.unwrap_or(0)));
    let mut out = Vec::new();
    for c in newest {
        let label = format!("{} - {}", c.id, c.name);
        if label.to_lowercase().contains(&needle) {
            out.push((label.chars().take(CHOICE_MAX_LEN).collect(), c.id));
        }
        if out.len() == limit {
            break;
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct ProblemFilter {
    pub min_rating: Option<i64>,
    pub max_rating: Option<i64>,
    pub tags: HashSet<String>,
    pub exclude_tags: HashSet<String>,
    pub solved: HashSet<String>,
    pub contest_id: Option<i64>,
}

pub fn filter_problems(problems: &[Problem], f: &ProblemFilter) -> Vec<Problem> {
    let mut out = Vec::new();
    for p in problems {
        let Some(rating) = p.rating else { continue }; // unrated problems are not useful for recommendations
        if f.contest_id.is_some_and(|id| p.contest_id != id)
            || f.min_rating.is_some_and(|m| rating < m)
            || f.max_rating.is_some_and(|m| rating > m)
        {
            continue;
        }
        let ptags: HashSet<&String> = p.tags.iter().collect();
        if !f.tags.iter().all(|t| ptags.contains(t)) || f.exclude_tags.iter().any(|t| ptags.contains(t)) {
            continue;
        }
        if f.solved.contains(&p.key()) {
            continue;
        }
        out.push(p.clone());
    }
    out
}

pub fn solved_keys(subs: &[Submission]) -> HashSet<String> {
    subs.iter().filter(|s| s.verdict.as_deref() == Some("OK")).map(|s| s.problem_key.clone()).collect()
}

pub fn format_hm(seconds: i64) -> String {
    format!("{}:{:02}", seconds.div_euclid(3600), seconds.rem_euclid(3600).div_euclid(60))
}

pub fn ordinal(n: i64) -> String {
    let m100 = n.rem_euclid(100);
    let suffix = if (10..=20).contains(&m100) {
        "th"
    } else {
        match n.rem_euclid(10) {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// 1-based place of a player among a match's rating changes (ties share a place); None when nobody's
/// rating moved, which means nobody solved.
pub fn placement(deltas: &[i64], mine: i64) -> Option<i64> {
    if deltas.iter().all(|&d| d == 0) {
        return None;
    }
    Some(1 + deltas.iter().filter(|&&d| d > mine).count() as i64)
}

/// (time, rating change) pairs, oldest first, to (time, rating after) from the start rating.
pub fn rating_series(changes: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut rating = START_RATING;
    changes
        .iter()
        .map(|&(time, delta)| {
            rating += delta;
            (time, rating)
        })
        .collect()
}

/// Problems attempted but never solved, in order of first appearance (newest attempt first).
pub fn unsolved_keys(subs: &[Submission]) -> Vec<String> {
    let solved = solved_keys(subs);
    let mut seen = HashSet::new();
    subs.iter()
        .filter(|s| !solved.contains(&s.problem_key) && seen.insert(s.problem_key.as_str()))
        .map(|s| s.problem_key.clone())
        .collect()
}

pub fn round_rating(rating: Option<i64>) -> i64 {
    match rating {
        None => 800,
        Some(r) => ((r + 50).div_euclid(100) * 100).clamp(800, 3500),
    }
}

pub fn floor_rating(rating: i64) -> i64 {
    (rating.div_euclid(100) * 100).clamp(800, 3500)
}

/// Days since the epoch; the daily problem resets when this changes (midnight UTC).
pub fn utc_day(now: i64) -> i64 {
    now.div_euclid(DAY_SECONDS)
}

/// Start of the week (Sunday 00:00 UTC) that contains now. Epoch day 0 was a Thursday.
pub fn week_start(now: i64) -> i64 {
    let day = utc_day(now);
    (day - (day + 4).rem_euclid(7)) * DAY_SECONDS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeeklyRow {
    pub rank: i64,
    pub user_id: i64,
    pub handle: String,
    pub rating: i64,
    /// rating change in the week before the cutoff
    pub gain: i64,
    /// places gained since a week earlier; None = not ranked then
    pub moved: Option<i64>,
}

/// All-time match ranking as of cutoff, with each player's gain over the week before it and their rank
/// change against the ranking a week earlier. events: (user_id, handle, at, delta).
pub fn weekly_board(events: &[(i64, String, i64, i64)], cutoff: i64) -> Vec<WeeklyRow> {
    let week_ago = cutoff - 7 * DAY_SECONDS;

    // user id -> (rank, handle, rating), plus ids in rank order
    let ranking = |until: i64| -> (Vec<i64>, HashMap<i64, (i64, &str, i64)>) {
        let mut first_seen = Vec::new(); // dict insertion order: the tie-break of Python's stable sort
        let mut total: HashMap<i64, i64> = HashMap::new();
        let mut played: HashMap<i64, i64> = HashMap::new();
        let mut names: HashMap<i64, &str> = HashMap::new();
        for (uid, handle, at, delta) in events {
            if *at < until {
                if !total.contains_key(uid) {
                    first_seen.push(*uid);
                }
                *total.entry(*uid).or_insert(START_RATING) += delta;
                *played.entry(*uid).or_insert(0) += 1;
                names.insert(*uid, handle);
            }
        }
        let mut order = first_seen;
        order.sort_by(|a, b| {
            total[b].cmp(&total[a]).then(played[b].cmp(&played[a])).then(names[a].cmp(names[b]))
        });
        let map = order.iter().enumerate().map(|(i, u)| (*u, (i as i64 + 1, names[u], total[u]))).collect();
        (order, map)
    };

    let (order, now) = ranking(cutoff);
    let (_, before) = ranking(week_ago);
    let mut gain: HashMap<i64, i64> = HashMap::new();
    for (uid, _, at, delta) in events {
        if week_ago <= *at && *at < cutoff {
            *gain.entry(*uid).or_insert(0) += delta;
        }
    }
    order
        .iter()
        .map(|uid| {
            let (rank, handle, rating) = now[uid];
            WeeklyRow {
                rank,
                user_id: *uid,
                handle: handle.to_string(),
                rating,
                gain: gain.get(uid).copied().unwrap_or(0),
                moved: before.get(uid).map(|b| b.0 - rank),
            }
        })
        .collect()
}

/// Claimed daily days (newest first) in a row, allowing one missed day between claims. Missing two days
/// in a row ends the streak, so it is already over once the newest claim is 3+ days old.
pub fn daily_streak(days: &[i64], today: i64) -> i64 {
    if days.is_empty() || today - days[0] > STREAK_GAP {
        return 0;
    }
    let mut streak = 1;
    for w in days.windows(2) {
        if w[0] - w[1] > STREAK_GAP {
            break;
        }
        streak += 1;
    }
    streak
}

pub fn earliest_solve(subs: &[Submission], problem_key: &str, since: i64) -> Option<i64> {
    subs.iter()
        .filter(|s| s.problem_key == problem_key && s.verdict.as_deref() == Some("OK") && s.created >= since)
        .map(|s| s.created)
        .min()
}

/// (time of the earliest accepted submission, wrong attempts before it) within since..=until.
/// Compilation errors and submissions still being judged are not wrong attempts.
pub fn first_solve(subs: &[Submission], problem_key: &str, since: i64, until: i64) -> Option<(i64, i64)> {
    let mine: Vec<&Submission> =
        subs.iter().filter(|s| s.problem_key == problem_key && since <= s.created && s.created <= until).collect();
    let solved_at = mine.iter().filter(|s| s.verdict.as_deref() == Some("OK")).map(|s| s.created).min()?;
    let wrong = mine
        .iter()
        .filter(|s| {
            s.created < solved_at && !matches!(s.verdict.as_deref(), None | Some("OK") | Some("COMPILATION_ERROR"))
        })
        .count();
    Some((solved_at, wrong as i64))
}

/// Problem rating for a match: the host's choice, else the players' average match rating.
/// Errors on an empty `ratings` without a `rating_arg` (Python ZeroDivisionError).
pub fn match_rating(rating_arg: Option<i64>, ratings: &[i64]) -> Result<i64> {
    if let Some(r) = rating_arg {
        return Ok(r);
    }
    let sum = ratings.iter().try_fold(0i64, |a, &r| a.checked_add(r)).ok_or_else(|| anyhow!("rating sum overflows"))?;
    let n = i64::try_from(ratings.len())?;
    Ok(round_rating(Some(sum.checked_div_euclid(n).ok_or_else(|| anyhow!("no ratings to average"))?)))
}

/// Order players by solve time; players who never solved form one tied group at the end.
pub fn rank_groups(entries: &[(i64, Option<i64>)]) -> Vec<Vec<i64>> {
    let mut solvers: Vec<(i64, usize, i64)> =
        entries.iter().enumerate().filter_map(|(i, &(uid, t))| t.map(|t| (t, i, uid))).collect();
    solvers.sort();
    let mut groups: Vec<Vec<i64>> = solvers.into_iter().map(|(_, _, uid)| vec![uid]).collect();
    let rest: Vec<i64> = entries.iter().filter(|e| e.1.is_none()).map(|e| e.0).collect();
    if !rest.is_empty() {
        groups.push(rest);
    }
    groups
}

/// Round half away from zero (Python `_round_away`).
fn round_away(x: f64) -> i64 {
    (x.abs() + 0.5).floor().copysign(x) as i64
}

/// Linear in the gap to the problem, +1 at 400 or more below you, +10 at equal, +33 at 400 or more above.
/// Failing a solo match never costs rating.
pub fn solo_delta(rating: i64, problem_rating: i64, solved: bool) -> i64 {
    if !solved {
        return 0;
    }
    let gap = (problem_rating - rating).clamp(-SOLO_RANGE, SOLO_RANGE);
    if gap < 0 {
        round_away(SOLO_MIN as f64 + ((SOLO_EQUAL - SOLO_MIN) * (gap + SOLO_RANGE)) as f64 / SOLO_RANGE as f64)
    } else {
        round_away(SOLO_EQUAL as f64 + ((SOLO_MAX - SOLO_EQUAL) * gap) as f64 / SOLO_RANGE as f64)
    }
}

/// Rating a winner gains, by how far the loser is above (positive gap) or below the winner.
pub fn duel_gain(gap: i64) -> i64 {
    if gap <= -400 {
        16
    } else if gap <= -200 {
        33
    } else if gap < 200 {
        49
    } else if gap < 400 {
        67
    } else {
        95
    }
}

/// Each pair of players is a duel: the winner gains duel_gain, the loser loses the same, a tie moves
/// nothing. A player's change is the average over their opponents, times scale. `groups` is ordered best
/// first; players in one group tie. Result is in the order players appear in `groups`.
/// Errors if a player has no rating (Python KeyError).
pub fn match_deltas(groups: &[Vec<i64>], ratings: &HashMap<i64, i64>, scale: f64) -> Result<Vec<(i64, i64)>> {
    let n: usize = groups.iter().map(Vec::len).sum();
    if n < 2 {
        return Err(anyhow!("a rated match needs at least 2 players"));
    }
    // dict semantics: a repeated player keeps its first position and takes the last place
    let mut place: Vec<(i64, usize)> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        for &uid in g {
            match place.iter_mut().find(|p| p.0 == uid) {
                Some(p) => p.1 = i,
                None => place.push((uid, i)),
            }
        }
    }
    let rating = |u: i64| ratings.get(&u).copied().ok_or_else(|| anyhow!("no rating for player {u}"));
    place
        .iter()
        .map(|&(uid, mine)| {
            let mut total: i64 = 0;
            for &(other, theirs) in &place {
                if mine < theirs {
                    total += duel_gain(rating(other)? - rating(uid)?);
                } else if mine > theirs {
                    total -= duel_gain(rating(uid)? - rating(other)?);
                }
            }
            Ok((uid, round_away(total as f64 * scale / (n - 1) as f64)))
        })
        .collect()
}

pub fn has_compile_error(subs: &[Submission], problem_key: &str, issued_at: i64, window: i64) -> bool {
    subs.iter().any(|s| {
        s.problem_key == problem_key
            && s.verdict.as_deref() == Some("COMPILATION_ERROR")
            && issued_at <= s.created
            && s.created <= issued_at + window
    })
}

pub fn select_contests(contests: &[Contest], kind: &str, limit: usize) -> Result<Vec<Contest>> {
    let mut picked: Vec<Contest>;
    match kind {
        "upcoming" => {
            picked = contests.iter().filter(|c| c.phase == "BEFORE" && c.start_time.is_some()).cloned().collect();
            picked.sort_by_key(|c| c.start_time);
        }
        "running" => {
            picked = contests.iter().filter(|c| RUNNING_PHASES.contains(&c.phase.as_str())).cloned().collect();
            picked.sort_by_key(|c| c.start_time.unwrap_or(0));
        }
        "recent" => {
            picked = contests.iter().filter(|c| c.phase == "FINISHED" && c.start_time.is_some()).cloned().collect();
            // stable descending, like Python's sorted(reverse=True)
            picked.sort_by_key(|c| std::cmp::Reverse(c.start_time));
            picked.truncate(limit);
        }
        _ => return Err(anyhow!("unknown contest kind: {kind}")),
    }
    Ok(picked)
}

pub fn refresh_minutes(contests: &[Contest]) -> i64 {
    if contests.iter().any(|c| RUNNING_PHASES.contains(&c.phase.as_str())) { 5 } else { 30 }
}

/// Code points of every Unicode decimal digit 0 (Python 3.12 `unicodedata`, Unicode 15.0). Each script's
/// digits are ten consecutive code points, so `c - zero` is the value.
const DECIMAL_ZEROS: [u32; 68] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950, 0x1fbf0,
];

/// Value of a Unicode decimal digit (Python `Py_UNICODE_TODECIMAL`), None for anything else.
fn decimal_digit(c: char) -> Option<u32> {
    let i = DECIMAL_ZEROS.partition_point(|&z| z <= c as u32);
    DECIMAL_ZEROS.get(i.checked_sub(1)?).and_then(|z| (c as u32 - z < 10).then(|| c as u32 - z))
}

/// Python 3.12 `int(part)`: surrounding whitespace (ASCII and Unicode, but not U+001C..U+001F), optional sign, Unicode decimal digits with single
/// underscores between digits, at most 4300 digits (`ValueError` above, even for leading zeros). Overflow
/// saturates, which is rejected as out of range exactly like a huge Python int.
pub fn py_int(part: &str) -> Option<i64> {
    let s = part.trim_matches(char::is_whitespace); // not py_strip: int() does not strip U+001C..U+001F
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut v: i64 = 0;
    let mut count = 0;
    for c in digits.chars().filter(|&c| c != '_') {
        v = v.saturating_mul(10).saturating_add(decimal_digit(c)? as i64);
        count += 1;
    }
    if count == 0 || count > 4300 {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// Python 3.12 `str.casefold`: ASCII by `to_ascii_lowercase`, everything else by the generated table (Python's own
/// Unicode data, not Rust's, which is newer). `ß` gives `ss`, and `Σ` always `σ` (no final-sigma rule).
pub fn casefold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
        } else {
            match super::casefold::fold(c) {
                Some(folded) => out.push_str(folded),
                None => out.push(c),
            }
        }
    }
    out
}

/// Python `\d+` full match then `int()` (custom ids): every char a Unicode decimal digit, no sign, spaces or underscores.
pub fn parse_digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.chars().all(|c| decimal_digit(c).is_some()) {
        return None;
    }
    py_int(s)
}

pub fn parse_minutes(raw: &str) -> Result<Vec<i64>, BotError> {
    let usage = || BotError::Plain("Use up to 5 comma-separated whole minutes between 1 and 10080, e.g. 10,60.".into());
    let mut values = Vec::new();
    for part in raw.split(',') {
        let v = py_int(part).ok_or_else(usage)?;
        if !values.contains(&v) {
            values.push(v);
        }
    }
    if values.len() > MAX_REMINDER_COUNT || values.iter().any(|&v| !(1..=MAX_REMINDER_MINUTES).contains(&v)) {
        return Err(usage());
    }
    values.sort_by(|a, b| b.cmp(a));
    Ok(values)
}

pub fn reminders_due(
    contests: &[Contest],
    before_mins: &[i64],
    sent: &HashSet<(i64, i64)>,
    now: i64,
) -> Vec<(Contest, i64)> {
    let mut due = Vec::new();
    for c in contests {
        let Some(start) = c.start_time else { continue };
        if c.phase != "BEFORE" || now >= start {
            continue;
        }
        for &b in before_mins {
            if now >= start - b * 60 && !sent.contains(&(c.id, b)) {
                due.push((c.clone(), b));
            }
        }
    }
    due
}

/// Contests that have not started and were not posted yet, soonest first.
pub fn new_contests(contests: &[Contest], posted: &HashSet<i64>, now: i64) -> Vec<Contest> {
    let mut due: Vec<Contest> = contests
        .iter()
        .filter(|c| c.phase == "BEFORE" && c.start_time.is_some_and(|s| s > now) && !posted.contains(&c.id))
        .cloned()
        .collect();
    due.sort_by_key(|c| c.start_time);
    due
}

/// Contests that start within `within` seconds and had no such post yet, soonest first.
pub fn soon_contests(contests: &[Contest], done: &HashSet<i64>, now: i64, within: i64) -> Vec<Contest> {
    let mut due: Vec<Contest> = contests
        .iter()
        .filter(|c| {
            c.phase == "BEFORE" && c.start_time.is_some_and(|s| now < s && s <= now + within) && !done.contains(&c.id)
        })
        .cloned()
        .collect();
    due.sort_by_key(|c| c.start_time);
    due
}

fn ics_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\r', "").replace('\n', "\\n")
}

/// An iCalendar file with one event for the contest and an alarm before it. Lines are not folded and end in
/// CRLF, like the Python original. Errors where Python raises an unexpected exception (no start time:
/// TypeError; a timestamp outside years 1..=9999: ValueError/OverflowError), which the bot reports as
/// "Something went wrong". The cog's `_upcoming` already rejects a contest without a start time.
pub fn contest_ics(contest: &Contest, now: i64, alarm_mins: i64) -> Result<String> {
    let stamp = |t: i64| -> Result<String> {
        let dt = DateTime::from_timestamp(t, 0).filter(|d| (1..=9999).contains(&d.year()));
        Ok(dt.ok_or_else(|| anyhow!("year out of range for timestamp {t}"))?.format("%Y%m%dT%H%M%SZ").to_string())
    };
    let start = contest.start_time.ok_or_else(|| anyhow!("contest_ics needs a start time"))?;
    let end = start.checked_add(contest.duration).ok_or_else(|| anyhow!("contest end overflows"))?;
    let url = contest.url();
    let lines = [
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//LightningCCP//Contests//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:cf-contest-{}@lightningccp", contest.id),
        format!("DTSTAMP:{}", stamp(now)?),
        format!("DTSTART:{}", stamp(start)?),
        format!("DTEND:{}", stamp(end)?),
        format!("SUMMARY:{}", ics_text(&contest.name)),
        format!("URL:{url}"),
        format!("DESCRIPTION:{}", ics_text(&url)),
        "BEGIN:VALARM".to_string(),
        "ACTION:DISPLAY".to_string(),
        format!("DESCRIPTION:{} starts in {alarm_mins} minutes", ics_text(&contest.name)),
        format!("TRIGGER:-PT{alarm_mins}M"),
        "END:VALARM".to_string(),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ];
    Ok(lines.join("\r\n") + "\r\n")
}

/// Finished contests that ended within the recap window and have no recap yet, oldest first.
pub fn recaps_due(contests: &[Contest], sent: &HashSet<i64>, now: i64) -> Vec<Contest> {
    let mut due: Vec<Contest> = contests
        .iter()
        .filter(|c| {
            c.phase == "FINISHED"
                && c.start_time.is_some_and(|s| (0..=RECAP_WINDOW_SECONDS).contains(&(now - (s + c.duration))))
                && !sent.contains(&c.id)
        })
        .cloned()
        .collect();
    due.sort_by_key(|c| c.start_time);
    due
}
