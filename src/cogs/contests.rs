//! Port of src/cogs/contests.py: the regex check, the persistent Add to calendar and Notify me buttons,
//! `/contests upcoming|running|recent`, `/ranklist`, the channel group and the reminder, new-contest, recap and weekly loops.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};

use fancy_regex::RegexBuilder;
use poise::serenity_prelude as serenity;
use serenity::{
    ButtonStyle, ChannelId, ComponentInteraction, CreateActionRow, CreateAllowedMentions, CreateAttachment, CreateButton,
    CreateEmbed, CreateInteractionResponse, CreateMessage, CreateInteractionResponseFollowup, CreateInteractionResponseMessage, GuildId,
    ReactionType,
};

use crate::bot::{Context, DiscordOutput};
use crate::cf::models::{Contest, Standing};
use crate::errors::{BotError, Interaction, SendFailed, report_error};
use crate::logic::base::{
    NEW_CONTEST_SOON_SECONDS, contest_ics, new_contests, parse_digits, parse_minutes, recaps_due, reminders_due, select_contests,
    soon_contests, week_start, weekly_board,
};
use crate::services::Services;
use crate::store::base::{Reminder, Store};
use crate::ui::embeds::a::{
    EmbedData, RankEntry, contest_pages, esc, new_contest_embed, ranklist_pages, recap_embed, weekly_embed,
};

pub const MAX_EXCLUDE_LENGTH: usize = 100;
const PAGE_SIZE: usize = 10; // Python default `per_page` of `contest_pages` and `ranklist_pages`
const RECENT_LIMIT: usize = 50; // Python default `limit` of `select_contests`
const ALARM_MINS: i64 = 10; // Python default `alarm_mins` of `contest_ics`

pub const TITLES: [(&str, &str); 3] = [
    ("upcoming", "Upcoming contests"),
    ("running", "Running contests"),
    ("recent", "Recently finished contests"),
];

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

/// Python `str.strip()` whitespace: Unicode White_Space plus U+001C..U+001F (logic::base keeps its copy private).
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Python `_check_regex`: the trimmed pattern, or the user-facing error. Python counts code points.
pub fn check_regex(raw: &str) -> anyhow::Result<String> {
    let pattern = py_strip(raw);
    if pattern.chars().count() > MAX_EXCLUDE_LENGTH {
        return Err(plain(format!("The regex can be at most {MAX_EXCLUDE_LENGTH} characters.")));
    }
    // Python's errors first (fancy-regex accepts some patterns `re` rejects), then fancy-regex's own.
    // Only the "Invalid regex: " prefix is byte-exact (user decision): the detail may differ.
    let clean = py_to_fancy::strip_noise(pattern);
    match py_re::check(&clean) {
        Ok(()) => {}
        Err(py_re::Error::Re(detail)) => return Err(plain(format!("Invalid regex: {detail}"))),
        // Python raises OverflowError here, which `except re.error` misses: the user sees "Something went wrong".
        Err(py_re::Error::Overflow) => anyhow::bail!("the repetition number is too large"),
    }
    delegate(&clean).build().map_err(|e| plain(format!("Invalid regex: {e}")))?;
    Ok(pattern.to_string())
}

fn delegate(python_pattern: &str) -> RegexBuilder {
    // default delegate size limit (regex's 10 MiB): a raised one lets one user's pattern spike the bot's memory
    RegexBuilder::new(&py_to_fancy::translate(python_pattern))
}

/// Python `_not_excluded`: drop contests whose name matches the regex (`re.IGNORECASE`, `search`), except the ids
/// in `keep`. A pattern that does not compile is an error, like `re.compile` in Python.
pub fn not_excluded(contests: &[Contest], pattern: &str, keep: &HashSet<i64>) -> anyhow::Result<Vec<Contest>> {
    if pattern.is_empty() {
        return Ok(contests.to_vec());
    }
    Ok(drop_matching(contests, &exclude_regex(pattern)?, keep))
}

fn exclude_regex(pattern: &str) -> anyhow::Result<fancy_regex::Regex> {
    let clean = py_to_fancy::strip_noise(pattern);
    // Python's `re.compile` rejects these (re.error or OverflowError, neither a BotError); fancy-regex alone would accept some.
    if py_re::check(&clean).is_err() {
        anyhow::bail!("pattern does not compile in Python's re");
    }
    Ok(delegate(&clean).case_insensitive(true).build()?)
}

fn drop_matching(contests: &[Contest], rx: &fancy_regex::Regex, keep: &HashSet<i64>) -> Vec<Contest> {
    // shortcut: a match that exceeds fancy-regex's backtrack limit counts as no match (Python would just run long)
    contests.iter().filter(|c| keep.contains(&c.id) || !rx.is_match(&c.name).unwrap_or(false)).cloned().collect()
}

/// What Python's `re.compile` rejects that fancy-regex accepts (a port must not take what Python refused): a
/// scanner over Python 3.12 pattern syntax. It only adds rejections; fancy-regex still judges the rest.
mod py_re {
    use std::collections::{HashMap, HashSet};

    pub enum Error {
        Re(String),
        Overflow,
    }
    type R<T> = Result<T, Error>;
    /// (min, max) width; `INF` stands for an unbounded repeat.
    type W = (u64, u64);
    const INF: u64 = 1 << 40;
    /// `_sre.MAXREPEAT` on 64-bit builds.
    const MAXREPEAT: u64 = 4_294_967_295;

    enum Item {
        Code(u32),
        Set,
        Unknown,
    }

    pub fn check(pattern: &str) -> R<()> {
        let mut p = Scan { s: pattern.chars().collect(), i: 0, groups: 0, names: HashMap::new(), widths: HashMap::new(), open: HashSet::new(), cond: vec![] };
        p.alt(true)?;
        if p.peek().is_some() {
            return p.err("unbalanced parenthesis");
        }
        if p.cond.iter().any(|&g| g > p.groups) {
            return p.err("invalid group reference");
        }
        Ok(())
    }

    struct Scan {
        s: Vec<char>,
        i: usize,
        groups: usize,
        names: HashMap<String, usize>,
        widths: HashMap<usize, W>,
        open: HashSet<usize>,
        cond: Vec<usize>,
    }

    fn merge(branches: &[W]) -> W {
        (branches.iter().map(|w| w.0).min().unwrap_or(0), branches.iter().map(|w| w.1).max().unwrap_or(0))
    }

    fn is_ident(name: &str) -> bool {
        let mut chars = name.chars();
        chars.next().is_some_and(|c| c == '_' || c.is_alphabetic()) && chars.all(|c| c == '_' || c.is_alphanumeric())
    }

    impl Scan {
        fn err<T>(&self, msg: &str) -> R<T> {
            Err(Error::Re(format!("{msg} at position {}", self.i)))
        }

        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }

        fn next(&mut self) -> Option<char> {
            let c = self.peek();
            self.i += usize::from(c.is_some());
            c
        }

        fn eat(&mut self, c: char) -> bool {
            let hit = self.peek() == Some(c);
            self.i += usize::from(hit);
            hit
        }

        /// The branches up to a `)` or the end. `top`: this is the whole pattern, where global flags may start.
        fn alt(&mut self, top: bool) -> R<Vec<W>> {
            let mut branches = vec![];
            loop {
                branches.push(self.seq(top && branches.is_empty())?);
                if !self.eat('|') {
                    return Ok(branches);
                }
            }
        }

        /// A group body and its `)`.
        fn body(&mut self) -> R<W> {
            let w = merge(&self.alt(false)?);
            if !self.eat(')') {
                return self.err("missing ), unterminated subpattern");
            }
            Ok(w)
        }

        fn seq(&mut self, global_ok: bool) -> R<W> {
            let (mut total, mut empty) = ((0u64, 0u64), true);
            while let Some(c) = self.peek() {
                if c == '|' || c == ')' {
                    break;
                }
                self.i += 1;
                let item = match c {
                    '(' => self.group(global_ok && empty)?,
                    '[' => {
                        self.class()?;
                        Some(((1, 1), true))
                    }
                    '\\' => Some(self.escape()?),
                    '^' | '$' => Some(((0, 0), false)),
                    '*' | '+' | '?' => return self.err("nothing to repeat"),
                    '{' => {
                        self.i -= 1;
                        if self.bounds()?.is_some() {
                            return self.err("nothing to repeat");
                        }
                        self.i += 1;
                        Some(((1, 1), true))
                    }
                    _ => Some(((1, 1), true)),
                };
                let Some((mut w, repeatable)) = item else { continue };
                empty = false;
                let mut repeated = false;
                loop {
                    let bounds = match self.peek() {
                        Some('*') => Some((0, INF)),
                        Some('+') => Some((1, INF)),
                        Some('?') => Some((0, 1)),
                        Some('{') => self.bounds()?.inspect(|_| self.i -= 1),
                        _ => None,
                    };
                    let Some((min, max)) = bounds else { break };
                    self.i += 1;
                    if !repeatable {
                        return self.err("nothing to repeat");
                    }
                    if repeated {
                        return self.err("multiple repeat");
                    }
                    if !self.eat('?') {
                        self.eat('+'); // possessive
                    }
                    repeated = true;
                    w = (min.saturating_mul(w.0).min(INF), if w.1 == 0 { 0 } else { max.saturating_mul(w.1).min(INF) });
                }
                total = (total.0.saturating_add(w.0).min(INF), total.1.saturating_add(w.1).min(INF));
            }
            Ok(total)
        }

        /// At a `{`: a repeat `{m}`, `{m,}`, `{,n}`, `{m,n}` (consumed up to and including `}`), or None and nothing
        /// consumed when Python reads the brace as a literal.
        fn bounds(&mut self) -> R<Option<(u64, u64)>> {
            let start = self.i;
            self.i += 1;
            if self.peek() == Some('}') {
                self.i = start;
                return Ok(None);
            }
            let digits = |me: &mut Scan| {
                let mut d = String::new();
                while let Some(c) = me.peek().filter(char::is_ascii_digit) {
                    d.push(c);
                    me.i += 1;
                }
                d
            };
            let lo = digits(self);
            let hi = if self.eat(',') { digits(self) } else { lo.clone() };
            if !self.eat('}') {
                self.i = start;
                return Ok(None);
            }
            let num = |d: &str| d.parse::<u64>().ok().filter(|&n| n < MAXREPEAT).ok_or(Error::Overflow);
            let min = if lo.is_empty() { 0 } else { num(&lo)? };
            let max = if hi.is_empty() { INF } else { num(&hi)? };
            if max < min {
                return self.err("min repeat greater than max repeat");
            }
            Ok(Some((min, max)))
        }

        fn hex(&mut self, n: usize) -> R<u32> {
            let mut v = 0u32;
            for _ in 0..n {
                let Some(d) = self.peek().and_then(|c| c.to_digit(16)).filter(|_| self.peek().is_some_and(|c| c.is_ascii_hexdigit())) else {
                    return self.err("incomplete escape");
                };
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
            }
            Ok(v)
        }

        /// After `\N`: `{name}`. The name is not looked up (no Unicode name table here).
        fn named(&mut self) -> R<()> {
            if !self.eat('{') {
                return self.err("missing {");
            }
            let mut len = 0;
            loop {
                match self.next() {
                    None => return self.err("missing }"),
                    Some('}') => break,
                    Some(_) => len += 1,
                }
            }
            if len == 0 { self.err("missing character name") } else { Ok(()) }
        }

        fn octal(&mut self, first: char) -> R<u32> {
            let mut v = first.to_digit(8).unwrap_or(0);
            for _ in 0..2 {
                let Some(d) = self.peek().filter(|c| ('0'..='7').contains(c)).and_then(|c| c.to_digit(8)) else { break };
                v = v * 8 + d;
                self.i += 1;
            }
            if v > 0o377 { self.err("octal escape value outside of range 0-0o377") } else { Ok(v) }
        }

        /// After a `\` outside a class: width and whether a repeat may follow (anchors cannot be repeated).
        fn escape(&mut self) -> R<(W, bool)> {
            let Some(c) = self.next() else { return self.err("bad escape (end of pattern)") };
            let one = Ok(((1, 1), true));
            match c {
                'A' | 'Z' | 'b' | 'B' => Ok(((0, 0), false)),
                'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'a' | 'f' | 'n' | 'r' | 't' | 'v' => one,
                'x' => self.hex(2).and(one),
                'u' => self.hex(4).and(one),
                'U' => self.code32().and(one),
                'N' => self.named().and(one),
                '0' => self.octal(c).and(one),
                '1'..='9' => {
                    let mut digits = c.to_string();
                    if let Some(d2) = self.peek().filter(char::is_ascii_digit) {
                        self.i += 1;
                        digits.push(d2);
                        let is_oct = |c: char| ('0'..='7').contains(&c);
                        if is_oct(c) && is_oct(d2) && self.peek().is_some_and(is_oct) {
                            self.i -= 2;
                            return self.octal(c).and(one);
                        }
                    }
                    let n: usize = digits.parse().unwrap_or(usize::MAX);
                    if n > self.groups {
                        return self.err(&format!("invalid group reference {n}"));
                    }
                    if self.open.contains(&n) {
                        return self.err("cannot refer to an open group");
                    }
                    Ok((self.widths[&n], true))
                }
                c if c.is_ascii_alphabetic() => self.err(&format!("bad escape \\{c}")),
                _ => one,
            }
        }

        fn code32(&mut self) -> R<u32> {
            let v = self.hex(8)?;
            if v > 0x10FFFF { self.err("bad escape \\U") } else { Ok(v) }
        }

        /// After `[`.
        fn class(&mut self) -> R<()> {
            self.eat('^');
            let mut first = true;
            loop {
                let Some(c) = self.next() else { return self.err("unterminated character set") };
                if c == ']' && !first {
                    return Ok(());
                }
                first = false;
                let lhs = self.class_item(c)?;
                if self.eat('-') {
                    let Some(t) = self.next() else { return self.err("unterminated character set") };
                    if t == ']' {
                        return Ok(());
                    }
                    match (lhs, self.class_item(t)?) {
                        (Item::Code(a), Item::Code(b)) if a > b => return self.err("bad character range"),
                        (Item::Set, _) | (_, Item::Set) => return self.err("bad character range"),
                        _ => {}
                    }
                }
            }
        }

        fn class_item(&mut self, c: char) -> R<Item> {
            if c != '\\' {
                return Ok(Item::Code(c as u32));
            }
            let Some(e) = self.next() else { return self.err("bad escape (end of pattern)") };
            Ok(match e {
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => Item::Set,
                'a' => Item::Code(7),
                'b' => Item::Code(8),
                'f' => Item::Code(12),
                'n' => Item::Code(10),
                'r' => Item::Code(13),
                't' => Item::Code(9),
                'v' => Item::Code(11),
                'x' => Item::Code(self.hex(2)?),
                'u' => Item::Code(self.hex(4)?),
                'U' => Item::Code(self.code32()?),
                'N' => {
                    self.named()?;
                    Item::Unknown
                }
                '0'..='7' => Item::Code(self.octal(e)?),
                c if c.is_ascii_alphanumeric() => return self.err(&format!("bad escape \\{c}")),
                c => Item::Code(c as u32),
            })
        }

        fn capture(&mut self, name: Option<String>) -> R<Option<(W, bool)>> {
            self.groups += 1;
            let n = self.groups;
            if let Some(name) = name {
                if self.names.insert(name, n).is_some() {
                    return self.err("redefinition of group name");
                }
            }
            self.open.insert(n);
            let w = self.body()?;
            self.open.remove(&n);
            self.widths.insert(n, w);
            Ok(Some((w, true)))
        }

        /// Text up to `end` (consumed).
        fn until(&mut self, end: char) -> R<String> {
            let mut text = String::new();
            loop {
                match self.next() {
                    None => return self.err(&format!("missing {end}, unterminated name")),
                    Some(c) if c == end => return Ok(text),
                    Some(c) => text.push(c),
                }
            }
        }

        /// After `(`: width and repeatability, or None for a comment or global flags (no item).
        fn group(&mut self, global_ok: bool) -> R<Option<(W, bool)>> {
            if !self.eat('?') {
                return self.capture(None);
            }
            let Some(c) = self.next() else { return self.err("unexpected end of pattern") };
            match c {
                '#' => {
                    while self.next() != Some(')') {
                        if self.i >= self.s.len() {
                            return self.err("missing ), unterminated comment");
                        }
                    }
                    Ok(None)
                }
                ':' | '>' => Ok(Some((self.body()?, true))),
                '=' | '!' => {
                    self.body()?;
                    Ok(Some(((0, 0), true)))
                }
                '<' => match self.next() {
                    Some('=' | '!') => {
                        let w = self.body()?;
                        if w.0 != w.1 {
                            return self.err("look-behind requires fixed-width pattern");
                        }
                        Ok(Some(((0, 0), true)))
                    }
                    _ => self.err("unknown extension ?<"),
                },
                'P' => match self.next() {
                    Some('<') => {
                        let name = self.until('>')?;
                        if !is_ident(&name) {
                            return self.err("bad character in group name");
                        }
                        self.capture(Some(name))
                    }
                    Some('=') => {
                        let name = self.until(')')?;
                        if !is_ident(&name) {
                            return self.err("bad character in group name");
                        }
                        let Some(&n) = self.names.get(&name) else { return self.err("unknown group name") };
                        if self.open.contains(&n) {
                            return self.err("cannot refer to an open group");
                        }
                        Ok(Some((self.widths[&n], true)))
                    }
                    _ => self.err("unknown extension ?P"),
                },
                '(' => {
                    let name = self.until(')')?;
                    if is_ident(&name) {
                        if !self.names.contains_key(&name) {
                            return self.err("unknown group name");
                        }
                    } else {
                        match name.parse::<usize>() {
                            Ok(n) if name.bytes().all(|b| b.is_ascii_digit()) && n > 0 => self.cond.push(n),
                            Ok(_) if name.bytes().all(|b| b.is_ascii_digit()) => return self.err("bad group number"),
                            _ => return self.err("bad character in group name"),
                        }
                    }
                    let branches = self.alt(false)?;
                    if branches.len() > 2 {
                        return self.err("conditional backref with more than two branches");
                    }
                    if !self.eat(')') {
                        return self.err("missing ), unterminated subpattern");
                    }
                    let no = branches.get(1).copied().unwrap_or((0, 0));
                    Ok(Some((merge(&[branches[0], no]), true)))
                }
                'a' | 'i' | 'L' | 'm' | 's' | 'u' | 'x' | '-' => self.flags(c, global_ok),
                _ => self.err(&format!("unknown extension ?{c}")),
            }
        }

        /// `(?aiLmsux-imsx)` and `(?...:...)`, `first` being the char after `?`.
        fn flags(&mut self, first: char, global_ok: bool) -> R<Option<(W, bool)>> {
            self.i -= 1;
            let mut on = String::new();
            while let Some(c) = self.peek().filter(|c| "aiLmsux".contains(*c)) {
                on.push(c);
                self.i += 1;
            }
            let mut off = String::new();
            let negative = self.eat('-');
            if negative {
                while let Some(c) = self.peek().filter(|c| "imsx".contains(*c)) {
                    off.push(c);
                    self.i += 1;
                }
                if off.is_empty() {
                    return self.err("missing flag");
                }
                if self.peek() != Some(':') {
                    return self.err("missing :");
                }
            }
            let _ = first;
            if on.contains('L') {
                return self.err("bad inline flags: cannot use 'L' flag with a str pattern");
            }
            if ["a", "u"].iter().filter(|f| on.contains(**f)).count() > 1 {
                return self.err("bad inline flags: flags 'a', 'u' and 'L' are incompatible");
            }
            if on.chars().any(|c| off.contains(c)) {
                return self.err("bad inline flags: flag turned on and off");
            }
            match self.next() {
                Some(':') => Ok(Some((self.body()?, true))),
                Some(')') if global_ok => Ok(None),
                Some(')') => self.err("global flags not at the start of the expression"),
                Some(_) => self.err("unknown flag"),
                None => self.err("missing -, : or )"),
            }
        }
    }
}

/// Python 3.12 `re` syntax to fancy-regex syntax, for patterns `py_re` accepted. fancy-regex reads the same text
/// differently (octal escapes, `\ `, `[` in a set, `{,n}`, `\Z`, `\<`, set operators `&&` and `--` in a class), has no
/// `(?a)` flag, rejects repeats of zero-width items, and its `(?x)` mode also drops whitespace inside a class. The
/// output never uses `(?x)`; verbose mode is applied by `strip_noise`.
mod py_to_fancy {
    #[derive(Clone, Copy)]
    struct Flags {
        ascii: bool,
        icase: bool,
    }

    struct Frame {
        open: usize,
        flags: Flags,
        look: bool,
        /// Every item so far is zero-width.
        zw: bool,
        /// Start in `out` of the last item, and whether it is zero-width (a repeat of it is rewritten).
        last: Option<(usize, bool)>,
    }

    /// `(?flags)` or `(?flags:` at `i` (just after the `?`): the on flags, off flags, terminator and the index after it.
    fn flag_group(s: &[char], mut i: usize) -> Option<(String, String, char, usize)> {
        let (mut on, mut off) = (String::new(), String::new());
        while let Some(&c) = s.get(i).filter(|c| "aiLmsux".contains(**c)) {
            on.push(c);
            i += 1;
        }
        if s.get(i) == Some(&'-') {
            i += 1;
            while let Some(&c) = s.get(i).filter(|c| "imsx".contains(**c)) {
                off.push(c);
                i += 1;
            }
        }
        let term = *s.get(i).filter(|c| **c == ')' || **c == ':')?;
        (!on.is_empty() || !off.is_empty()).then_some((on, off, term, i + 1))
    }

    /// Drops `(?#...)` comments (Python lets a quantifier follow one) and, in verbose mode (`(?x)`, `(?x:...)`),
    /// whitespace and `#` comments outside a class. Shortcut: `a +` becomes `a+`, and `a+ +` then reads as the
    /// possessive `a++` where Python says "multiple repeat"; upgrade if that ever matters.
    pub fn strip_noise(src: &str) -> String {
        let s: Vec<char> = src.chars().collect();
        let (mut out, mut i, mut verbose, mut saved) = (String::new(), 0, false, vec![]);
        while i < s.len() {
            let c = s[i];
            i += 1;
            match c {
                '\\' => {
                    out.push(c);
                    out.extend(s.get(i));
                    i += usize::from(i < s.len());
                }
                '[' => {
                    out.push(c);
                    if s.get(i) == Some(&'^') {
                        out.push('^');
                        i += 1;
                    }
                    let mut first = true;
                    while i < s.len() {
                        let d = s[i];
                        i += 1;
                        out.push(d);
                        if d == '\\' {
                            out.extend(s.get(i));
                            i += usize::from(i < s.len());
                        } else if d == ']' && !first {
                            break;
                        }
                        first = false;
                    }
                }
                '(' if s.get(i) == Some(&'?') && s.get(i + 1) == Some(&'#') => match s[i..].iter().position(|&c| c == ')') {
                    Some(n) => i += n + 1,
                    None => out.push(c), // unterminated: keep it so `py_re` reports it
                },
                '(' => match s.get(i).filter(|c| **c == '?').and_then(|_| flag_group(&s, i + 1)) {
                    Some((on, off, term, next)) => {
                        out.extend(&s[i - 1..next]);
                        i = next;
                        if term == ')' {
                            verbose |= on.contains('x');
                        } else {
                            saved.push(verbose);
                            verbose = (verbose || on.contains('x')) && !off.contains('x');
                        }
                    }
                    None => {
                        saved.push(verbose);
                        out.push(c);
                    }
                },
                ')' => {
                    out.push(c);
                    verbose = saved.pop().unwrap_or(verbose);
                }
                '#' if verbose => {
                    while i < s.len() && s[i] != '\n' {
                        i += 1;
                    }
                }
                c if verbose && " \t\n\r\x0b\x0c".contains(c) => {}
                _ => out.push(c),
            }
        }
        out
    }

    /// Item text for a class (`\d \D \s \S \w \W`): Python's `\s` also has U+001C..U+001F; `(?a)` limits to ASCII.
    fn set_body(e: char, ascii: bool) -> &'static str {
        match (e, ascii) {
            ('d', false) => r"\d",
            ('D', false) => r"\D",
            ('w', false) => r"\w",
            ('W', false) => r"\W",
            ('s', false) => r"\s\x{1c}-\x{1f}",
            ('S', false) => r"[^\s\x{1c}-\x{1f}]",
            ('d', true) => "0-9",
            ('D', true) => "[^0-9]",
            ('w', true) => "a-zA-Z0-9_",
            ('W', true) => "[^a-zA-Z0-9_]",
            ('s', true) => r"\x{20}\x{9}-\x{d}",
            _ => r"[^\x{20}\x{9}-\x{d}]",
        }
    }

    fn code(c: u32) -> String {
        format!(r"\x{{{c:x}}}")
    }

    /// A literal code point. `(?a)` with ignore-case folds ASCII letters only: fancy-regex's `(?i)` would also fold
    /// `K` (Kelvin sign) and `s` (long s) to ASCII.
    fn lit(c: u32, f: Flags) -> String {
        match char::from_u32(c) {
            Some(ch) if f.ascii && f.icase && ch.is_ascii_alphabetic() => format!("(?-i:[{}{}])", ch.to_ascii_lowercase(), ch.to_ascii_uppercase()),
            Some(ch) if f.ascii && ch as u32 > 127 => format!("(?-i:{ch})"),
            Some(ch) if ch.is_ascii_alphanumeric() || c > 127 => ch.to_string(),
            _ => code(c),
        }
    }

    enum Item {
        Code(u32),
        Set(String),
    }

    struct T {
        s: Vec<char>,
        i: usize,
        out: String,
        frames: Vec<Frame>,
    }

    pub fn translate(src: &str) -> String {
        let flags = Flags { ascii: false, icase: true };
        let mut t = T { s: src.chars().collect(), i: 0, out: String::new(), frames: vec![Frame { open: 0, flags, look: false, zw: true, last: None }] };
        while let Some(c) = t.next() {
            t.step(c);
        }
        t.out
    }

    impl T {
        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }

        fn next(&mut self) -> Option<char> {
            let c = self.peek();
            self.i += usize::from(c.is_some());
            c
        }

        fn top(&mut self) -> &mut Frame {
            self.frames.last_mut().expect("the root frame stays")
        }

        fn flags(&self) -> Flags {
            self.frames.last().map_or(Flags { ascii: false, icase: true }, |f| f.flags)
        }

        /// Append an item and remember it as the target of a following repeat.
        fn put(&mut self, text: &str, zw: bool) {
            let start = self.out.len();
            self.out.push_str(text);
            let top = self.top();
            top.zw &= zw;
            top.last = Some((start, zw));
        }

        fn open(&mut self, text: &str, look: bool, flags: Flags) {
            let open = self.out.len();
            self.out.push_str(text);
            self.frames.push(Frame { open, flags, look, zw: true, last: None });
        }

        fn digits(&mut self, radix: u32, max: usize) -> u32 {
            let mut v = 0u32;
            for _ in 0..max {
                let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) else { break };
                v = v.saturating_mul(radix).saturating_add(d);
                self.i += 1;
            }
            v
        }

        /// Python: `\0` plus up to two octal digits; in a class `\1`..`\7` too.
        fn octal(&mut self, first: char) -> u32 {
            let mut v = first.to_digit(8).unwrap_or(0);
            for _ in 0..2 {
                let Some(d) = self.peek().and_then(|c| c.to_digit(8)) else { break };
                v = v * 8 + d;
                self.i += 1;
            }
            v
        }

        fn step(&mut self, c: char) {
            let f = self.flags();
            match c {
                '(' => self.group(),
                ')' => {
                    self.out.push(')');
                    if self.frames.len() > 1 {
                        let g = self.frames.pop().expect("checked");
                        let zw = g.look || g.zw;
                        let top = self.top();
                        top.zw &= zw;
                        top.last = Some((g.open, zw));
                    }
                }
                '|' => {
                    self.out.push('|');
                    self.top().last = None;
                }
                '[' => {
                    let text = self.class(f);
                    self.put(&text, false);
                }
                '\\' => self.escape(f),
                '^' | '$' => self.put(&c.to_string(), true),
                '.' => self.put(".", false),
                '*' => self.quantify(c, 0, None),
                '+' => self.quantify(c, 1, None),
                '?' => self.quantify(c, 0, Some(1)),
                '{' => match self.bounds() {
                    Some((min, max)) => self.quantify('{', min, max),
                    None => self.put(&code(c as u32), false),
                },
                _ => self.put(&lit(c as u32, f), false),
            }
        }

        /// After a `{` that opens a repeat `{m}`, `{m,}`, `{,n}`, `{m,n}` (consumed through `}`), else None, nothing consumed.
        fn bounds(&mut self) -> Option<(u64, Option<u64>)> {
            let start = self.i;
            let num = |me: &mut T| {
                let mut d = String::new();
                while let Some(c) = me.peek().filter(char::is_ascii_digit) {
                    d.push(c);
                    me.i += 1;
                }
                d
            };
            let lo = num(self);
            let comma = self.peek() == Some(',');
            self.i += usize::from(comma);
            let hi = if comma { num(self) } else { lo.clone() };
            if self.peek() != Some('}') || (lo.is_empty() && hi.is_empty() && !comma) {
                self.i = start;
                return None;
            }
            self.i += 1;
            Some((lo.parse().unwrap_or(0), hi.parse().ok()))
        }

        /// A repeat of the last item. A repeat of a zero-width item (fancy-regex rejects it) is `(?:X|)` when it may match
        /// zero times, else `X`: such an item matches empty or not at all, whatever the count.
        fn quantify(&mut self, c: char, min: u64, max: Option<u64>) {
            let suffix = self.peek().filter(|c| *c == '?' || *c == '+');
            self.i += usize::from(suffix.is_some());
            if let Some((start, true)) = self.top().last {
                if min == 0 {
                    // `(?:X|)`: fancy-regex rejects `X?` for a lookaround too
                    self.out.insert_str(start, "(?:");
                    self.out.push_str("|)");
                }
                return;
            }
            match (c, max) {
                ('{', Some(m)) if m == min => self.out.push_str(&format!("{{{min}}}")),
                ('{', Some(m)) => self.out.push_str(&format!("{{{min},{m}}}")),
                ('{', None) => self.out.push_str(&format!("{{{min},}}")),
                _ => self.out.push(c),
            }
            self.out.extend(suffix);
        }

        fn escape(&mut self, f: Flags) {
            let Some(c) = self.next() else { return };
            match c {
                'A' => self.put(r"\A", true),
                'Z' => self.put(r"\z", true),
                // fancy-regex cannot switch off Unicode inline, so the ASCII boundary is spelled with lookarounds
                'b' | 'B' if f.ascii => {
                    let w = "(?-i:[a-zA-Z0-9_])";
                    let (same, diff) = (format!("(?<={w})(?={w})|(?<!{w})(?!{w})"), format!("(?<={w})(?!{w})|(?<!{w})(?={w})"));
                    self.put(&format!("(?:{})", if c == 'b' { diff } else { same }), true)
                }
                'b' | 'B' => self.put(&format!("\\{c}"), true),
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                    let body = format!("[{}]", set_body(c, f.ascii));
                    self.put(&if f.ascii { format!("(?-i:{body})") } else { body }, false);
                }
                'a' => self.put(&code(7), false),
                'f' => self.put(&code(12), false),
                'n' => self.put(&code(10), false),
                'r' => self.put(&code(13), false),
                't' => self.put(&code(9), false),
                'v' => self.put(&code(11), false),
                'x' | 'u' | 'U' => {
                    let v = self.digits(16, [('x', 2), ('u', 4), ('U', 8)].iter().find(|p| p.0 == c).map_or(2, |p| p.1));
                    self.put(&lit(v, f), false);
                }
                'N' => {
                    // shortcut: no Unicode name table, `\N{name}` stays as written (fancy-regex reads it as `\N`, any
                    // non-newline, then text); upgrade when a name table is available.
                    let start = self.i;
                    while self.next().is_some_and(|c| c != '}') {}
                    let text: String = self.s[start..self.i].iter().collect();
                    self.put(&format!("\\N{text}"), false);
                }
                '0' => {
                    let v = self.octal(c);
                    self.put(&lit(v, f), false);
                }
                '1'..='9' => {
                    let is_oct = |c: Option<char>| c.is_some_and(|c| ('0'..='7').contains(&c));
                    if is_oct(Some(c)) && is_oct(self.s.get(self.i).copied()) && is_oct(self.s.get(self.i + 1).copied()) {
                        let v = self.octal(c);
                        self.put(&lit(v, f), false);
                    } else {
                        let two = self.peek().filter(char::is_ascii_digit);
                        self.i += usize::from(two.is_some());
                        self.put(&format!("\\{c}{}", two.map(String::from).unwrap_or_default()), false);
                    }
                }
                _ => self.put(&lit(c as u32, f), false),
            }
        }

        fn class_item(&mut self, c: char) -> Item {
            if c != '\\' {
                return Item::Code(c as u32);
            }
            let Some(e) = self.next() else { return Item::Code('\\' as u32) };
            let ascii = self.flags().ascii;
            match e {
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => Item::Set(set_body(e, ascii).to_string()),
                'a' => Item::Code(7),
                'b' => Item::Code(8),
                'f' => Item::Code(12),
                'n' => Item::Code(10),
                'r' => Item::Code(13),
                't' => Item::Code(9),
                'v' => Item::Code(11),
                'x' => Item::Code(self.digits(16, 2)),
                'u' => Item::Code(self.digits(16, 4)),
                'U' => Item::Code(self.digits(16, 8)),
                '0'..='7' => Item::Code(self.octal(e)),
                'N' => {
                    let start = self.i;
                    while self.next().is_some_and(|c| c != '}') {}
                    Item::Set(format!("\\N{}", self.s[start..self.i].iter().collect::<String>()))
                }
                _ => Item::Code(e as u32),
            }
        }

        /// After `[`. Every literal becomes `\x{..}` so `[`, `&&`, `--`, `~~` stay literal. Under `(?a)` the class is
        /// ignore-case free and its ASCII letters are expanded to both cases by hand.
        fn class(&mut self, f: Flags) -> String {
            let neg = self.peek() == Some('^');
            self.i += usize::from(neg);
            let mut ranges: Vec<(u32, u32)> = vec![];
            let mut sets = String::new();
            let mut first = true;
            while let Some(c) = self.next() {
                if c == ']' && !first {
                    break;
                }
                first = false;
                let lhs = self.class_item(c);
                if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|n| *n != ']') {
                    self.i += 1;
                    let t = self.next().expect("checked");
                    if let (Item::Code(a), Item::Code(b)) = (&lhs, self.class_item(t)) {
                        ranges.push((*a, b));
                        continue;
                    }
                }
                match lhs {
                    Item::Code(a) => ranges.push((a, a)),
                    Item::Set(text) => sets.push_str(&text),
                }
            }
            if f.ascii && f.icase {
                for (lo, hi) in ranges.clone() {
                    for (a, b, delta) in [(97, 122, -32i64), (65, 90, 32)] {
                        let (x, y) = (lo.max(a), hi.min(b));
                        if x <= y {
                            ranges.push(((x as i64 + delta) as u32, (y as i64 + delta) as u32));
                        }
                    }
                }
            }
            let body: String = ranges.iter().map(|&(a, b)| if a == b { code(a) } else { format!("{}-{}", code(a), code(b)) }).collect();
            let class = format!("[{}{body}{sets}]", if neg { "^" } else { "" });
            if f.ascii { format!("(?-i:{class})") } else { class }
        }

        /// After `(`.
        fn group(&mut self) {
            let f = self.flags();
            if self.peek() != Some('?') {
                return self.open("(", false, f);
            }
            self.i += 1;
            match (self.peek(), self.s.get(self.i + 1).copied()) {
                (Some(':'), _) | (Some('>'), _) => {
                    let c = self.next().expect("peeked");
                    self.open(&format!("(?{c}"), false, f)
                }
                (Some('=' | '!'), _) => {
                    let c = self.next().expect("peeked");
                    self.open(&format!("(?{c}"), true, f)
                }
                (Some('<'), Some('=' | '!')) => {
                    let (a, b) = (self.next().expect("peeked"), self.next().expect("peeked"));
                    self.open(&format!("(?{a}{b}"), true, f)
                }
                (Some('P'), Some('<')) => {
                    let start = self.i;
                    while self.next().is_some_and(|c| c != '>') {}
                    let text: String = self.s[start..self.i].iter().collect();
                    self.open(&format!("(?{text}"), false, f)
                }
                (Some('P'), _) => {
                    let start = self.i;
                    while self.next().is_some_and(|c| c != ')') {}
                    let text: String = self.s[start..self.i].iter().collect();
                    self.put(&format!("(?{text}"), false)
                }
                (Some('('), _) => {
                    let start = self.i;
                    while self.next().is_some_and(|c| c != ')') {}
                    let text: String = self.s[start..self.i].iter().collect();
                    // fancy-regex reads `(?(n)b|c)` as a lookahead condition `n`: a group name needs `(?(<n>)`
                    let name = text.trim_matches(['(', ')']);
                    let text = if name.is_empty() || name.bytes().all(|b| b.is_ascii_digit()) { text } else { format!("(<{name}>)") };
                    self.open(&format!("(?{text}"), false, f)
                }
                _ => {
                    let Some((on, off, term, next)) = flag_group(&self.s, self.i) else { return };
                    self.i = next;
                    let g = Flags { ascii: f.ascii || on.contains('a'), icase: (f.icase || on.contains('i')) && !off.contains('i') };
                    let on: String = on.chars().filter(|c| !"aux".contains(*c)).collect();
                    let off: String = off.chars().filter(|c| *c != 'x').collect();
                    let text = format!("(?{on}{}{}{term}", if off.is_empty() { "" } else { "-" }, off);
                    if term == ':' {
                        self.open(&text, false, g);
                    } else {
                        if !(on.is_empty() && off.is_empty()) {
                            self.out.push_str(&text);
                        }
                        self.top().flags = g;
                    }
                }
            }
        }
    }
}

// ---- persistent buttons ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Cal,
    Notify,
}

impl Action {
    fn id(self) -> &'static str {
        match self {
            Action::Cal => "cal",
            Action::Notify => "notify",
        }
    }

    /// Python `BUTTONS`: label and emoji (SPIRAL CALENDAR PAD, BELL).
    fn label_emoji(self) -> (&'static str, &'static str) {
        match self {
            Action::Cal => ("Add to calendar", "\u{1F5D3}"),
            Action::Notify => ("Notify me", "\u{1F514}"),
        }
    }
}

/// Python `ContestButton.template` `(?P<action>cal|notify):(?P<contest>\d+)`, full match, then `int()`. A number
/// too big for `i64` saturates: no contest has that id, so the handler answers "not upcoming any more" like Python.
pub fn parse_custom_id(custom_id: &str) -> Option<(Action, i64)> {
    let (action, number) = custom_id.split_once(':')?;
    let action = match action {
        "cal" => Action::Cal,
        "notify" => Action::Notify,
        _ => return None,
    };
    Some((action, parse_digits(number)?))
}

pub fn contest_button(action: Action, contest_id: i64) -> CreateButton {
    let (label, emoji) = action.label_emoji();
    let style = if action == Action::Cal { ButtonStyle::Primary } else { ButtonStyle::Secondary };
    CreateButton::new(format!("{}:{contest_id}", action.id()))
        .label(label)
        .emoji(ReactionType::Unicode(emoji.into()))
        .style(style)
}

/// Python `_buttons`: Add to calendar, then Notify me when `notify`. (The view has no timeout: the buttons are
/// persistent and `on_component` answers them after a restart.)
pub fn contest_buttons(contest_id: i64, notify: bool) -> CreateActionRow {
    let mut buttons = vec![contest_button(Action::Cal, contest_id)];
    if notify {
        buttons.push(contest_button(Action::Notify, contest_id));
    }
    CreateActionRow::Buttons(buttons)
}

// ---- what the commands read and write ----------------------------------------------------------------

/// The caches, store and Codeforces reads of this half of the cog.
pub trait ContestData: Sync {
    /// `services.contests.contests`
    fn contests(&self) -> Arc<Vec<Contest>>;
    fn reminder(&self, guild: i64) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;
    fn add_contest_remind(&self, guild: i64, contest: i64, user: i64) -> impl Future<Output = anyhow::Result<bool>> + Send;
    fn linked_handles(&self, guild: i64) -> impl Future<Output = anyhow::Result<Vec<(i64, String)>>> + Send;
    fn standings(&self, contest: i64, handles: &[String]) -> impl Future<Output = anyhow::Result<Vec<Standing>>> + Send;
}

impl ContestData for Services {
    fn contests(&self) -> Arc<Vec<Contest>> {
        self.contests.contests()
    }
    async fn reminder(&self, guild: i64) -> anyhow::Result<Option<Reminder>> {
        self.db.get_reminder(guild).await
    }
    async fn add_contest_remind(&self, guild: i64, contest: i64, user: i64) -> anyhow::Result<bool> {
        self.db.add_contest_remind(guild, contest, user).await
    }
    async fn linked_handles(&self, guild: i64) -> anyhow::Result<Vec<(i64, String)>> {
        self.db.linked_handles(guild).await
    }
    async fn standings(&self, contest: i64, handles: &[String]) -> anyhow::Result<Vec<Standing>> {
        self.cf.standings(contest, handles).await
    }
}

/// The ephemeral first response of a button press.
pub trait ReplyOutput: Sync {
    /// `interaction.response.send_message(text, ephemeral=True)`
    fn reply(&self, text: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `interaction.response.send_message(text, file=discord.File(..., filename), ephemeral=True)`
    fn reply_file(&self, text: &str, filename: &str, bytes: Vec<u8>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// A deferred ephemeral slash command that answers with pages.
pub trait PagesOutput: Sync {
    /// `interaction.response.defer(ephemeral=True)`
    fn defer(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Python `send_pages`, including the Prev/Next life of the message.
    fn send_pages(&self, pages: Vec<CreateEmbed>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

// ---- button flows ------------------------------------------------------------------------------------

/// Python `_upcoming`.
fn upcoming<D: ContestData>(data: &D, contest_id: i64) -> anyhow::Result<Contest> {
    data.contests()
        .iter()
        .find(|c| c.id == contest_id)
        .filter(|c| c.phase == "BEFORE" && c.start_time.is_some())
        .cloned()
        .ok_or_else(|| plain("That contest is not upcoming any more."))
}

/// Python `calendar`: send the .ics file.
pub async fn calendar<D: ContestData, O: ReplyOutput>(data: &D, out: &O, contest_id: i64, now: i64) -> anyhow::Result<()> {
    let contest = upcoming(data, contest_id)?;
    let ics = contest_ics(&contest, now, ALARM_MINS)?;
    out.reply_file("Open the file to add the contest to your calendar.", &format!("contest-{contest_id}.ics"), ics.into_bytes())
        .await
}

/// Python `notify`: ping the member in the server's reminder channel when the contest reminder is posted.
pub async fn notify<D: ContestData, O: ReplyOutput>(
    data: &D,
    out: &O,
    contest_id: i64,
    guild: Option<i64>,
    user: i64,
) -> anyhow::Result<()> {
    upcoming(data, contest_id)?;
    // no guild (a DM): Python passes None to the store, which finds no reminder
    let reminder = match guild {
        Some(g) => data.reminder(g).await?,
        None => None,
    };
    let (Some(guild), Some(channel)) = (guild, reminder.and_then(|r| r.channel_id)) else {
        return Err(plain("This server has no reminder channel. An admin can set one with /channel reminder."));
    };
    let text = if data.add_contest_remind(guild, contest_id, user).await? {
        format!("You will be pinged in <#{channel}> when the reminder is posted.")
    } else {
        "You are already on the list.".to_string()
    };
    out.reply(&text).await
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Answers a button press: the interaction response of a message component, and `errors::Interaction` for
/// `report_error` (a component error keeps the original message).
struct ButtonOutput<'a> {
    ctx: &'a serenity::Context,
    interaction: &'a ComponentInteraction,
    done: AtomicBool,
}

impl ButtonOutput<'_> {
    async fn respond(&self, message: CreateInteractionResponseMessage) -> anyhow::Result<()> {
        self.interaction.create_response(self.ctx, CreateInteractionResponse::Message(message)).await?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl ReplyOutput for ButtonOutput<'_> {
    async fn reply(&self, text: &str) -> anyhow::Result<()> {
        self.respond(CreateInteractionResponseMessage::new().content(text).ephemeral(true)).await
    }

    async fn reply_file(&self, text: &str, filename: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let message = CreateInteractionResponseMessage::new()
            .content(text)
            .add_file(CreateAttachment::bytes(bytes, filename))
            .ephemeral(true);
        self.respond(message).await
    }
}

impl Interaction for ButtonOutput<'_> {
    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn is_application_command(&self) -> bool {
        false
    }

    async fn send_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.respond(CreateInteractionResponseMessage::new().content(message).ephemeral(true))
            .await
            .map_err(|_| SendFailed)
    }

    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        let followup = CreateInteractionResponseFollowup::new().content(message).ephemeral(true);
        self.interaction.create_followup(self.ctx, followup).await.map(drop).map_err(|_| SendFailed)
    }

    async fn delete_original(&self) -> Result<(), SendFailed> {
        Err(SendFailed) // never called for a component
    }
}

/// Python `ContestButton.callback` (matched by `from_custom_id`): for the component interaction event handler.
/// True when the custom id is `cal:<id>` or `notify:<id>` and the press was handled, false to let the
/// next handler look at it. Works after a restart: the id alone carries the state.
pub async fn on_component(ctx: &serenity::Context, interaction: &ComponentInteraction, data: &Services) -> bool {
    let Some((action, contest_id)) = parse_custom_id(&interaction.data.custom_id) else { return false };
    let out = ButtonOutput { ctx, interaction, done: AtomicBool::new(false) };
    let result = match action {
        Action::Cal => calendar(data, &out, contest_id, now()).await,
        Action::Notify => {
            let guild = interaction.guild_id.map(|g: GuildId| g.get() as i64);
            notify(data, &out, contest_id, guild, interaction.user.id.get() as i64).await
        }
    };
    if let Err(e) = result {
        report_error(&out, &e).await;
    }
    true
}

// ---- /contests and /ranklist -------------------------------------------------------------------------

/// Python `_list`: `kind` is upcoming, running or recent.
pub async fn list<D: ContestData, O: PagesOutput>(data: &D, out: &O, kind: &str) -> anyhow::Result<()> {
    let all = data.contests();
    if all.is_empty() {
        return Err(BotError::CacheNotReady.into());
    }
    let title = TITLES.iter().find(|(k, _)| *k == kind).map(|(_, t)| *t).ok_or_else(|| anyhow::anyhow!("unknown kind {kind}"))?;
    let pages = contest_pages(&select_contests(&all, kind, RECENT_LIMIT)?, title, PAGE_SIZE)?;
    if pages.is_empty() {
        return Err(plain(format!("No {kind} contests.")));
    }
    out.defer().await?;
    out.send_pages(pages.into_iter().map(CreateEmbed::from).collect()).await
}

/// Python `_entries`: (rank, user id, handle, points, penalty) of this server's linked members, best first.
pub async fn entries<D: ContestData>(data: &D, guild: i64, contest_id: i64) -> anyhow::Result<Vec<RankEntry>> {
    let linked = data.linked_handles(guild).await?;
    if linked.is_empty() {
        return Ok(vec![]);
    }
    // like the Python dict, a later row with the same lowercase handle wins
    let owners: std::collections::HashMap<String, i64> = linked.iter().map(|(user, h)| (h.to_lowercase(), *user)).collect();
    let handles: Vec<String> = linked.into_iter().map(|(_, h)| h).collect();
    let mut out = vec![];
    for row in data.standings(contest_id, &handles).await? {
        // A team row counts for the first member who is linked here.
        if let Some(h) = row.handles.iter().find(|h| owners.contains_key(&h.to_lowercase())) {
            out.push((row.rank, owners[&h.to_lowercase()], h.clone(), row.points, row.penalty));
        }
    }
    // chunked Codeforces calls arrive in separate rank order; Python sorts the tuples
    out.sort_by(|a, b| {
        (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)).then(a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal)).then(a.4.cmp(&b.4))
    });
    Ok(out)
}

/// Python `ranklist`.
pub async fn show_ranklist<D: ContestData, O: PagesOutput>(data: &D, out: &O, guild: i64, contest_id: i64) -> anyhow::Result<()> {
    out.defer().await?;
    let contest = data.contests().iter().find(|c| c.id == contest_id).cloned();
    if contest.as_ref().is_some_and(|c| c.phase == "BEFORE") {
        return Err(plain("That contest has not started yet."));
    }
    let entries = entries(data, guild, contest_id).await?;
    let title = match &contest {
        Some(c) => format!("Ranklist - {}", c.name),
        None => format!("Ranklist - contest {contest_id}"),
    };
    let pages = ranklist_pages(&entries, &title, PAGE_SIZE)?;
    if pages.is_empty() {
        return Err(plain("No linked member took part in that contest."));
    }
    out.send_pages(pages.into_iter().map(CreateEmbed::from).collect()).await
}

/// The real output of a slash command.
struct CtxOutput<'a>(Context<'a>);

impl PagesOutput for CtxOutput<'_> {
    async fn defer(&self) -> anyhow::Result<()> {
        Ok(self.0.defer_ephemeral().await?)
    }

    async fn send_pages(&self, pages: Vec<CreateEmbed>) -> anyhow::Result<()> {
        DiscordOutput::from_ctx(self.0)?.send_pages(pages).await
    }
}

fn guild(ctx: Context<'_>) -> anyhow::Result<i64> {
    Ok(ctx.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?.get() as i64)
}

/// List Codeforces contests
#[poise::command(slash_command, guild_only, subcommands("contests_upcoming", "contests_running", "contests_recent"))]
pub async fn contests(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// List future contests
#[poise::command(slash_command, guild_only, rename = "upcoming")]
pub async fn contests_upcoming(ctx: Context<'_>) -> anyhow::Result<()> {
    list(ctx.data().as_ref(), &CtxOutput(ctx), "upcoming").await
}

/// List contests running now
#[poise::command(slash_command, guild_only, rename = "running")]
pub async fn contests_running(ctx: Context<'_>) -> anyhow::Result<()> {
    list(ctx.data().as_ref(), &CtxOutput(ctx), "running").await
}

/// List recently finished contests
#[poise::command(slash_command, guild_only, rename = "recent")]
pub async fn contests_recent(ctx: Context<'_>) -> anyhow::Result<()> {
    list(ctx.data().as_ref(), &CtxOutput(ctx), "recent").await
}

/// Standings of linked members in a contest
#[poise::command(slash_command, guild_only)]
pub async fn ranklist(ctx: Context<'_>, #[description = "Codeforces contest id"] contest_id: i64) -> anyhow::Result<()> {
    show_ranklist(ctx.data().as_ref(), &CtxOutput(ctx), guild(ctx)?, contest_id).await
}

// ---- the channel commands -----------------------------------------------------------------------------

/// A channel option of a command: its id and whether the bot may send there (Python
/// `set.permissions_for(interaction.guild.me).send_messages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelArg {
    pub id: i64,
    pub can_send: bool,
}

impl ChannelArg {
    /// `TextChannel.mention`
    fn mention(&self) -> String {
        format!("<#{}>", self.id)
    }

    fn require_send(&self) -> anyhow::Result<()> {
        if self.can_send { Ok(()) } else { Err(plain(format!("I cannot send messages in {}.", self.mention()))) }
    }
}

/// The options of `/channel reminder`.
#[derive(Debug, Clone, Default)]
pub struct ReminderArgs {
    pub set: Option<ChannelArg>,
    pub role: Option<i64>,
    pub before: Option<String>,
    pub exclude: Option<String>,
    pub clear: bool,
    pub clear_role: bool,
}

/// Python `_reminder_text`.
pub fn reminder_text(r: &Reminder) -> String {
    let where_ = r.channel_id.map_or("no channel (use set)".to_string(), |c| format!("<#{c}>"));
    let role = r.role_id.map_or(String::new(), |id| format!(", pinging <@&{id}>"));
    let skip = if r.exclude.is_empty() { String::new() } else { format!("\nExcluding names matching `{}`.", r.exclude) };
    let mins = r.before_mins.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
    format!("Reminders: {where_}{role}, {mins} minutes before start.{skip}")
}

fn truthy(s: &Option<String>) -> bool {
    s.as_deref().is_some_and(|s| !s.is_empty())
}

/// Python `reminder_config`.
pub async fn reminder_config<O: ReplyOutput>(db: &Store, out: &O, guild: i64, a: ReminderArgs) -> anyhow::Result<()> {
    let mut current = match db.get_reminder(guild).await? {
        Some(r) => r,
        None => Reminder { guild_id: guild, channel_id: None, role_id: None, before_mins: vec![60, 10], exclude: String::new() },
    };
    if a.clear {
        if a.set.is_some() || a.role.is_some() || truthy(&a.before) || truthy(&a.exclude) || a.clear_role {
            return Err(plain("Use clear on its own."));
        }
        if !db.clear_reminder(guild).await? {
            return Err(plain("No reminder channel set."));
        }
        return out.reply("Reminders stopped.").await;
    }
    if a.set.is_none() && a.role.is_none() && a.before.is_none() && a.exclude.is_none() && !a.clear_role {
        return out.reply(&reminder_text(&current)).await;
    }
    if let Some(set) = &a.set {
        set.require_send()?;
    }
    if a.clear_role && a.role.is_some() {
        return Err(plain("Give a role or clear_role, not both."));
    }
    let mut pattern = current.exclude.clone();
    if let Some(exclude) = &a.exclude {
        pattern = if py_strip(exclude).to_lowercase() == "clear" { String::new() } else { check_regex(exclude)? };
    }
    current = Reminder {
        guild_id: guild,
        channel_id: a.set.map(|c| c.id).or(current.channel_id),
        role_id: if a.clear_role { None } else { a.role.or(current.role_id) },
        before_mins: if truthy(&a.before) { parse_minutes(a.before.as_deref().unwrap_or(""))? } else { current.before_mins },
        exclude: pattern,
    };
    db.set_reminder(guild, current.channel_id, current.role_id, &current.before_mins, &current.exclude).await?;
    out.reply(&reminder_text(&current)).await
}

/// Python `recap_config`.
pub async fn recap_config<O: ReplyOutput>(db: &Store, out: &O, guild: i64, set: Option<ChannelArg>, clear: bool) -> anyhow::Result<()> {
    if clear {
        if set.is_some() {
            return Err(plain("Use clear on its own."));
        }
        if !db.clear_recap(guild).await? {
            return Err(plain("No recap channel set."));
        }
        return out.reply("Recaps stopped.").await;
    }
    let Some(set) = set else {
        let text = match db.get_recap(guild).await? {
            Some(c) if c != 0 => format!("Recaps go to <#{c}>."),
            _ => "No recap channel set.".to_string(),
        };
        return out.reply(&text).await;
    };
    set.require_send()?;
    db.set_recap(guild, set.id).await?;
    out.reply(&format!("Recaps go to {}.", set.mention())).await
}

/// Python `leaderboard_config`; `now` is `int(time.time())`.
pub async fn leaderboard_config<O: ReplyOutput>(db: &Store, out: &O, guild: i64, set: Option<ChannelArg>, clear: bool, now: i64) -> anyhow::Result<()> {
    if clear {
        if set.is_some() {
            return Err(plain("Use clear on its own."));
        }
        if !db.clear_weekly(guild).await? {
            return Err(plain("No weekly leaderboard channel set."));
        }
        return out.reply("Weekly leaderboard stopped.").await;
    }
    let Some(set) = set else {
        let text = match db.get_weekly(guild).await? {
            Some(c) if c != 0 => format!("The leaderboard goes to <#{c}> every Sunday 00:00 UTC."),
            _ => "No weekly leaderboard channel set.".to_string(),
        };
        return out.reply(&text).await;
    };
    set.require_send()?;
    db.set_weekly(guild, set.id, week_start(now)).await?;
    out.reply(&format!("The leaderboard goes to {} every Sunday 00:00 UTC, starting next Sunday.", set.mention())).await
}

/// Python `new_contest_config`.
pub async fn new_contest_config<D: ContestData, O: ReplyOutput>(
    db: &Store,
    data: &D,
    out: &O,
    guild: i64,
    set: Option<ChannelArg>,
    clear: bool,
) -> anyhow::Result<()> {
    if clear {
        if set.is_some() {
            return Err(plain("Use clear on its own."));
        }
        if !db.clear_newcontest(guild).await? {
            return Err(plain("No new-contest channel set."));
        }
        return out.reply("New-contest posts stopped.").await;
    }
    let Some(set) = set else {
        let text = match db.get_newcontest(guild).await? {
            Some(c) if c != 0 => format!("New contests are posted in <#{c}>."),
            _ => "No new-contest channel set.".to_string(),
        };
        return out.reply(&text).await;
    };
    set.require_send()?;
    // Contests already listed are not "new": post only the ones that appear from now on.
    let known: Vec<i64> = data.contests().iter().map(|c| c.id).collect();
    db.set_newcontest(guild, set.id, &known).await?;
    out.reply(&format!("New contests are posted in {}, checked every 30 minutes.", set.mention())).await
}

impl ReplyOutput for CtxOutput<'_> {
    async fn reply(&self, text: &str) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().content(text).ephemeral(true)).await?;
        Ok(())
    }

    async fn reply_file(&self, text: &str, filename: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let reply = poise::CreateReply::default().content(text).attachment(CreateAttachment::bytes(bytes, filename)).ephemeral(true);
        self.0.send(reply).await?;
        Ok(())
    }
}

/// Python `set.permissions_for(guild.me).send_messages`. Without the guild or the bot's member in the cache the
/// answer is yes (a failed send is logged by the loops, like Python's HTTPException).
fn bot_can_send(ctx: Context<'_>, channel: &serenity::GuildChannel) -> bool {
    let Some(guild) = ctx.guild() else { return true };
    let Some(me) = guild.members.get(&ctx.framework().bot_id()) else { return true };
    guild.user_permissions_in(channel, me).send_messages()
}

fn channel_arg(ctx: Context<'_>, channel: Option<serenity::GuildChannel>) -> Option<ChannelArg> {
    channel.map(|c| ChannelArg { id: c.id.get() as i64, can_send: bot_can_send(ctx, &c) })
}

/// Channels for contest reminders, recaps and the weekly leaderboard
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("channel_reminder", "channel_recap", "channel_leaderboard", "channel_new_contest")
)]
pub async fn channel(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// Where to remind about upcoming contests; no options shows it
#[poise::command(slash_command, guild_only, rename = "reminder", required_permissions = "MANAGE_GUILD")]
pub async fn channel_reminder(
    ctx: Context<'_>,
    #[description = "Channel to post reminders in"]
    #[channel_types("Text", "News")]
    set: Option<serenity::GuildChannel>,
    #[description = "Role to ping"] role: Option<serenity::Role>,
    #[description = "Minutes before the start, comma-separated (default 10,60)"] before: Option<String>,
    #[description = "Regex on the contest name: matching contests get no reminder; clear removes it"] exclude: Option<String>,
    #[description = "Stop reminders (forgets the channel only)"] clear: Option<bool>,
    #[description = "Stop pinging the role"] clear_role: Option<bool>,
) -> anyhow::Result<()> {
    let args = ReminderArgs {
        set: channel_arg(ctx, set),
        role: role.map(|r| r.id.get() as i64),
        before,
        exclude,
        clear: clear.unwrap_or(false),
        clear_role: clear_role.unwrap_or(false),
    };
    reminder_config(&ctx.data().db, &CtxOutput(ctx), guild(ctx)?, args).await
}

/// Where to post how linked members placed after a contest; no options shows it
#[poise::command(slash_command, guild_only, rename = "recap", required_permissions = "MANAGE_GUILD")]
pub async fn channel_recap(
    ctx: Context<'_>,
    #[description = "Channel to post recaps in"]
    #[channel_types("Text", "News")]
    set: Option<serenity::GuildChannel>,
    #[description = "Stop recaps"] clear: Option<bool>,
) -> anyhow::Result<()> {
    recap_config(&ctx.data().db, &CtxOutput(ctx), guild(ctx)?, channel_arg(ctx, set), clear.unwrap_or(false)).await
}

/// Where to post the leaderboard every Sunday 00:00 UTC; no options shows it
#[poise::command(slash_command, guild_only, rename = "leaderboard", required_permissions = "MANAGE_GUILD")]
pub async fn channel_leaderboard(
    ctx: Context<'_>,
    #[description = "Channel to post the weekly leaderboard in"]
    #[channel_types("Text", "News")]
    set: Option<serenity::GuildChannel>,
    #[description = "Stop it"] clear: Option<bool>,
) -> anyhow::Result<()> {
    leaderboard_config(&ctx.data().db, &CtxOutput(ctx), guild(ctx)?, channel_arg(ctx, set), clear.unwrap_or(false), now()).await
}

/// Where to announce new contests with an Add to calendar button; no options shows it
#[poise::command(slash_command, guild_only, rename = "new-contest", required_permissions = "MANAGE_GUILD")]
pub async fn channel_new_contest(
    ctx: Context<'_>,
    #[description = "Channel to announce new contests in"]
    #[channel_types("Text", "News")]
    set: Option<serenity::GuildChannel>,
    #[description = "Stop announcements"] clear: Option<bool>,
) -> anyhow::Result<()> {
    new_contest_config(&ctx.data().db, ctx.data().as_ref(), &CtxOutput(ctx), guild(ctx)?, channel_arg(ctx, set), clear.unwrap_or(false)).await
}

// ---- the loops ----------------------------------------------------------------------------------------

/// `@tasks.loop(minutes=30)` of `new_contest_loop`.
pub const NEW_CONTEST_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// `@tasks.loop(seconds=60)` of `remind_loop` (reminders, recaps, weekly leaderboard).
pub const REMIND_INTERVAL: Duration = Duration::from_secs(60);
/// Python `RECAP_MAX_BACKOFF`: seconds between tries of a failing recap, at most.
pub const RECAP_MAX_BACKOFF: i64 = 1800;
/// Python `MAX_PINGED_MEMBERS`: a message holds 2000 characters, a mention takes about 22.
pub const MAX_PINGED_MEMBERS: usize = 50;

/// What a loop posts. `buttons` is `(contest id, notify)` of Python `_buttons`; `pings` is
/// `AllowedMentions(roles=True, users=True)` (discord.py's `everyone` default stays True, so `@everyone` in a
/// contest name pings too, as in Python).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Post {
    pub content: Option<String>,
    pub embed: Option<EmbedData>,
    pub buttons: Option<(i64, bool)>,
    pub pings: bool,
}

/// `bot.get_channel(id)` and `channel.send(...)`.
pub trait Channels: Sync {
    /// False when the channel is not known to the bot.
    fn exists(&self, channel: i64) -> bool;
    fn send(&self, channel: i64, post: Post) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// The real channels: the serenity cache and REST.
#[derive(Clone)]
pub struct DiscordChannels(pub serenity::Context);

impl Channels for DiscordChannels {
    fn exists(&self, channel: i64) -> bool {
        // Python `get_channel` also finds threads; a linear scan over the cached guilds is fine for a minute loop
        let id = ChannelId::new(channel as u64);
        let cache = &self.0.cache;
        cache.guilds().into_iter().any(|g| cache.guild(g).is_some_and(|g| g.channels.contains_key(&id) || g.threads.iter().any(|t| t.id == id)))
    }

    async fn send(&self, channel: i64, post: Post) -> anyhow::Result<()> {
        let mut message = CreateMessage::new();
        if let Some(content) = post.content {
            message = message.content(content);
        }
        if let Some(embed) = post.embed {
            message = message.embed(embed.into());
        }
        if let Some((contest, notify)) = post.buttons {
            message = message.components(vec![contest_buttons(contest, notify)]);
        }
        if post.pings {
            message = message.allowed_mentions(CreateAllowedMentions::new().everyone(true).all_users(true).all_roles(true));
        }
        ChannelId::new(channel as u64).send_message(&self.0, message).await?;
        Ok(())
    }
}

/// In-memory state of the cog: `_warned_channels` and `_recap_retry`.
#[derive(Debug, Default)]
pub struct LoopState {
    pub warned_channels: HashSet<(i64, i64)>,
    /// (guild, contest) to (next try, delay) of a recap that failed
    pub recap_retry: HashMap<(i64, i64), (i64, i64)>,
    /// compiled reminder-exclude patterns, so the 60 s tick does not recompile them (pruned to the stored ones)
    excludes: HashMap<String, fancy_regex::Regex>,
}

/// A send that failed is logged and swallowed (Python `except discord.HTTPException`).
async fn post<C: Channels>(chans: &C, channel: i64, post: Post, what: &str) {
    if let Err(e) = chans.send(channel, post).await {
        tracing::warn!("Could not send {what} to {channel}: {e:#}");
    }
}

/// Python `_send_new_contests`: each upcoming contest is announced twice in its new-contest channel, at most
/// once each per server: when it first shows up, and when it starts within 48 hours (skipped if the first post
/// already came that late).
pub async fn send_new_contests<D: ContestData, C: Channels>(db: &Store, data: &D, chans: &C, now: i64) -> anyhow::Result<()> {
    for (guild, channel) in db.all_newcontests().await? {
        if !chans.exists(channel) {
            continue;
        }
        for contest in new_contests(&data.contests(), &db.posted(guild).await?, now) {
            if !db.mark_posted(guild, contest.id).await? {
                continue;
            }
            if contest.start_time.is_some_and(|s| s - now <= NEW_CONTEST_SOON_SECONDS) {
                db.mark_soon(guild, contest.id).await?;
            }
            post_contest(chans, channel, &contest, "New contest").await;
        }
        let soon = soon_contests(&data.contests(), &db.soon_posted(guild).await?, now, NEW_CONTEST_SOON_SECONDS);
        for contest in soon {
            if db.mark_soon(guild, contest.id).await? {
                post_contest(chans, channel, &contest, "Starts within 48 hours").await;
            }
        }
    }
    Ok(())
}

async fn post_contest<C: Channels>(chans: &C, channel: i64, contest: &Contest, title: &str) {
    let p = Post { embed: Some(new_contest_embed(contest, title)), buttons: Some((contest.id, true)), ..Default::default() };
    post(chans, channel, p, "new contest").await;
}

/// Python `_send_due_reminders`. Marks come before the post: all due offsets but the smallest first, the smallest
/// last, and only its mark deciding whether to post (at most once per guild, contest and offset, across restarts).
pub async fn send_due_reminders<D: ContestData, C: Channels>(
    db: &Store,
    data: &D,
    chans: &C,
    state: &mut LoopState,
    now: i64,
) -> anyhow::Result<()> {
    let reminders = db.all_reminders().await?;
    state.excludes.retain(|k, _| reminders.iter().any(|r| &r.exclude == k));
    for reminder in reminders {
        // a reminder row may have no channel: Python looks up channel None and never finds it
        let channel = reminder.channel_id.filter(|&c| chans.exists(c));
        let Some(channel) = channel else {
            let key = (reminder.guild_id, reminder.channel_id.unwrap_or(0));
            if state.warned_channels.insert(key) {
                tracing::warn!("Reminder channel {:?} not found for guild {}", reminder.channel_id, reminder.guild_id);
            }
            continue;
        };
        let sent = db.sent_for_guild(reminder.guild_id).await?;
        let asked_rows = db.contest_reminders(reminder.guild_id).await?;
        let asked: HashMap<i64, Vec<i64>> = asked_rows.into_iter().collect();
        let keep: HashSet<i64> = asked.keys().copied().collect();
        let mut offsets: Vec<(i64, Vec<i64>)> = vec![];
        let mut contests: HashMap<i64, Contest> = HashMap::new();
        let candidates = if reminder.exclude.is_empty() {
            data.contests().to_vec()
        } else {
            if !state.excludes.contains_key(&reminder.exclude) {
                state.excludes.insert(reminder.exclude.clone(), exclude_regex(&reminder.exclude)?);
            }
            drop_matching(&data.contests(), &state.excludes[&reminder.exclude], &keep)
        };
        for (contest, before) in reminders_due(&candidates, &reminder.before_mins, &sent, now) {
            match offsets.iter_mut().find(|(id, _)| *id == contest.id) {
                Some((_, v)) => v.push(before),
                None => offsets.push((contest.id, vec![before])),
            }
            contests.insert(contest.id, contest);
        }
        for (contest_id, mut befores) in offsets {
            let contest = &contests[&contest_id];
            befores.sort_by(|a, b| b.cmp(a));
            let (last, rest) = befores.split_last().expect("an offset was recorded with a value");
            for before in rest {
                db.mark_sent(reminder.guild_id, contest_id, *before).await?;
            }
            if !db.mark_sent(reminder.guild_id, contest_id, *last).await? {
                continue;
            }
            let mut ping = reminder.role_id.filter(|&r| r != 0).map_or(String::new(), |r| format!("<@&{r}> "));
            for member in asked.get(&contest_id).map_or(&[][..], |v| &v[..v.len().min(MAX_PINGED_MEMBERS)]) {
                ping += &format!("<@{member}> ");
            }
            let content = format!(
                "{ping}**[{}]({})** starts <t:{}:R>.",
                esc(&contest.name),
                contest.url(),
                contest.start_time.unwrap_or(0)
            );
            let p = Post { content: Some(content), buttons: Some((contest_id, false)), pings: true, ..Default::default() };
            post(chans, channel, p, "reminder").await;
        }
    }
    Ok(())
}

/// Python `_send_recaps`: how linked members placed in each contest that just ended, in every server with a recap
/// channel, at most once per server and contest. A failed fetch is not marked and retried after a back-off of
/// 1 min, 2 min ... 30 min kept in `state`.
pub async fn send_recaps<D: ContestData, C: Channels>(db: &Store, data: &D, chans: &C, state: &mut LoopState, now: i64) -> anyhow::Result<()> {
    for (guild, channel) in db.all_recaps().await? {
        if !chans.exists(channel) {
            continue;
        }
        for contest in recaps_due(&data.contests(), &db.recapped(guild).await?, now) {
            let key = (guild, contest.id);
            if state.recap_retry.get(&key).map_or(0, |r| r.0) > now {
                continue; // failed lately: wait out the back-off
            }
            let found = match entries(data, guild, contest.id).await {
                Ok(found) => found,
                Err(e) => {
                    let delay = RECAP_MAX_BACKOFF.min(state.recap_retry.get(&key).map_or(30, |r| r.1) * 2);
                    state.recap_retry.insert(key, (now + delay, delay));
                    tracing::warn!("Could not fetch recap for contest {}: {e:#}", contest.id);
                    continue; // not marked: try again after the back-off
                }
            };
            state.recap_retry.remove(&key);
            if !db.mark_recapped(guild, contest.id).await? || found.is_empty() {
                continue;
            }
            let p = Post { embed: Some(recap_embed(&contest, &found)?), ..Default::default() };
            post(chans, channel, p, "recap").await;
        }
    }
    Ok(())
}

/// Python `_send_weekly`: the leaderboard of the week that just ended in every server that set a channel, at
/// most once per server and week.
pub async fn send_weekly<C: Channels>(db: &Store, chans: &C, now: i64) -> anyhow::Result<()> {
    let week = week_start(now);
    for (guild, channel, last_sent) in db.all_weeklies().await? {
        if !chans.exists(channel) || last_sent >= week {
            continue;
        }
        if !db.mark_weekly(guild, week).await? {
            continue;
        }
        let rows = weekly_board(&db.rating_events(guild).await?, week);
        if rows.is_empty() {
            continue;
        }
        post(chans, channel, Post { embed: Some(weekly_embed(&rows)), ..Default::default() }, "leaderboard").await;
    }
    Ok(())
}

/// One round of `new_contest_loop`: an error is logged, never raised.
pub async fn new_contest_tick<D: ContestData, C: Channels>(db: &Store, data: &D, chans: &C, now: i64) {
    if let Err(e) = send_new_contests(db, data, chans, now).await {
        tracing::error!("New contest loop failed: {e:#}");
    }
}

/// One round of `remind_loop`: reminders, recaps, then the weekly leaderboard, each failing alone.
pub async fn remind_tick<D: ContestData, C: Channels>(db: &Store, data: &D, chans: &C, state: &mut LoopState, now: i64) {
    if let Err(e) = send_due_reminders(db, data, chans, state, now).await {
        tracing::error!("Reminder loop failed: {e:#}");
    }
    if let Err(e) = send_recaps(db, data, chans, state, now).await {
        tracing::error!("Recap loop failed: {e:#}");
    }
    if let Err(e) = send_weekly(db, chans, now).await {
        tracing::error!("Weekly leaderboard loop failed: {e:#}");
    }
}

fn timer(period: Duration) -> tokio::time::Interval {
    let mut t = tokio::time::interval(period); // the first tick is at once, like discord.py's loop
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    t
}

/// `new_contest_loop`, spawned by `bot::start_loops` once the bot is ready (Python `wait_until_ready`); runs until aborted.
pub async fn run_new_contest_loop(services: Arc<Services>, chans: DiscordChannels) {
    let mut t = timer(NEW_CONTEST_INTERVAL);
    loop {
        t.tick().await;
        new_contest_tick(&services.db, services.as_ref(), &chans, now()).await;
    }
}

/// `remind_loop`, spawned by `bot::start_loops` once the bot is ready; runs until aborted.
pub async fn run_remind_loop(services: Arc<Services>, chans: DiscordChannels) {
    let (mut t, mut state) = (timer(REMIND_INTERVAL), LoopState::default());
    loop {
        t.tick().await;
        remind_tick(&services.db, services.as_ref(), &chans, &mut state, now()).await;
    }
}
