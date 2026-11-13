//! Mashup and bingo messages and embeds, ported from src/ui/embeds.py lines 463-743. Text, field order,
//! colours and limits are byte-exact; where Python raises (StopIteration, KeyError, ValueError) these
//! return `Err`, and a Python `None` in an f-string renders as "None".
use std::collections::HashMap;

use anyhow::{Result, anyhow};

use super::a::{EmbedData, GOLD, GREEN, BLURPLE, RED, esc, profile_link};
use crate::cf::models::Problem;
use crate::logic::base::{BINGO_REPLACE_STEP, MASHUP_LOBBY_TTL_SECONDS, format_hm};
use crate::logic::mashup::{BingoBoard, MashupRank, Solves, bingo_board, bingo_standings, mashup_standings, team_solves};
use crate::store::mashup::{MashupCell, MashupPlayer, MashupRow};

/// A solve for the thread log. Field order is Python's tuple order, so the derived `Ord` is its `sorted`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogEntry {
    pub at: i64,
    pub team: i64,
    pub key: String,
    pub rating: i64,
    /// the player who solved it
    pub user: i64,
    /// wrong attempts before the solve
    pub wrong: i64,
}

/// Python `MashupView`. Its derived values are methods and fail where Python raises; a player without a
/// team is an error there too (Python would treat `None` as a team).
#[derive(Debug, Clone)]
pub struct MashupView {
    pub mashup: MashupRow,
    /// (problem key, rating), easy to hard; empty in the lobby
    pub problems: Vec<(String, i64)>,
    pub players: Vec<MashupPlayer>,
    /// user -> problem -> (solve time, wrong), in insertion order
    pub solves: Solves,
    pub note: Option<String>,
    /// bingo only: every problem on the board
    pub cells: Vec<MashupCell>,
}

fn solves_of(solves: &Solves, id: i64) -> &[(String, (i64, i64))] {
    solves.iter().find(|(u, _)| *u == id).map_or(&[], |(_, v)| v)
}

fn overflow() -> anyhow::Error {
    anyhow!("integer overflow")
}

/// Python `{x}` of an Optional int.
fn opt(v: Option<i64>) -> String {
    v.map_or_else(|| "None".into(), |v| v.to_string())
}

fn team_of(p: &MashupPlayer) -> Result<i64> {
    p.team.ok_or_else(|| anyhow!("player {} has no team", p.user_id))
}

impl MashupView {
    pub fn team_solves(&self) -> Result<Solves> {
        let members = self.players.iter().map(|p| Ok((p.user_id, team_of(p)?))).collect::<Result<Vec<_>>>()?;
        Ok(team_solves(&members, &self.solves))
    }

    pub fn board(&self) -> Result<BingoBoard> {
        let m = &self.mashup;
        let cells = self
            .cells
            .iter()
            .map(|c| Ok((c.key.clone(), c.rating, c.cell.ok_or_else(|| anyhow!("cell {} has no board cell", c.key))?)))
            .collect::<Result<Vec<_>>>()?;
        bingo_board(usize::try_from(m.size)?, &m.mode, &m.win, &cells, &self.team_solves()?)
    }

    /// The solves that count, oldest first: a contest team's solve of each problem, or a bingo claim.
    pub fn log(&self) -> Result<Vec<LogEntry>> {
        let mut rating: HashMap<&str, i64> = self.cells.iter().map(|c| (c.key.as_str(), c.rating)).collect();
        rating.extend(self.problems.iter().map(|(k, r)| (k.as_str(), *r)));
        let team_solves = self.team_solves()?;
        let mut found: Vec<(i64, String, (i64, i64))> = Vec::new();
        if self.mashup.mode != "contest" {
            for (key, (team, _)) in self.board()?.claims {
                let at = solves_of(&team_solves, team).iter().find(|(k, _)| *k == key).ok_or_else(|| anyhow!("KeyError {key}"))?.1;
                found.push((team, key, at));
            }
        } else {
            for (team, mine) in &team_solves {
                found.extend(mine.iter().map(|(k, f)| (*team, k.clone(), *f)));
            }
        }
        let mut out = Vec::new();
        for (team, key, at) in found {
            let user = self
                .players
                .iter()
                .find(|p| p.team == Some(team) && solves_of(&self.solves, p.user_id).iter().any(|(k, f)| *k == key && *f == at))
                .ok_or_else(|| anyhow!("no player of team {team} solved {key}"))?
                .user_id;
            let rating = *rating.get(key.as_str()).ok_or_else(|| anyhow!("KeyError {key}"))?;
            out.push(LogEntry { at: at.0, team, key, rating, user, wrong: at.1 });
        }
        out.sort();
        Ok(out)
    }

    /// The teams (`MashupRank::user_id` is the team number; a team of one shows its player's handle).
    pub fn standings(&self) -> Result<Vec<MashupRank>> {
        let mut joined: Vec<(i64, (i64, &str))> = Vec::new();
        for p in &self.players {
            let team = team_of(p)?;
            match joined.iter_mut().find(|(t, _)| *t == team) {
                Some((_, cur)) => {
                    if p.joined_at < cur.0 {
                        *cur = (p.joined_at, &p.handle);
                    }
                }
                None => joined.push((team, (p.joined_at, &p.handle))),
            }
        }
        let m = &self.mashup;
        let solo = m.team_size == 1;
        let teams: Vec<(i64, String, i64)> =
            joined.into_iter().map(|(t, (at, h))| (t, if solo { h.to_string() } else { format!("Team {t}") }, at)).collect();
        let started = m.started_at.unwrap_or(0);
        if m.mode != "contest" {
            return bingo_standings(&teams, &self.board()?, started);
        }
        mashup_standings(&teams, &self.team_solves()?, started, m.penalty_minutes)
    }
}

/// The match rating change of a standings row (a player, or a team in a team contest).
fn mashup_change(view: &MashupView, rank: &MashupRank) -> Option<i64> {
    view.players.iter().find(|p| p.team == Some(rank.user_id)).and_then(|p| p.delta)
}

fn setup(view: &MashupView) -> String {
    let m = &view.mashup;
    let mut head = if m.mode != "contest" {
        let win = if m.win == "line" { "the first full line wins" } else { "most lines at the end wins" };
        let mut h = format!("{0}x{0} {1} bingo, {win}", m.size, m.mode);
        if m.mode == "replace" {
            h += &format!(", a claimed square gets a problem {BINGO_REPLACE_STEP} higher to take it over");
        }
        h + &format!(", problems rated {}-{}, {} long", m.min_rating, m.max_rating, format_hm(m.duration_seconds))
    } else {
        let penalty =
            if m.penalty_minutes != 0 { format!("{} min per wrong attempt", m.penalty_minutes) } else { "no penalty".into() };
        format!(
            "{} problems rated {}-{}, {} long, {penalty}",
            m.count,
            m.min_rating,
            m.max_rating,
            format_hm(m.duration_seconds)
        )
    };
    if m.team_size != 1 {
        head += &format!(", {} teams", m.teams);
    }
    if m.team_size > 1 {
        head += &format!(" of up to {}", m.team_size);
    }
    if let Some(c) = m.contest_id.filter(|c| *c != 0) {
        head += &format!(", virtual contest {c}");
    }
    if !m.tags.is_empty() {
        head += &format!(", tags {}", m.tags.replace(',', ", "));
    }
    if !m.exclude_tags.is_empty() {
        head += &format!(", without {}", m.exclude_tags.replace(',', ", "));
    }
    if m.rated == 0 {
        head += ", unrated";
    }
    head
}

fn log_who(view: &MashupView, team: i64, user: i64) -> String {
    if view.mashup.team_size == 1 { format!("<@{user}>") } else { format!("Team {team} (<@{user}>)") }
}

fn kind(m: &MashupRow) -> &'static str {
    if m.mode != "contest" { "bingo" } else { "contest" }
}

/// The thread post that tells every player the contest began.
pub fn start_message(view: &MashupView) -> String {
    let m = &view.mashup;
    let who = view.players.iter().map(|p| format!("<@{}>", p.user_id)).collect::<Vec<_>>().join(" ");
    format!(
        "{who}\nThe {} has started and ends <t:{}:R>. Solve the problems on Codeforces; the bot reads your submissions, and Check reads you at once.",
        kind(m),
        opt(m.ends_at)
    )
}

pub fn give_up_message(view: &MashupView, user: i64) -> Result<String> {
    let p = view.players.iter().find(|p| p.user_id == user).ok_or_else(|| anyhow!("no player {user}"))?;
    Ok(format!("{} gave up.", log_who(view, team_of(p)?, user)))
}

/// The clock time of a solve and how long after the start it came.
fn stamp(view: &MashupView, at: i64) -> Result<String> {
    let base = view.mashup.started_at.filter(|s| *s != 0).unwrap_or(at);
    let minutes = at.checked_sub(base).ok_or_else(overflow)?.max(0) / 60;
    let elapsed = if minutes < 60 { format!("{minutes}m") } else { format!("{}h{:02}m", minutes / 60, minutes % 60) };
    Ok(format!("<t:{at}:T> (+{elapsed})"))
}

fn link(key: &str, by_key: &HashMap<String, Problem>) -> String {
    by_key.get(key).map_or_else(|| key.to_string(), |p| format!("[{key}](<{}>)", p.url()))
}

fn letter(i: usize) -> Result<char> {
    u32::try_from(i).ok().and_then(|i| char::from_u32(65 + i)).ok_or_else(|| anyhow!("chr() arg not in range"))
}

fn letters(view: &MashupView) -> Result<HashMap<&str, char>> {
    view.problems.iter().enumerate().map(|(i, (k, _))| Ok((k.as_str(), letter(i)?))).collect()
}

/// One solve for the thread log; a replace bingo adds the cell's new problem.
pub fn log_message(view: &MashupView, entry: &LogEntry, by_key: &HashMap<String, Problem>) -> Result<String> {
    let m = &view.mashup;
    let name = link(&entry.key, by_key);
    let who = log_who(view, entry.team, entry.user);
    let when = stamp(view, entry.at)?;
    if m.mode == "contest" {
        let i = view.problems.iter().position(|(k, _)| *k == entry.key).ok_or_else(|| anyhow!("{} is not in list", entry.key))?;
        let mut text = format!("{when} {who} solved {}. {name} ({})", letter(i)?, entry.rating);
        if entry.wrong != 0 {
            text += &format!(", {} wrong before", entry.wrong);
        }
        // the log is oldest first, so the first entry of a problem is its first solve
        let log = view.log()?;
        let first = log.iter().find(|e| e.key == entry.key).ok_or_else(|| anyhow!("StopIteration"))?;
        return Ok(text + if first == entry { " - first solve!" } else { "" });
    }
    let mut text = format!("{when} {who} claimed {name} ({})", entry.rating);
    let cell = view.cells.iter().find(|c| c.key == entry.key).ok_or_else(|| anyhow!("StopIteration"))?;
    if let Some(new) = view.cells.iter().find(|c| c.cell == cell.cell && c.idx > cell.idx) {
        text += &format!(". New problem there: {} ({})", link(&new.key, by_key), new.rating);
    }
    Ok(text)
}

/// The newest claims, newest first, that fit a field: when, problem, team and who solved.
fn bingo_log(view: &MashupView) -> Result<String> {
    let mut out: Vec<String> = Vec::new();
    let mut size = 0usize;
    for e in view.log()?.iter().rev() {
        let line = format!("{} {} ({}) - {}", stamp(view, e.at)?, e.key, e.rating, log_who(view, e.team, e.user));
        let n = line.chars().count();
        if out.len() == 12 || size + n + 1 > 1000 {
            break;
        }
        out.push(line);
        size += n + 1;
    }
    Ok(out.join("\n"))
}

/// Python `str.rstrip()` strips these (`\x1c`-`\x1f` are whitespace there, not in Rust's `char::is_whitespace`).
fn rstrip(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Python `seq[r*size:(r+1)*size]` of a row, clamped like a slice.
fn row_of<T>(seq: &[T], r: i64, size: i64) -> &[T] {
    let at = |i: i64| usize::try_from(i.saturating_mul(size)).unwrap_or(0).min(seq.len());
    &seq[at(r)..at(r + 1)]
}

/// The board as text: a classic claimed square shows [team] instead of its problem; a replace one shows
/// its problem and the holder.
fn bingo_grid(view: &MashupView) -> Result<String> {
    let m = &view.mashup;
    let shown: Vec<String> = view
        .board()?
        .squares
        .iter()
        .map(|(key, _, team)| match (m.mode == "replace", team) {
            (true, Some(t)) => format!("{key}[{t}]"),
            (false, Some(t)) => format!("[{t}]"),
            _ => key.clone(),
        })
        .collect();
    let width = shown.iter().map(|t| t.chars().count()).max().ok_or_else(|| anyhow!("max() arg is an empty sequence"))?;
    let rows: Vec<String> = (0..m.size)
        .map(|r| {
            let cells: Vec<String> =
                row_of(&shown, r, m.size).iter().map(|t| format!("{t}{}", " ".repeat(width - t.chars().count()))).collect();
            rstrip(&cells.join("  ")).to_string()
        })
        .collect();
    Ok(rows.join("\n"))
}

/// The current board (a bingo's grid, a contest's standings) for the newest message of the solve log thread.
pub fn log_board_embed(view: &MashupView) -> Result<EmbedData> {
    let text = if view.mashup.mode == "contest" {
        standings_lines(view, &letters(view)?, false)?.join("\n")
    } else {
        let mut text = format!("```\n{}\n```", bingo_grid(view)?);
        if view.mashup.status == "finished" {
            text += &format!("\n{}", bingo_standings_lines(view, false)?.join("\n"));
        }
        text
    };
    Ok(EmbedData { description: Some(text), colour: Some(GOLD), ..EmbedData::default() })
}

/// The closing line of the solve log thread.
pub fn final_message(view: &MashupView) -> Result<String> {
    let m = &view.mashup;
    let kind = kind(m);
    if m.status == "cancelled" {
        return Ok(format!("This {kind} was cancelled."));
    }
    if view.log()?.is_empty() {
        return Ok(format!("The {kind} is over. Nobody solved anything."));
    }
    let standings = view.standings()?;
    let names = standings
        .iter()
        .filter(|r| r.rank == 1)
        .map(|r| {
            if m.team_size != 1 {
                return Ok(format!("Team {}", r.user_id));
            }
            let p = view.players.iter().find(|p| p.team == Some(r.user_id)).ok_or_else(|| anyhow!("StopIteration"))?;
            Ok(format!("<@{}>", p.user_id))
        })
        .collect::<Result<Vec<_>>>()?;
    let verdict = if names.len() == 1 { format!("Winner: {}.", names[0]) } else { format!("Tied for first: {}.", names.join(", ")) };
    let rated = if m.rated != 0 && standings.len() > 1 { " Ratings changed." } else { "" };
    Ok(format!("The {kind} is over. {verdict}{rated}"))
}

/// Discord's limit on an embed description, in characters.
const DESCRIPTION_MAX: usize = 4096;

/// A handle as a profile link, or as plain text when `linked` is false.
fn handle_of(handle: &str, linked: bool) -> String {
    if linked { profile_link(handle) } else { esc(handle) }
}

/// An unpaged description built with linked handles; built again with plain ones if it goes over Discord's limit.
fn fit(build: impl Fn(bool) -> Result<String>) -> Result<String> {
    let text = build(true)?;
    if text.chars().count() > DESCRIPTION_MAX { build(false) } else { Ok(text) }
}

/// A standings row's name: the handle, plus the team's members in a team contest.
fn who_of(view: &MashupView, r: &MashupRank, linked: bool) -> String {
    if view.mashup.team_size == 1 {
        return handle_of(&r.handle, linked);
    }
    let members = view.players.iter().filter(|p| p.team == Some(r.user_id)).map(|p| format!("<@{}>", p.user_id)).collect::<Vec<_>>();
    format!("{} ({})", esc(&r.handle), members.join(", "))
}

/// " (gave up)" for a solo player of a running contest who left.
fn gave_up(view: &MashupView, r: &MashupRank) -> Result<&'static str> {
    let m = &view.mashup;
    if m.status == "active" && m.team_size == 1 {
        let p = view.players.iter().find(|p| p.team == Some(r.user_id)).ok_or_else(|| anyhow!("StopIteration"))?;
        if !p.active {
            return Ok(" (gave up)");
        }
    }
    Ok("")
}

fn bingo_standings_lines(view: &MashupView, linked: bool) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for r in view.standings()? {
        let mut line = format!("{}\\. [{}] {} - {} squares, {} lines", r.rank, r.user_id, who_of(view, &r, linked), r.solved, r.lines);
        line += gave_up(view, &r)?;
        if view.mashup.status == "finished"
            && let Some(change) = mashup_change(view, &r)
        {
            line += &format!(", rating {change:+}");
        }
        lines.push(line);
    }
    Ok(lines)
}

fn ends_line(m: &MashupRow) -> String {
    if m.status == "finished" { format!("Ended <t:{}:f>", opt(m.ends_at)) } else { format!("Ends <t:{}:R>", opt(m.ends_at)) }
}

/// The board, the open problems by row and the standings.
fn bingo_embed(view: &MashupView, by_key: &HashMap<String, Problem>) -> Result<EmbedData> {
    let m = &view.mashup;
    let finished = m.status == "finished";
    let grid = bingo_grid(view)?;
    let head = [format!("Host: <@{}> | {} | {}", m.host_id, setup(view), ends_line(m)), format!("```\n{grid}\n```"), "**Standings** (the number in brackets is the team on the board)".into()];
    let description = fit(|linked| Ok(head.iter().cloned().chain(bingo_standings_lines(view, linked)?).collect::<Vec<_>>().join("\n")))?;
    let title = format!("{}{}", if m.team_size != 1 { "Team bingo" } else { "Bingo" }, if finished { " - final standings" } else { "" });
    let mut embed = EmbedData::new(title, Some(if finished { GREEN } else { GOLD })).description(description);
    let log = bingo_log(view)?;
    if !log.is_empty() {
        embed = embed.field("Solve log", log, false);
    }
    if !finished {
        let board = view.board()?;
        for r in 0..m.size {
            // open problems by row, linked (a field holds 1024 characters)
            let row: Vec<String> = row_of(&board.squares, r, m.size)
                .iter()
                .filter(|(_, _, team)| team.is_none() || m.mode == "replace")
                .map(|(key, rating, _)| match by_key.get(key) {
                    Some(p) => format!("[{key}]({}) {rating}", p.url()),
                    None => format!("{key} {rating}"),
                })
                .collect();
            if !row.is_empty() {
                embed = embed.field(format!("Row {}", r + 1), row.join(" | "), false);
            }
        }
    }
    Ok(embed)
}

fn standings_lines(view: &MashupView, letters: &HashMap<&str, char>, linked: bool) -> Result<Vec<String>> {
    let m = &view.mashup;
    let team_solves = view.team_solves()?;
    let mut lines = Vec::new();
    for r in view.standings()? {
        // Python sorts the times dict (insertion order) by time; ties keep the order of the team's solves
        let mut done: Vec<(&str, i64)> =
            solves_of(&team_solves, r.user_id).iter().filter_map(|(k, _)| r.times.get(k).map(|t| (k.as_str(), *t))).collect();
        done.sort_by_key(|&(_, t)| t);
        let mut parts = Vec::new();
        for (k, t) in done {
            let l = letters.get(k).ok_or_else(|| anyhow!("KeyError {k}"))?;
            let w = r.wrong.get(k).ok_or_else(|| anyhow!("KeyError {k}"))?;
            parts.push(format!("{l} {}m{}", t.div_euclid(60), if *w != 0 { format!(" ({w} wrong)") } else { String::new() }));
        }
        let detail = if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) };
        let mut line = format!("{}\\. {} - {} solved, {}{detail}", r.rank, who_of(view, &r, linked), r.solved, format_hm(r.total_seconds));
        line += gave_up(view, &r)?;
        if m.status == "finished"
            && let Some(change) = mashup_change(view, &r)
        {
            line += &format!(", rating {change:+}");
        }
        lines.push(line);
    }
    Ok(lines)
}

pub fn mashup_embed(view: &MashupView, by_key: &HashMap<String, Problem>) -> Result<EmbedData> {
    let m = &view.mashup;
    let players = format!("{}/{} players", view.players.len(), m.max_players);
    if m.status == "cancelled" {
        let note = view.note.as_deref().filter(|n| !n.is_empty()).unwrap_or("The contest was cancelled.");
        let title = if m.mode != "contest" { "Bingo cancelled" } else { "Mashup cancelled" };
        return Ok(EmbedData::new(title, Some(RED)).description(format!("{note}\nHost: <@{}>", m.host_id)));
    }
    if m.status == "lobby" {
        let title = format!("{}{} lobby", if m.team_size != 1 { "Team " } else { "" }, if m.mode != "contest" { "bingo" } else { "mashup" });
        let expires = m.created_at.checked_add(MASHUP_LOBBY_TTL_SECONDS).ok_or_else(overflow)?;
        let join = if m.team_size != 1 { format!("Join asks for a team number (1-{}). ", m.teams) } else { String::new() };
        let description = fit(|linked| {
            let user = |p: &MashupPlayer| format!("<@{}> ({})", p.user_id, handle_of(&p.handle, linked));
            let names = if m.team_size != 1 {
                (1..=m.teams)
                    .filter(|t| view.players.iter().any(|p| p.team == Some(*t)))
                    .map(|t| {
                        let members: Vec<String> = view.players.iter().filter(|p| p.team == Some(t)).map(user).collect();
                        format!("Team {t}: {}", members.join(", "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                view.players.iter().map(user).collect::<Vec<_>>().join("\n")
            };
            Ok(format!(
                "Host: <@{}>\n{}\n{players}:\n{names}\nExpires <t:{expires}:R>. {join}The host presses Start; nobody can join after that.",
                m.host_id,
                setup(view)
            ))
        })?;
        // Python `str.capitalize()`: first letter upper, the rest lower; only "Team ..." / "bingo" / "mashup" occur
        let mut chars = title.chars();
        let title = chars.next().map(|c| c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()).unwrap_or_default();
        return Ok(EmbedData::new(title, Some(BLURPLE)).description(description));
    }
    if m.mode != "contest" {
        return bingo_embed(view, by_key);
    }
    let letters = letters(view)?;
    let finished = m.status == "finished";
    let mut lines = vec![format!("Host: <@{}> | {} | {}", m.host_id, setup(view), ends_line(m)), String::new(), "**Problems**".into()];
    for (key, rating) in &view.problems {
        let name = match by_key.get(key) {
            Some(p) => format!("[{key} {}]({})", esc(&p.name), p.url()),
            None => key.clone(),
        };
        lines.push(format!("{}. {name} ({rating})", letters[key.as_str()]));
    }
    lines.extend(["".into(), "**Standings**".into()]);
    let description = fit(|linked| Ok(lines.iter().cloned().chain(standings_lines(view, &letters, linked)?).collect::<Vec<_>>().join("\n")))?;
    let title = format!("{}{}", if m.team_size == 1 { "Mashup contest" } else { "Team mashup" }, if finished { " - final standings" } else { "" });
    Ok(EmbedData::new(title, Some(if finished { GREEN } else { GOLD })).description(description))
}
