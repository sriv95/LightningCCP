//! Port of src/cogs/contests.py. s14 (lines 1-260): the regex check, the persistent Add to calendar and
//! Notify me buttons, `/contests upcoming|running|recent` and `/ranklist`. s15 adds the rest below the marker.
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fancy_regex::Regex;
use poise::serenity_prelude as serenity;
use serenity::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateAttachment, CreateButton, CreateEmbed,
    CreateInteractionResponse, CreateInteractionResponseFollowup, CreateInteractionResponseMessage, GuildId,
    ReactionType,
};

use crate::bot::{Context, DiscordOutput};
use crate::cf::models::{Contest, Standing};
use crate::errors::{BotError, Interaction, SendFailed, report_error};
use crate::logic::base::{contest_ics, select_contests};
use crate::services::Services;
use crate::store::base::Reminder;
use crate::ui::embeds::a::{RankEntry, contest_pages, ranklist_pages};

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
    // fancy-regex (user decision): only the "Invalid regex: " prefix is byte-exact, the detail may differ
    Regex::new(pattern).map_err(|e| plain(format!("Invalid regex: {e}")))?;
    Ok(pattern.to_string())
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

/// Python `ContestButton.template` `(?P<action>cal|notify):(?P<contest>\d+)`, full match. A number too big for
/// `i64` saturates: no contest has that id, so the handler answers "not upcoming any more" like Python.
pub fn parse_custom_id(custom_id: &str) -> Option<(Action, i64)> {
    let (action, number) = custom_id.split_once(':')?;
    let action = match action {
        "cal" => Action::Cal,
        "notify" => Action::Notify,
        _ => return None,
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((action, number.parse().unwrap_or(i64::MAX)))
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
        todo!("TODO(s18): self.contests.contests() once Services holds the ContestCache")
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

/// Python `ContestButton.callback` (matched by `from_custom_id`): for the component interaction event handler
/// of s18. True when the custom id is `cal:<id>` or `notify:<id>` and the press was handled, false to let the
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

// ---- s15: src/cogs/contests.py from line 260 (channel group, reminder/new-contest/recap/weekly loops) ----
