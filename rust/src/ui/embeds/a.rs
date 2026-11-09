//! Port of src/ui/embeds.py lines 1-460 (problem, daily, profile, contest, match, weekly, stats, ranklist,
//! stalk and history embeds) plus the shared pieces `esc`, the colours and `EmbedData`.
use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use fancy_regex::{Captures, Regex};
use serenity::all::{CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, Timestamp};

use crate::cf::models::{Contest, Problem, User};
use crate::logic::base::{DAY_SECONDS, LOBBY_TTL_SECONDS, WeeklyRow, ordinal, placement};
use crate::logic::mashup::chunked;
use crate::store::mashup::MashupRow;
use crate::store::matches::{DailyRow, MatchPlayer, MatchRow};

// discord.Colour values
pub const BLUE: u32 = 0x3498db;
pub const GREEN: u32 = 0x2ecc71;
pub const BLURPLE: u32 = 0x5865f2;
pub const ORANGE: u32 = 0xe67e22;
pub const RED: u32 = 0xe74c3c;
pub const GOLD: u32 = 0xf1c40f;
pub const TEAL: u32 = 0x1abc9c;
pub const DARK_ORANGE: u32 = 0xa84300;
pub const PURPLE: u32 = 0x9b59b6;

/// A `discord.Embed` as plain data. Like Python, an empty title, description or url is not sent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmbedData {
    pub title: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub colour: Option<u32>,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
    pub author: Option<String>,
    pub timestamp: Option<Timestamp>,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

impl EmbedData {
    pub fn new(title: impl Into<String>, colour: Option<u32>) -> Self {
        Self { title: Some(title.into()), colour, ..Self::default() }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(text.into());
        self
    }

    /// Python `add_field`.
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) -> Self {
        self.fields.push(EmbedField { name: name.into(), value: value.into(), inline });
        self
    }
}

impl From<EmbedData> for CreateEmbed {
    fn from(e: EmbedData) -> Self {
        let mut out = CreateEmbed::new();
        let truthy = |s: Option<String>| s.filter(|s| !s.is_empty());
        if let Some(v) = truthy(e.title) {
            out = out.title(v);
        }
        if let Some(v) = truthy(e.description) {
            out = out.description(v);
        }
        if let Some(v) = truthy(e.url) {
            out = out.url(v);
        }
        if let Some(c) = e.colour {
            out = out.colour(c);
        }
        for f in e.fields {
            out = out.field(f.name, f.value, f.inline);
        }
        if let Some(v) = e.footer {
            out = out.footer(CreateEmbedFooter::new(v));
        }
        if let Some(v) = e.author {
            out = out.author(CreateEmbedAuthor::new(v));
        }
        if let Some(v) = e.timestamp {
            out = out.timestamp(v);
        }
        if let Some(v) = e.thumbnail {
            out = out.thumbnail(v);
        }
        out
    }
}

// discord.utils: _URL_REGEX and _MARKDOWN_STOCK_REGEX (with _MARKDOWN_ESCAPE_COMMON), joined as
// escape_markdown does with ignore_links. Python `\s` also matches \x1c-\x1f, Rust `\s` does not.
static ESCAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    let s = r"[\s\x1c-\x1f]";
    let not_s = r"\s\x1c-\x1f";
    Regex::new(&format!(
        r#"(?m)(?:(?P<url><[^: >]+:/[^ >]+>|(?:https?|steam)://[^{not_s}<]+[^<.,:;"'\]{not_s}])|(?P<markdown>[_\\~|*`]|^>(?:>>)?{s}|\[.+\]\(.+\)|^#{{1,3}}|^{s}*-))"#
    ))
    .expect("static regex")
});

/// Text from Codeforces or the user, safe to show in a message: no markdown and no link breaking
/// brackets. `discord.utils.escape_markdown` then `[` and `]` backslashed.
pub fn esc(text: &str) -> String {
    let escaped = ESCAPE_RE.replace_all(text, |c: &Captures<'_, str>| match c.name("url") {
        Some(u) => u.as_str().to_string(),
        None => format!("\\{}", &c["markdown"]),
    });
    escaped.replace('[', "\\[").replace(']', "\\]")
}

/// Python `{x}` of an Optional int.
fn opt(v: Option<i64>) -> String {
    v.map_or_else(|| "None".into(), |v| v.to_string())
}

/// Python `f"{v:+d}"` of a value that may be None (a TypeError there).
fn signed(v: Option<i64>) -> Result<String> {
    v.map(|v| format!("{v:+}")).ok_or_else(|| anyhow!("unsupported format string passed to NoneType.__format__"))
}

/// Python `f"{x:g}"` for a float.
pub fn fmt_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x.is_infinite() {
        return format!("{sign}inf");
    }
    if x == 0.0 {
        return format!("{sign}0");
    }
    let sci = format!("{:.5e}", x.abs());
    let (mant, exp) = sci.split_once('e').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent");
    let strip = |s: &str| if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() };
    if !(-4..6).contains(&exp) {
        let e = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{}e{e}{:02}", strip(mant), exp.abs());
    }
    format!("{sign}{}", strip(&format!("{:.*}", (5 - exp) as usize, x.abs())))
}

pub fn problem_embed(problem: &Problem, footer: Option<&str>) -> EmbedData {
    let tags = problem.tags.join(", ");
    let mut e = EmbedData::new(format!("{}. {}", problem.key(), problem.name), Some(BLUE))
        .url(problem.url())
        .field("Rating", problem.rating.filter(|&r| r != 0).map_or("unrated".into(), |r| r.to_string()), true)
        .field("Tags", if tags.is_empty() { "none".into() } else { format!("||{tags}||") }, true);
    if let Some(f) = footer.filter(|f| !f.is_empty()) {
        e = e.footer(f);
    }
    e
}

fn days(n: i64) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// Python `DailyView`.
#[derive(Debug, Clone)]
pub struct DailyView {
    pub row: DailyRow,
    pub problem: Option<Problem>,
    pub delta: Option<i64>,
    pub streak: i64,
}

pub fn daily_embed(view: &DailyView) -> EmbedData {
    let row = &view.row;
    let resets = format!("<t:{}:R>", (row.day + 1) * DAY_SECONDS);
    if row.solved_at.is_none() {
        let footer = "Solve it on Codeforces, then run /daily again to claim double rating.";
        let mut e = match &view.problem {
            // dropped from the problem cache since it was issued
            None => EmbedData::new(format!("Daily: {}", row.problem_key.as_deref().unwrap_or("None")), Some(BLUE)).footer(footer),
            Some(p) => {
                let mut e = problem_embed(p, Some(footer));
                e.title = Some(format!("Daily: {}", e.title.unwrap_or_default()));
                e
            }
        };
        e.description = Some(format!("New daily problem {resets}."));
        if view.streak != 0 {
            e = e.field("Streak", days(view.streak), true);
        }
        return e;
    }
    let text = if let Some(d) = view.delta {
        format!("Daily solved: {d:+} rating (double). Next problem {resets}.")
    } else if row.problem_key.is_none() {
        format!("Today's daily is done: your first solo win counted double. Next {resets}.")
    } else {
        format!("Today's daily is already done. Next problem {resets}.")
    };
    EmbedData::new("Daily", Some(GREEN)).description(text).field("Streak", days(view.streak), true)
}

/// `display_name` is the Discord member's display name.
pub fn profile_embed(user: &User, display_name: &str) -> EmbedData {
    let rating = match user.rating {
        Some(r) => format!("{r} ({})", user.rank.as_deref().unwrap_or("None")),
        None => "unrated".into(),
    };
    let peak = user.max_rating.map_or(String::new(), |m| format!("\nPeak: {m}"));
    EmbedData::new(format!("{} - {display_name}", user.handle), Some(GREEN))
        .url(format!("https://codeforces.com/profile/{}", user.handle))
        .description(format!("Rating: {rating}{peak}"))
}

/// Arguments of Python `member_profile_embed` (keyword-only there).
pub struct MemberProfile<'a> {
    pub display_name: &'a str,
    pub rating: i64,
    pub rank: Option<i64>,
    pub ranked: i64,
    pub matches: i64,
    pub contests: i64,
    /// the rating change of each finished game
    pub changes: &'a [i64],
    pub peak: Option<i64>,
    pub streak: i64,
    pub codeforces: &'a str,
    pub contest_wins: i64,
}

pub fn member_profile_embed(m: &MemberProfile) -> EmbedData {
    let place = match m.rank.filter(|&r| r != 0) {
        Some(r) => format!("#{r} of {}", m.ranked),
        None => "unranked".into(),
    };
    let up = m.changes.iter().filter(|&&c| c > 0).count();
    let down = m.changes.iter().filter(|&&c| c < 0).count();
    let lines = [
        format!("Match rating: **{}** ({place})", m.rating),
        match m.peak.filter(|&p| p != 0) {
            Some(p) => format!("Peak: {p}"),
            None => "Peak: none yet".into(),
        },
        format!(
            "Games: {} matches, {} contests (rating up {up}, down {down}, even {})",
            m.matches,
            m.contests,
            m.changes.len() - up - down
        ),
        format!("Contest wins: {}", m.contest_wins),
        format!("Daily streak: {}", m.streak),
        format!("Codeforces: {}", m.codeforces),
    ];
    EmbedData::new(m.display_name, Some(BLURPLE)).description(lines.join("\n"))
}

fn contest_line(c: &Contest) -> String {
    let (hours, minutes) = ((c.duration.div_euclid(60)).div_euclid(60), c.duration.div_euclid(60).rem_euclid(60));
    let when = match c.start_time.filter(|&t| t != 0) {
        Some(t) => format!("<t:{t}:f> (<t:{t}:R>)"),
        None => "time not set".into(),
    };
    format!("[{}]({}) - {when} - {hours}h{minutes:02}", esc(&c.name), c.url())
}

/// Python default per_page is 10.
pub fn contest_pages(contests: &[Contest], title: &str, per_page: usize) -> Result<Vec<EmbedData>> {
    let groups = chunked(contests, per_page)?;
    let n = groups.len();
    Ok(groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            EmbedData::new(title, Some(ORANGE))
                .description(g.iter().map(contest_line).collect::<Vec<_>>().join("\n"))
                .footer(format!("Page {}/{n}", i + 1))
        })
        .collect())
}

pub fn new_contest_embed(c: &Contest, title: &str) -> EmbedData {
    EmbedData::new(title, Some(ORANGE)).description(contest_line(c))
}

/// Python `MatchView`.
#[derive(Debug, Clone)]
pub struct MatchView {
    pub r#match: MatchRow,
    pub players: Vec<MatchPlayer>,
    pub problem: Option<Problem>,
    /// new match ratings; set only for finished matches
    pub ratings: Option<HashMap<i64, i64>>,
    pub note: Option<String>,
}

/// Python `MODE_LABEL[m.mode]`: an unknown mode is a `KeyError`.
fn mode_label(m: &MatchRow) -> anyhow::Result<String> {
    Ok(match m.mode.as_str() {
        "custom" => format!("Custom ({} players max)", m.max_players),
        "solo" => "Solo".into(),
        "duel" => "Duel".into(),
        other => anyhow::bail!("KeyError: {other:?}"),
    })
}

fn player_status(p: &MatchPlayer) -> String {
    match p.solved_at {
        Some(t) => format!("solved <t:{t}:R>"),
        None if p.gave_up => "gave up".into(),
        None => "solving".into(),
    }
}

fn first_solve(players: &[MatchPlayer]) -> Option<i64> {
    players.iter().filter_map(|p| p.solved_at).min()
}

pub fn match_embed(view: &MatchView) -> anyhow::Result<EmbedData> {
    let (m, players) = (&view.r#match, &view.players);
    if m.status == "cancelled" {
        let note = view.note.as_deref().filter(|n| !n.is_empty()).unwrap_or("The match was cancelled.");
        return Ok(EmbedData::new("Match cancelled", Some(RED))
            .description(format!("{note}\nHost: <@{}> | {}", m.host_id, mode_label(m)?)));
    }
    if m.status == "lobby" {
        let lines = players.iter().map(|p| format!("<@{}> ({})", p.user_id, esc(&p.handle))).collect::<Vec<_>>().join("\n");
        let mut desc = format!("Host: <@{}>\nPlayers {}/{}:\n{lines}", m.host_id, players.len(), m.max_players);
        if let Some(i) = m.invited_id {
            desc += &format!("\nInvited: <@{i}>");
        }
        if !m.tags.is_empty() {
            desc += &format!("\nTags: {}", m.tags.replace(',', ", "));
        }
        if !m.exclude_tags.is_empty() {
            desc += &format!("\nExcluding: {}", m.exclude_tags.replace(',', ", "));
        }
        desc += &format!("\nExpires <t:{}:R>. The host presses Start.", m.created_at + LOBBY_TTL_SECONDS);
        return Ok(EmbedData::new(format!("Match lobby - {}", mode_label(m)?), Some(BLURPLE)).description(desc));
    }
    let mut e = match &view.problem {
        Some(p) => problem_embed(p, None),
        None => EmbedData::new(format!("Problem {}", m.problem_key.as_deref().unwrap_or("None")), Some(BLUE)),
    };
    e.author = Some(format!("{} match", mode_label(m)?));
    if m.status == "active" {
        let status = players.iter().map(|p| format!("<@{}>: {}", p.user_id, player_status(p))).collect::<Vec<_>>().join("\n");
        return Ok(e.field("Players", status, false).field("Ends", format!("<t:{}:R>", opt(m.expires_at)), false));
    }
    let empty = HashMap::new();
    let ratings = view.ratings.as_ref().unwrap_or(&empty);
    let results = if players.iter().all(|p| p.solved_at.is_none()) && players.iter().all(|p| p.delta.unwrap_or(0) == 0) {
        "Nobody solved it. No ratings changed.".to_string()
    } else {
        let mut ordered: Vec<&MatchPlayer> = players.iter().collect();
        ordered.sort_by_key(|p| -p.delta.unwrap_or(0));
        let rating = |p: &MatchPlayer| ratings.get(&p.user_id).copied().unwrap_or(0);
        if m.mode == "custom" {
            // the first solver wins, nobody else is placed
            let first = first_solve(players);
            ordered
                .iter()
                .map(|p| {
                    let winner = if p.solved_at == first { "Winner " } else { "" };
                    format!("{winner}<@{}> {:+} (rating {})", p.user_id, p.delta.unwrap_or(0), rating(p))
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            ordered
                .iter()
                .enumerate()
                .map(|(i, p)| format!("{}. <@{}> {:+} (rating {})", i + 1, p.user_id, p.delta.unwrap_or(0), rating(p)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    e.colour = Some(GREEN);
    e = e.field("Results", results, false);
    if let Some(n) = view.note.as_deref().filter(|n| !n.is_empty()) {
        e = e.footer(n);
    }
    Ok(e)
}

pub const STATS_SIZE: usize = 10;

/// The top STATS_SIZE of the weekly leaderboard: rating, this week's gain and places moved.
pub fn weekly_embed(rows: &[WeeklyRow]) -> EmbedData {
    let moved = |m: Option<i64>| match m {
        None => "new".to_string(),
        Some(m) if m > 0 => format!("\u{25b2}{m}"),
        Some(m) if m < 0 => format!("\u{25bc}{}", -m),
        Some(_) => "-".into(),
    };
    let text = rows
        .iter()
        .take(STATS_SIZE)
        .map(|r| format!("{}. <@{}> ({}) {} ({:+}) {}", r.rank, r.user_id, esc(&r.handle), r.rating, r.gain, moved(r.moved)))
        .collect::<Vec<_>>()
        .join("\n");
    EmbedData::new("Weekly leaderboard", Some(GOLD))
        .description(text)
        .footer("Rating, change this week, places moved since last week")
}

/// Rows: (user_id, handle, total, played). `streaks`: daily streak by user id. `kind`: "all", "matches"
/// or "contests". Pages of 10 ranked players (never empty); the caller's row is bold.
pub fn stats_pages(
    rows: &[(i64, String, i64, i64)],
    me_id: i64,
    streaks: &HashMap<i64, i64>,
    by_streak: bool,
    kind: &str,
) -> Vec<EmbedData> {
    let streak = |id: i64| streaks.get(&id).copied().unwrap_or(0);
    let noun = match kind {
        "matches" => "matches",
        "contests" => "contests",
        _ => "games",
    };
    let mut rows = rows.to_vec();
    if by_streak {
        rows.retain(|r| streak(r.0) != 0);
        rows.sort_by_key(|r| -streak(r.0)); // stable: rating order kept among equal streaks
    }
    let title = if by_streak {
        "Daily streaks"
    } else if kind == "contests" {
        "Contest stats"
    } else {
        "Match stats"
    };
    if rows.is_empty() {
        let empty = if by_streak { "No daily streaks yet.".to_string() } else { format!("No finished {noun} yet.") };
        return vec![EmbedData::new(title, Some(GOLD)).description(empty)];
    }
    let you = match rows.iter().position(|r| r.0 == me_id) {
        Some(i) => format!("You are #{}.", i + 1),
        None if by_streak => "You have no daily streak.".into(),
        None => format!("You have no finished {noun} yet."),
    };
    let line = |rank: usize, r: &(i64, String, i64, i64)| {
        let mut text = format!("{rank}. <@{}> ({}) {} ({} {noun})", r.0, esc(&r.1), r.2, r.3);
        if streak(r.0) != 0 {
            text += &format!(" \u{1f525}{}", streak(r.0));
        }
        if r.0 == me_id { format!("**{text}**") } else { text }
    };
    let n = rows.len().div_ceil(STATS_SIZE);
    rows.chunks(STATS_SIZE)
        .enumerate()
        .map(|(i, g)| {
            let text = g.iter().enumerate().map(|(j, r)| line(i * STATS_SIZE + j + 1, r)).collect::<Vec<_>>().join("\n");
            EmbedData::new(title, Some(GOLD)).description(text).footer(format!("Page {}/{n} | {you}", i + 1))
        })
        .collect()
}

/// entries: (rank, user_id, handle, points, penalty), best first.
pub type RankEntry = (i64, i64, String, f64, i64);

pub fn ranklist_pages(entries: &[RankEntry], title: &str, per_page: usize) -> Result<Vec<EmbedData>> {
    let line = |e: &RankEntry| {
        let extra = if e.4 != 0 { format!(", penalty {}", e.4) } else { String::new() };
        format!("{}. <@{}> ({}) {}{extra}", e.0, e.1, esc(&e.2), fmt_g(e.3))
    };
    let groups = chunked(entries, per_page)?;
    let n = groups.len();
    Ok(groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            EmbedData::new(title, Some(TEAL))
                .description(g.iter().map(line).collect::<Vec<_>>().join("\n"))
                .footer(format!("Page {}/{n}", i + 1))
        })
        .collect())
}

/// entries: (user_id, handle, Codeforces rating, rank), highest rating first, unrated last.
pub fn handle_list_pages(entries: &[(i64, String, Option<i64>, Option<String>)], per_page: usize) -> Result<Vec<EmbedData>> {
    let line = |i: usize, e: &(i64, String, Option<i64>, Option<String>)| {
        let info = match (e.2, e.3.as_deref().filter(|r| !r.is_empty())) {
            (Some(r), Some(rank)) => format!("{r} ({rank})"),
            _ => "unrated".into(),
        };
        format!("{i}. <@{}> ({}) {info}", e.0, esc(&e.1))
    };
    let groups = chunked(entries, per_page)?;
    let n = groups.len();
    Ok(groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let first = i * per_page + 1;
            EmbedData::new("Linked handles", Some(BLUE))
                .description(g.iter().enumerate().map(|(j, e)| line(first + j, e)).collect::<Vec<_>>().join("\n"))
                .footer(format!("Page {}/{n} - {} linked members", i + 1, entries.len()))
        })
        .collect())
}

/// The top 10 of `entries` as a recap; an empty list is an IndexError in Python.
pub fn recap_embed(contest: &Contest, entries: &[RankEntry]) -> Result<EmbedData> {
    let page = ranklist_pages(&entries[..entries.len().min(10)], &format!("Contest recap - {}", contest.name), 10)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("list index out of range"))?;
    let n = entries.len();
    Ok(page.url(contest.url()).footer(format!("{n} linked member{} took part", if n == 1 { "" } else { "s" })))
}

/// items: (problem key, problem if the cache knows it), newest attempt first.
pub fn stalk_pages(handle: &str, items: &[(String, Option<Problem>)], per_page: usize) -> Result<Vec<EmbedData>> {
    let line = |(key, problem): &(String, Option<Problem>)| match problem {
        None => key.clone(),
        Some(p) => {
            let rating = p.rating.filter(|&r| r != 0).map_or(String::new(), |r| format!(" ({r})"));
            format!("[{key}. {}]({}){rating}", esc(&p.name), p.url())
        }
    };
    let groups = chunked(items, per_page)?;
    let n = groups.len();
    Ok(groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            EmbedData::new(format!("Unsolved by {handle}"), Some(DARK_ORANGE))
                .url(format!("https://codeforces.com/profile/{handle}"))
                .description(g.iter().map(line).collect::<Vec<_>>().join("\n"))
                .footer(format!("Page {}/{n} - {} problems", i + 1, items.len()))
        })
        .collect())
}

pub type MatchRecord = (MatchRow, Vec<MatchPlayer>);

/// Python `MashupRecord`: how one player (or their team) did in a finished contest.
#[derive(Debug, Clone)]
pub struct MashupRecord {
    pub mashup: MashupRow,
    pub rank: i64,
    /// teams (a team of one is a player)
    pub entrants: i64,
    pub solved: i64,
    pub team: Option<i64>,
    /// match rating change; None when the contest did not rate
    pub delta: Option<i64>,
}

fn mode_name(mode: &str) -> &'static str {
    match mode {
        "solo" => "Solo",
        "duel" => "Duel",
        _ => "Custom",
    }
}

fn history_line(record: &MatchRecord, me_id: i64, by_key: &HashMap<String, Problem>, versus: bool) -> Result<String> {
    let (m, players) = record;
    let me = players.iter().find(|p| p.user_id == me_id).ok_or_else(|| anyhow!("StopIteration"))?;
    let name = match by_key.get(m.problem_key.as_deref().unwrap_or("")) {
        Some(p) => format!("[{}]({})", p.key(), p.url()),
        None => m.problem_key.clone().unwrap_or_else(|| "None".into()),
    };
    let head = format!("<t:{}:d> {} {name} ({})", opt(m.finished_at), mode_name(&m.mode), opt(m.rating));
    if m.mode == "solo" {
        return Ok(format!("{head} - {}, {}", if me.solved_at.is_some() { "solved" } else { "not solved" }, signed(me.delta)?));
    }
    let result = if m.mode == "custom" {
        match first_solve(players) {
            None => "no result".to_string(),
            Some(f) if me.solved_at == Some(f) => "won".into(),
            Some(_) => "lost".into(),
        }
    } else {
        let deltas: Vec<i64> = players.iter().map(|p| p.delta.unwrap_or(0)).collect();
        match placement(&deltas, me.delta.unwrap_or(0)) {
            None => "no result".to_string(),
            Some(place) => format!("{} of {}", ordinal(place), players.len()),
        }
    };
    let others: Vec<String> = players.iter().filter(|p| p.user_id != me_id).map(|p| format!("<@{}>", p.user_id)).collect();
    let more = if others.len() > 3 { format!(" +{}", others.len() - 3) } else { String::new() };
    let vs = if versus { format!(" vs {}{more}", others[..others.len().min(3)].join(", ")) } else { String::new() };
    Ok(format!("{head}{vs} - {result}, {}", signed(me.delta)?))
}

fn line_pages(title: &str, lines: &[String]) -> Result<Vec<EmbedData>> {
    Ok(chunked(lines, 10)?.iter().map(|g| EmbedData::new(title, Some(PURPLE)).description(g.join("\n"))).collect())
}

fn mashup_history_line(r: &MashupRecord) -> String {
    let m = &r.mashup;
    let name = if m.mode != "contest" { "Bingo" } else { "Mashup" };
    let kind = if m.team_size == 1 { name.to_string() } else { format!("Team {} (team {})", name.to_lowercase(), opt(r.team)) };
    let place = if r.solved != 0 { format!("{} of {}", ordinal(r.rank), r.entrants) } else { "nothing solved".into() };
    let rating = r.delta.map_or(String::new(), |d| format!(", {d:+}"));
    format!("<t:{}:d> {kind} - {place}, {}/{} solved{rating}", opt(m.ends_at), r.solved, m.count)
}

/// Matches and finished mashups together, newest first. Empty when there are none.
pub fn history_pages(
    title: &str,
    records: &[MatchRecord],
    user_id: i64,
    by_key: &HashMap<String, Problem>,
    mashups: &[MashupRecord],
) -> Result<Vec<EmbedData>> {
    let none = || anyhow!("bad operand type for unary -: 'NoneType'");
    let mut entries = Vec::new();
    for r in records {
        entries.push((r.0.finished_at.ok_or_else(none)?, history_line(r, user_id, by_key, true)?));
    }
    for r in mashups {
        entries.push((r.mashup.ends_at.ok_or_else(none)?, mashup_history_line(r)));
    }
    entries.sort_by_key(|e| -e.0); // stable: equal times keep matches first
    line_pages(title, &entries.into_iter().map(|e| e.1).collect::<Vec<_>>())
}

/// Matches both players finished, with the win/loss record by rating change. Never empty.
pub fn head_to_head_pages(
    title: &str,
    records: &[MatchRecord],
    me_id: i64,
    other_id: i64,
    by_key: &HashMap<String, Problem>,
) -> Result<Vec<EmbedData>> {
    let shared: Vec<&MatchRecord> = records.iter().filter(|r| r.1.iter().any(|p| p.user_id == other_id)).collect();
    if shared.is_empty() {
        return Ok(vec![EmbedData { title: Some(title.into()), description: Some("No matches against each other yet.".into()), ..Default::default() }]);
    }
    let (mut wins, mut losses, mut ties) = (0, 0, 0);
    for (_, players) in &shared {
        let delta = |id| players.iter().find(|p| p.user_id == id).ok_or_else(|| anyhow!("StopIteration")).map(|p| p.delta.unwrap_or(0));
        let (mine, theirs) = (delta(me_id)?, delta(other_id)?);
        wins += i32::from(mine > theirs);
        losses += i32::from(mine < theirs);
        ties += i32::from(mine == theirs);
    }
    let summary = format!("**Wins {wins}, losses {losses}, ties {ties}** ({} matches)", shared.len());
    let lines = shared.iter().map(|r| history_line(r, me_id, by_key, false)).collect::<Result<Vec<_>>>()?;
    let mut pages = line_pages(title, &lines)?;
    let first = &mut pages[0];
    first.description = Some(format!("{summary}\n{}", first.description.as_deref().unwrap_or("")));
    Ok(pages)
}

/// Python `number_pages`: footer "Page i/n" on every page.
pub fn number_pages(pages: Vec<EmbedData>) -> Vec<EmbedData> {
    let n = pages.len();
    pages.into_iter().enumerate().map(|(i, p)| p.footer(format!("Page {}/{n}", i + 1))).collect()
}

