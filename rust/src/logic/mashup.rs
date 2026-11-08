//! Mashup and bingo decisions, ported from src/logic.py lines 470-689.
//! Every pick takes an injected rng. Overflow where Python's unbounded int would not wrap is an error.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use rand::Rng;
use rand::seq::IndexedRandom;

use super::base::{BINGO_REPLACE_STEP, MAX_PROBLEM_RATING, ProblemFilter, filter_problems};
use crate::cf::models::Problem;

/// team or user -> problem key -> (solve time, wrong attempts), both levels in insertion order like
/// Python's dicts (bingo_board breaks ties by that order).
pub type Solves = Vec<(i64, Vec<(String, (i64, i64))>)>;

fn solves_of(solves: &Solves, id: i64) -> &[(String, (i64, i64))] {
    solves.iter().find(|(u, _)| *u == id).map_or(&[], |(_, v)| v)
}

/// Python `list(seq[i:i+size] for i in range(0, len, size))`; size 0 is a ValueError there.
pub fn chunked<T: Clone>(seq: &[T], size: usize) -> Result<Vec<Vec<T>>> {
    if size == 0 {
        return Err(anyhow!("range() arg 3 must not be zero"));
    }
    Ok(seq.chunks(size).map(<[T]>::to_vec).collect())
}

fn overflow() -> anyhow::Error {
    anyhow!("integer overflow")
}

/// `count` distinct unsolved problems, easy to hard, nearest to ratings spread evenly over
/// min_rating..max_rating. `None` when the filters cannot supply that many.
#[allow(clippy::too_many_arguments)]
pub fn pick_mashup(
    problems: &[Problem],
    min_rating: i64,
    max_rating: i64,
    count: i64,
    solved: &HashSet<String>,
    rng: &mut impl Rng,
    tags: &HashSet<String>,
    exclude_tags: &HashSet<String>,
) -> Result<Option<Vec<Problem>>> {
    let targets: Vec<f64> = if count == 1 {
        vec![min_rating.checked_add(max_rating).ok_or_else(overflow)? as f64 / 2.0]
    } else {
        let span = max_rating.checked_sub(min_rating).ok_or_else(overflow)?;
        let mut v = Vec::new();
        for i in 0..count.max(0) {
            let num = span.checked_mul(i).ok_or_else(overflow)?;
            v.push(min_rating as f64 + num as f64 / (count - 1) as f64);
        }
        v
    };
    let mut chosen: Vec<Problem> = Vec::new();
    let mut taken = solved.clone();
    for target in targets {
        let pool = filter_problems(
            problems,
            &ProblemFilter {
                min_rating: Some(min_rating),
                max_rating: Some(max_rating),
                tags: tags.clone(),
                exclude_tags: exclude_tags.clone(),
                solved: taken.clone(),
                contest_id: None,
            },
        );
        // filter_problems drops unrated problems, so rating is always Some here
        let dist = |p: &Problem| (p.rating.unwrap_or(0) as f64 - target).abs();
        let Some(nearest) = pool.iter().map(dist).min_by(f64::total_cmp) else {
            return Ok(None);
        };
        let near: Vec<&Problem> = pool.iter().filter(|p| dist(p) == nearest).collect();
        let pick = (*near.choose(rng).ok_or_else(|| anyhow!("empty pool"))?).clone();
        taken.insert(pick.key());
        chosen.push(pick);
    }
    Ok(Some(chosen))
}

/// Team number -> problem -> (solve time, wrong attempts) of the teammate who solved it first.
/// `members` are (user_id, team); a problem counts once for the team.
pub fn team_solves(members: &[(i64, i64)], solves: &Solves) -> Solves {
    let mut out: Solves = Vec::new();
    for &(user_id, team) in members {
        let i = out.iter().position(|(t, _)| *t == team).unwrap_or_else(|| {
            out.push((team, Vec::new()));
            out.len() - 1
        });
        let mine = &mut out[i].1;
        for (key, f) in solves_of(solves, user_id) {
            match mine.iter_mut().find(|(k, _)| k == key) {
                Some((_, cur)) => {
                    if f < cur {
                        *cur = *f;
                    }
                }
                None => mine.push((key.clone(), *f)),
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MashupRank {
    pub rank: i64,
    pub user_id: i64, // the team number in a team contest
    pub handle: String,
    pub solved: i64,
    pub total_seconds: i64,
    pub times: HashMap<String, i64>,
    pub wrong: HashMap<String, i64>,
    pub lines: i64, // full bingo lines (always 0 in a contest)
}

/// Most problems solved first, then the lowest total time; equal scores share a rank.
/// `players` are (user_id, handle, joined_at). Every wrong attempt adds `penalty_minutes`.
pub fn mashup_standings(
    players: &[(i64, String, i64)],
    solves: &Solves,
    started_at: i64,
    penalty_minutes: i64,
) -> Result<Vec<MashupRank>> {
    struct Row {
        neg: i64,
        total: i64,
        joined_at: i64,
        user_id: i64,
        handle: String,
        times: HashMap<String, i64>,
        wrong: HashMap<String, i64>,
    }
    let mut rows = Vec::new();
    for (user_id, handle, joined_at) in players {
        let mine = solves_of(solves, *user_id);
        let mut times = HashMap::new();
        let mut wrong = HashMap::new();
        for (k, (t, w)) in mine {
            times.insert(k.clone(), t.checked_sub(started_at).ok_or_else(overflow)?.max(0));
            wrong.insert(k.clone(), *w);
        }
        let mut total = 0i64;
        for &t in times.values() {
            total = total.checked_add(t).ok_or_else(overflow)?;
        }
        let mut wsum = 0i64;
        for &w in wrong.values() {
            wsum = wsum.checked_add(w).ok_or_else(overflow)?;
        }
        let pen = wsum.checked_mul(penalty_minutes).and_then(|x| x.checked_mul(60)).ok_or_else(overflow)?;
        total = total.checked_add(pen).ok_or_else(overflow)?;
        let neg = -i64::try_from(times.len())?;
        rows.push(Row { neg, total, joined_at: *joined_at, user_id: *user_id, handle: handle.clone(), times, wrong });
    }
    rows.sort_by_key(|r| (r.neg, r.total, r.joined_at)); // stable, like Python's sort on r[:3]
    let mut ranked: Vec<MashupRank> = Vec::new();
    for (i, r) in rows.into_iter().enumerate() {
        let same = ranked.last().is_some_and(|l| (l.solved, l.total_seconds) == (-r.neg, r.total));
        let rank = match ranked.last() {
            Some(l) if same => l.rank,
            _ => i as i64 + 1,
        };
        ranked.push(MashupRank {
            rank,
            user_id: r.user_id,
            handle: r.handle,
            solved: -r.neg,
            total_seconds: r.total,
            times: r.times,
            wrong: r.wrong,
            lines: 0,
        });
    }
    Ok(ranked)
}

/// A random unused problem rated BINGO_REPLACE_STEP above `rating`, else the next rating up that has
/// one, never above `max_rating`; when nothing higher fits, one of the same rating.
pub fn pick_replacement(
    problems: &[Problem],
    rating: i64,
    used: &HashSet<String>,
    max_rating: i64,
    rng: &mut impl Rng,
    tags: &HashSet<String>,
    exclude_tags: &HashSet<String>,
) -> Option<Problem> {
    let start = rating.saturating_add(BINGO_REPLACE_STEP);
    let end = max_rating.min(MAX_PROBLEM_RATING) + 1;
    let ladder = (start..end).step_by(100).chain(std::iter::once(rating));
    for r in ladder {
        let pool = filter_problems(
            problems,
            &ProblemFilter {
                min_rating: Some(r),
                max_rating: Some(r),
                tags: tags.clone(),
                exclude_tags: exclude_tags.clone(),
                solved: used.clone(),
                contest_id: None,
            },
        );
        if let Some(p) = pool.choose(rng) {
            return Some(p.clone());
        }
    }
    None
}

/// Every row, column and the two diagonals of a size x size board, as cell numbers.
pub fn bingo_lines(size: usize) -> Vec<Vec<i64>> {
    let n = size as i64;
    let mut out: Vec<Vec<i64>> = (0..n).map(|r| (r * n..(r + 1) * n).collect()).collect();
    out.extend((0..n).map(|c| (c..n * n).step_by(size.max(1)).collect::<Vec<i64>>()));
    out.push((0..n).map(|i| i * (n + 1)).collect());
    out.push((0..n).map(|i| (i + 1) * (n - 1)).collect());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BingoBoard {
    pub squares: Vec<(String, i64, Option<i64>)>, // per cell: (current problem key, rating, holder)
    pub claims: HashMap<String, (i64, i64)>,      // claimed problem key -> (team, solve time)
    pub holders: HashMap<i64, (i64, i64)>,        // cell -> (team holding it, time of its latest claim)
    pub lines: HashMap<i64, i64>,                 // team -> full lines held
    pub over: bool,
}

/// State of a shared bingo board. `cells` are (problem key, rating, cell) in the order they were put
/// on the board (the last problem of a cell is the current one); `solves` is team -> key -> (time, wrong).
/// Errors where Python raises KeyError: a board cell 0..size*size with no problem.
pub fn bingo_board(size: usize, mode: &str, win: &str, cells: &[(String, i64, i64)], solves: &Solves) -> Result<BingoBoard> {
    // key -> (solve time, team), kept in the order Python's dict gets the keys
    let mut first: Vec<(&str, (i64, i64))> = Vec::new();
    for &(team, ref mine) in solves {
        for (key, (at, _)) in mine {
            match first.iter_mut().find(|(k, _)| k == key) {
                Some((_, cur)) => {
                    if (*at, team) < *cur {
                        *cur = (*at, team);
                    }
                }
                None => first.push((key, (*at, team))),
            }
        }
    }
    let cell_of: HashMap<&str, i64> = cells.iter().map(|(k, _, c)| (k.as_str(), *c)).collect();
    let mut holders: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut kept: Vec<(&str, i64, i64)> = Vec::new();
    let mut over = false;
    let lines = bingo_lines(size);
    // a cell nobody holds compares as team 0, like Python's holders.get(c, (0,))[0]
    let holds = |holders: &HashMap<i64, (i64, i64)>, team: i64| -> Vec<&Vec<i64>> {
        lines.iter().filter(|l| l.iter().all(|c| holders.get(c).map_or(0, |h| h.0) == team)).collect()
    };
    first.sort_by_key(|&(_, v)| v); // stable, like Python's sorted
    for (key, (at, team)) in first {
        let Some(&cell) = cell_of.get(key) else { continue };
        kept.push((key, team, at));
        holders.insert(cell, (team, at));
        if win == "line" && holds(&holders, team).iter().any(|l| l.contains(&cell)) {
            over = true;
            break;
        }
    }
    if mode == "classic" {
        over = over || kept.len() == size * size;
    }
    let claims = kept.iter().map(|&(k, t, a)| (k.to_string(), (t, a))).collect();
    let teams: HashSet<i64> = holders.values().map(|h| h.0).collect();
    let held = teams.into_iter().map(|t| (t, holds(&holders, t).len() as i64)).filter(|&(_, n)| n > 0).collect();
    let latest: HashMap<i64, (&String, i64)> = cells.iter().map(|(k, r, c)| (*c, (k, *r))).collect();
    let mut squares = Vec::new();
    for c in 0..(size * size) as i64 {
        let &(k, r) = latest.get(&c).ok_or_else(|| anyhow!("no problem on board cell {c}"))?;
        squares.push((k.clone(), r, holders.get(&c).map(|h| h.0)));
    }
    Ok(BingoBoard { squares, claims, holders, lines: held, over })
}

/// Teams by full lines held, then squares held, then who reached that score first; equal scores
/// share a rank. `players` are (team, handle, joined_at).
pub fn bingo_standings(players: &[(i64, String, i64)], board: &BingoBoard, started_at: i64) -> Result<Vec<MashupRank>> {
    struct Row {
        score: (i64, i64, i64),
        joined_at: i64,
        team: i64,
        handle: String,
        n: i64,
        held: i64,
    }
    let mut rows = Vec::new();
    for (team, handle, joined_at) in players {
        let mine: Vec<i64> = board.holders.values().filter(|h| h.0 == *team).map(|h| h.1).collect();
        let held = board.lines.get(team).copied().unwrap_or(0);
        let n = mine.len() as i64;
        rows.push(Row {
            score: (-held, -n, mine.iter().copied().max().unwrap_or(0)),
            joined_at: *joined_at,
            team: *team,
            handle: handle.clone(),
            n,
            held,
        });
    }
    rows.sort_by_key(|r| (r.score, r.joined_at));
    let mut ranked: Vec<MashupRank> = Vec::new();
    let mut prev: Option<(i64, i64, i64)> = None;
    for (i, r) in rows.into_iter().enumerate() {
        let rank = match (ranked.last(), prev) {
            (Some(l), Some(p)) if p == r.score => l.rank,
            _ => i as i64 + 1,
        };
        let total = if r.n > 0 { r.score.2.checked_sub(started_at).ok_or_else(overflow)?.max(0) } else { 0 };
        prev = Some(r.score);
        ranked.push(MashupRank {
            rank,
            user_id: r.team,
            handle: r.handle,
            solved: r.n,
            total_seconds: total,
            times: HashMap::new(),
            wrong: HashMap::new(),
            lines: r.held,
        });
    }
    Ok(ranked)
}
