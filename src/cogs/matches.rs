//! Port of src/cogs/match.py: `/match solo|duel|custom|history`, `/ff`, `/leaderboard`, `/profile`, the persistent
//! `match:<action>:<id>` buttons and the 60 s expiry loop. Each command is a function over the real store, the
//! `MatchService`, `MatchEnv` (the problem cache, mashups and Codeforces profile) and a narrow
//! output trait (a fake in tests). The poise commands at the bottom are thin wrappers over these functions.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use poise::serenity_prelude as serenity;
use serenity::{ButtonStyle, ChannelId, CreateActionRow, CreateButton, CreateEmbed, MessageId};

use crate::bot::{Context, DiscordOutput};
use crate::cache::problems::ProblemCache;
use crate::cf::client::CodeforcesClient;
use crate::cf::models::{Problem, User};
use crate::errors::{BotError, Interaction, SendFailed, report_error};
use crate::logic::base::{daily_streak, parse_digits, rating_series, tag_suggestions, utc_day};
use crate::matches::{CfApi, CreateMatch, MatchService, MatchView, ProblemSource};
use crate::services::Services;
use crate::store::base::Store;
use crate::ui::embeds::a::{
    EmbedData, MashupRecord, MatchRecord, MemberProfile, esc, profile_link, head_to_head_pages, history_pages, match_embed, member_profile_embed,
    number_pages, stats_pages,
};

/// `@tasks.loop(seconds=60)`
pub const EXPIRE_INTERVAL: Duration = Duration::from_secs(60);
/// Python `Store.finished_matches` default `limit`.
pub const HISTORY_LIMIT: i64 = 100;

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn pages(embeds: Vec<EmbedData>) -> Vec<CreateEmbed> {
    embeds.into_iter().map(CreateEmbed::from).collect()
}

// ---- persistent buttons ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Join,
    Start,
    Leave,
    Check,
    GiveUp,
    Rematch,
}

impl Action {
    fn id(self) -> &'static str {
        match self {
            Action::Join => "join",
            Action::Start => "start",
            Action::Leave => "leave",
            Action::Check => "check",
            Action::GiveUp => "giveup",
            Action::Rematch => "rematch",
        }
    }

    /// Python `BUTTONS`: label and style.
    pub fn label_style(self) -> (&'static str, ButtonStyle) {
        match self {
            Action::Join => ("Join", ButtonStyle::Success),
            Action::Start => ("Start", ButtonStyle::Primary),
            Action::Leave => ("Leave", ButtonStyle::Secondary),
            Action::Check => ("Check", ButtonStyle::Success),
            Action::GiveUp => ("Give Up", ButtonStyle::Danger),
            Action::Rematch => ("Rematch", ButtonStyle::Primary),
        }
    }
}

const LOBBY_ACTIONS: &[Action] = &[Action::Join, Action::Start, Action::Leave];
const ACTIVE_ACTIONS: &[Action] = &[Action::Check, Action::GiveUp];
const FINISHED_DUEL_ACTIONS: &[Action] = &[Action::Rematch];

/// `MatchButton.template` `match:(?P<action>join|start|leave|check|giveup|rematch):(?P<id>\d+)`, full match. The id
/// saturates past `i64`: no match has it, so the service answers "no longer open".
pub fn parse_custom_id(custom_id: &str) -> Option<(Action, i64)> {
    let rest = custom_id.strip_prefix("match:")?;
    let (action, number) = rest.split_once(':')?;
    let action = [Action::Join, Action::Start, Action::Leave, Action::Check, Action::GiveUp, Action::Rematch]
        .into_iter()
        .find(|a| a.id() == action)?;
    Some((action, parse_digits(number)?))
}

pub fn match_button(action: Action, match_id: i64) -> CreateButton {
    let (label, style) = action.label_style();
    CreateButton::new(format!("match:{}:{match_id}", action.id())).label(label).style(style)
}

/// Python `match_components`, as the actions: lobby, active, or a finished duel; `None` otherwise.
pub fn match_actions(view: &MatchView) -> Option<&'static [Action]> {
    let m = &view.r#match;
    match (m.status.as_str(), m.mode.as_str()) {
        ("lobby", _) => Some(LOBBY_ACTIONS),
        ("active", _) => Some(ACTIVE_ACTIONS),
        ("finished", "duel") => Some(FINISHED_DUEL_ACTIONS),
        _ => None,
    }
}

pub fn match_components(view: &MatchView) -> Option<CreateActionRow> {
    let id = view.r#match.id;
    match_actions(view).map(|actions| CreateActionRow::Buttons(actions.iter().map(|&a| match_button(a, id)).collect()))
}

// ---- what the commands read and write ----------------------------------------------------------------

/// A guild member as the cog sees one.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: i64,
    /// `display_name`
    pub name: String,
    pub bot: bool,
}

/// A message the bot just sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub id: i64,
    /// `message.jump_url`
    pub url: String,
}

/// What is not ported yet or lives in a cache: the problem cache, the mashup side and the Codeforces profile.
pub trait MatchEnv: Sync {
    /// `services.problems.problems`
    fn problems(&self) -> Arc<Vec<Problem>>;
    /// `services.problems.tags`
    fn tags(&self) -> Vec<String>;
    /// `services.cf.user_info`
    fn user_info(&self, handle: &str) -> impl Future<Output = anyhow::Result<User>> + Send;
    /// `services.mashups.history(guild, user)`
    fn contest_history(&self, guild: i64, user: i64) -> impl Future<Output = anyhow::Result<Vec<MashupRecord>>> + Send;
    /// `services.mashups.wins(guild, user)`
    fn contest_wins(&self, guild: i64, user: i64) -> impl Future<Output = anyhow::Result<i64>> + Send;
    /// The contest half of `/ff`: leave the lobby or give up the contest the user is in, refresh its message and log
    /// the give-up (`MashupCog.refresh`, `log_gave_up`). `Some(true)` for a lobby left, `Some(false)` for a give-up,
    /// `None` when the user is in no unfinished contest.
    fn forfeit_contest(&self, guild: i64, user: i64) -> impl Future<Output = anyhow::Result<Option<bool>>> + Send;
}

/// Edits of messages the bot posted earlier (Python `get_partial_messageable(..).get_partial_message(..).edit`).
pub trait Editor: Sync {
    /// `edit(embed=..., view=...)`: a `None` row removes the buttons.
    fn edit_message(
        &self,
        channel: i64,
        message: i64,
        embed: CreateEmbed,
        row: Option<CreateActionRow>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `edit(content=..., embed=None, view=None)`
    fn retire_message(&self, channel: i64, message: i64, content: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// What both kinds of interaction can do.
pub trait PostOutput: Editor {
    fn user_id(&self) -> i64;
    fn channel_id(&self) -> i64;
    /// `interaction.response.defer(ephemeral=...)`
    fn defer(&self, ephemeral: bool) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `interaction.followup.send(embed=..., view=..., wait=True)`: a public message.
    fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> impl Future<Output = anyhow::Result<Sent>> + Send;
    /// `interaction.followup.send(text, ephemeral=True)`
    fn followup_text(&self, text: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// A deferred ephemeral slash command.
pub trait SlashOutput: PostOutput {
    /// `interaction.followup.send(embed=..., ephemeral=True)`
    fn followup_embed(&self, embed: CreateEmbed) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Python `send_pages`, including the Prev/Next life of the message.
    fn send_pages(&self, pages: Vec<CreateEmbed>) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `guild.fetch_member`; `None` on `discord.NotFound`.
    fn fetch_member(&self, user: i64) -> impl Future<Output = anyhow::Result<Option<Member>>> + Send;
}

/// A button press; errors go through `report_error`, so it is also an `Interaction`.
pub trait ButtonOutput: PostOutput + Interaction {
    /// `interaction.edit_original_response(embed=..., view=...)`
    fn edit_original(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

// ---- the match flow ----------------------------------------------------------------------------------

/// Python `_edit_stored_message`: a message that cannot be edited is logged, never an error.
/// An embed that cannot be built (unknown mode, Python `KeyError`) is an error, like Python.
pub async fn edit_stored_message<E: Editor>(editor: &E, view: &MatchView) -> anyhow::Result<()> {
    let m = &view.r#match;
    let Some(message) = m.message_id else { return Ok(()) };
    let embed = match_embed(view)?;
    if let Err(e) = editor.edit_message(m.channel_id, message, embed.into(), match_components(view)).await {
        tracing::warn!("Could not edit match message {message}: {e:?}");
    }
    Ok(())
}

/// Python `_post`: send the match as a new message and remember it; retire the old one on a re-post.
async fn post<C: CfApi, P: ProblemSource, O: PostOutput>(svc: &MatchService<C, P>, out: &O, view: &MatchView) -> anyhow::Result<()> {
    let sent = out.post(match_embed(view)?.into(), match_components(view)).await?;
    let old = &view.r#match;
    svc.set_message(old.id, out.channel_id(), sent.id).await?;
    if let Some(old_message) = old.message_id.filter(|&m| m != sent.id) {
        // Re-post: retire the old message so its buttons do not go stale (failures are suppressed).
        let _ = out.retire_message(old.channel_id, old_message, &format!("This match moved to {}", sent.url)).await;
    }
    Ok(())
}

/// Python `_create`. `spec` carries mode, size, invitee, rating and tags; the place and host come from the interaction.
pub async fn create<C: CfApi, P: ProblemSource, O: PostOutput>(
    svc: &MatchService<C, P>,
    out: &O,
    guild: i64,
    spec: CreateMatch,
) -> anyhow::Result<()> {
    out.defer(false).await?;
    // Already in a match: re-post it with fresh buttons instead of creating another.
    let view = match svc.current(guild, out.user_id()).await? {
        Some(view) => view,
        None => {
            let spec = CreateMatch { guild_id: guild, channel_id: out.channel_id(), host_id: out.user_id(), ..spec };
            svc.create(spec).await?
        }
    };
    post(svc, out, &view).await
}

/// `/match solo`
pub async fn solo<C: CfApi, P: ProblemSource, O: PostOutput>(
    svc: &MatchService<C, P>,
    out: &O,
    guild: i64,
    rating: Option<i64>,
    tags: Option<String>,
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    let spec = CreateMatch { mode: "solo".into(), max_players: 1, rating_arg: rating, tags, exclude_tags, ..Default::default() };
    create(svc, out, guild, spec).await
}

/// `/match duel`
pub async fn duel<C: CfApi, P: ProblemSource, O: PostOutput>(
    svc: &MatchService<C, P>,
    out: &O,
    guild: i64,
    opponent: Option<&Member>,
    rating: Option<i64>,
    tags: Option<String>,
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    if opponent.is_some_and(|o| o.bot) {
        return Err(plain("You cannot duel a bot."));
    }
    let spec = CreateMatch {
        mode: "duel".into(),
        max_players: 2,
        invited_id: opponent.map(|o| o.id),
        rating_arg: rating,
        tags,
        exclude_tags,
        ..Default::default()
    };
    create(svc, out, guild, spec).await
}

/// `/match custom`
pub async fn custom<C: CfApi, P: ProblemSource, O: PostOutput>(
    svc: &MatchService<C, P>,
    out: &O,
    guild: i64,
    players: i64,
    rating: Option<i64>,
    tags: Option<String>,
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    let spec = CreateMatch { mode: "custom".into(), max_players: players, rating_arg: rating, tags, exclude_tags, ..Default::default() };
    create(svc, out, guild, spec).await
}

/// The three tag options' autocomplete: Python `tag_suggestions(current, problems.tags)`, 25 choices.
pub fn tag_autocomplete<E: MatchEnv>(env: &E, current: &str) -> Vec<String> {
    tag_suggestions(current, &env.tags(), 25)
}

/// The `kind` option of `/match history` and `/leaderboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, poise::ChoiceParameter)]
pub enum Kind {
    #[name = "all"]
    All,
    #[name = "matches"]
    Matches,
    #[name = "contests"]
    Contests,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::All => "all",
            Kind::Matches => "matches",
            Kind::Contests => "contests",
        }
    }
}

/// The `kind` option as sent (a string choice); Python `Literal` has only the three values.
fn kind_of(kind: Option<String>) -> Kind {
    kind.and_then(|k| poise::ChoiceParameter::from_name(&k)).unwrap_or(Kind::All)
}

/// The `sort` option of `/leaderboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, poise::ChoiceParameter)]
pub enum Sort {
    #[name = "rating"]
    Rating,
    #[name = "streak"]
    Streak,
}

async fn finished(db: &Store, guild: i64, user: i64, kind: Kind) -> anyhow::Result<Vec<MatchRecord>> {
    if kind == Kind::Contests { Ok(vec![]) } else { db.finished_matches(guild, user, HISTORY_LIMIT).await }
}

async fn finished_contests<E: MatchEnv>(env: &E, guild: i64, user: i64, kind: Kind) -> anyhow::Result<Vec<MashupRecord>> {
    if kind == Kind::Matches { Ok(vec![]) } else { env.contest_history(guild, user).await }
}

/// `/match history`
pub async fn history<E: MatchEnv, O: SlashOutput>(
    db: &Store,
    env: &E,
    out: &O,
    guild: i64,
    me: &Member,
    member: Option<&Member>,
    kind: Kind,
) -> anyhow::Result<()> {
    out.defer(true).await?;
    let by_key: HashMap<String, Problem> = env.problems().iter().map(|p| (p.key(), p.clone())).collect();
    let none = match kind {
        Kind::All => "matches or mashups",
        Kind::Matches => "matches",
        Kind::Contests => "contests",
    };
    let mine = finished(db, guild, me.id, kind).await?;
    let result = match member.filter(|m| m.id != me.id) {
        None => {
            let contests = finished_contests(env, guild, me.id, kind).await?;
            let result = history_pages(&format!("Match history - {}", me.name), &mine, me.id, &by_key, &contests)?;
            if result.is_empty() {
                return Err(plain(format!("You have no finished {none} yet.")));
            }
            result
        }
        Some(member) => {
            let mut result = if kind != Kind::Contests {
                head_to_head_pages(&format!("You vs {}", member.name), &mine, me.id, member.id, &by_key)?
            } else {
                vec![]
            };
            let theirs = finished(db, guild, member.id, kind).await?;
            let contests = finished_contests(env, guild, member.id, kind).await?;
            let title = format!("Match history - {}", member.name);
            let mut own = history_pages(&title, &theirs, member.id, &by_key, &contests)?;
            if own.is_empty() {
                own.push(EmbedData {
                    title: Some(title),
                    description: Some(format!("No finished {none} yet.")),
                    ..Default::default()
                });
            }
            result.extend(own);
            result
        }
    };
    out.send_pages(pages(number_pages(result))).await
}

/// `/ff`: leave a lobby or give up, for the match or contest the user is in.
pub async fn forfeit<C: CfApi, P: ProblemSource, E: MatchEnv, O: SlashOutput>(
    svc: &MatchService<C, P>,
    env: &E,
    out: &O,
    guild: i64,
) -> anyhow::Result<()> {
    out.defer(true).await?;
    let user = out.user_id();
    let (lobby, kind) = if let Some(current) = svc.current(guild, user).await? {
        let lobby = current.r#match.status == "lobby";
        let id = current.r#match.id;
        let view = if lobby { svc.leave(id, user).await? } else { svc.give_up(id, user).await? };
        edit_stored_message(out, &view).await?;
        (lobby, "match")
    } else if let Some(lobby) = env.forfeit_contest(guild, user).await? {
        (lobby, "contest")
    } else {
        return Err(plain("You are not in an unfinished match or contest."));
    };
    let text = if lobby { format!("You left the {kind} lobby.") } else { format!("You gave up the {kind}.") };
    out.followup_text(&text).await
}

/// `/leaderboard`
pub async fn leaderboard<O: SlashOutput>(db: &Store, out: &O, guild: i64, now: i64, sort: Sort, kind: Kind) -> anyhow::Result<()> {
    out.defer(true).await?;
    let rows = db.stats(guild, kind.as_str()).await?;
    let today = utc_day(now);
    let streaks: HashMap<i64, i64> =
        db.daily_days_by_user(guild).await?.into_iter().map(|(user, days)| (user, daily_streak(&days, today))).collect();
    out.send_pages(pages(stats_pages(&rows, out.user_id(), &streaks, sort == Sort::Streak, kind.as_str()))).await
}

#[allow(clippy::too_many_arguments)]
/// `/profile`; `member` and `handle` are the options, `me` the caller.
pub async fn profile<E: MatchEnv, O: SlashOutput>(
    db: &Store,
    env: &E,
    out: &O,
    guild: i64,
    now: i64,
    me: &Member,
    member: Option<Member>,
    handle: Option<&str>,
) -> anyhow::Result<()> {
    if member.is_some() && handle.is_some() {
        return Err(plain("Give a member or a handle, not both."));
    }
    out.defer(true).await?;
    let mut member = member;
    if let Some(handle) = handle {
        let Some(owner) = db.user_for_handle(guild, handle).await? else {
            return Err(plain(format!("No member is linked to {}.", esc(handle))));
        };
        member = Some(
            out.fetch_member(owner)
                .await?
                .ok_or_else(|| plain(format!("{} is linked to a member who left this server.", esc(handle))))?,
        );
    }
    let target = member.as_ref().unwrap_or(me);
    let rows = db.stats(guild, "all").await?;
    let rank = rows.iter().position(|r| r.0 == target.id).map(|i| i as i64 + 1);
    let mut counts = [0, 0];
    for (slot, kind) in counts.iter_mut().zip(["matches", "contests"]) {
        *slot = db.stats(guild, kind).await?.iter().find(|r| r.0 == target.id).map_or(0, |r| r.3);
    }
    let changes = db.rating_changes(guild, target.id).await?;
    let series = rating_series(&changes);
    let streak = daily_streak(&db.daily_days(guild, target.id).await?, utc_day(now));
    let codeforces = match db.get_handle(guild, target.id).await? {
        None => "no linked handle".to_string(),
        Some(linked) => match env.user_info(&linked.handle).await {
            Ok(user) => {
                let rated = match user.rating {
                    Some(rating) => format!("{rating} ({})", user.rank.as_deref().unwrap_or("None")),
                    None => "unrated".into(),
                };
                format!("{} {rated}", profile_link(&user.handle))
            }
            // `except CodeforcesError`: only that error, other errors (an unknown handle) propagate
            Err(e) if matches!(e.downcast_ref::<BotError>(), Some(BotError::Codeforces(_))) => {
                format!("{} (Codeforces did not answer)", esc(&linked.handle))
            }
            Err(e) => return Err(e),
        },
    };
    let deltas: Vec<i64> = changes.iter().map(|c| c.1).collect();
    let embed = member_profile_embed(&MemberProfile {
        display_name: &target.name,
        rating: db.rating(guild, target.id).await?,
        rank,
        ranked: rows.len() as i64,
        matches: counts[0],
        contests: counts[1],
        changes: &deltas,
        peak: series.iter().map(|s| s.1).max(),
        streak,
        codeforces: &codeforces,
        contest_wins: env.contest_wins(guild, target.id).await?,
    });
    out.followup_embed(embed.into()).await
}

/// Python `on_button`.
pub async fn on_button<C: CfApi, P: ProblemSource, O: ButtonOutput>(
    svc: &MatchService<C, P>,
    out: &O,
    action: Action,
    match_id: i64,
) -> anyhow::Result<()> {
    out.defer(false).await?;
    let user = out.user_id();
    if action == Action::Rematch {
        // A new lobby in a new message; the finished result stays as it is.
        let view = svc.rematch(match_id, user, out.channel_id()).await?;
        return post(svc, out, &view).await;
    }
    let view = match action {
        Action::Join => svc.join(match_id, user).await?,
        Action::Start => svc.start(match_id, user).await?,
        Action::Leave => svc.leave(match_id, user).await?,
        Action::Check => svc.check(match_id, user).await?,
        Action::GiveUp => svc.give_up(match_id, user).await?,
        Action::Rematch => unreachable!("handled above"),
    };
    out.edit_original(match_embed(&view)?.into(), match_components(&view)).await?;
    if action == Action::Check && view.r#match.status == "active" {
        out.followup_text(check_note(&view, user)).await?;
    }
    Ok(())
}

/// The private note after a Check that did not finish the match.
pub fn check_note(view: &MatchView, user: i64) -> &'static str {
    let solved = view.players.iter().find(|p| p.user_id == user).is_some_and(|p| p.solved_at.is_some());
    if solved {
        "You solved it. Waiting for the other players."
    } else {
        "No accepted submission found yet. Solve the problem on Codeforces, then press Check again."
    }
}

/// `MatchButton.callback`: errors are reported here, privately (discord.py never calls `DynamicItem.on_error`).
pub async fn press<C: CfApi, P: ProblemSource, O: ButtonOutput>(svc: &MatchService<C, P>, out: &O, action: Action, match_id: i64) {
    if let Err(e) = on_button(svc, out, action, match_id).await {
        report_error(out, &e).await;
    }
}

/// The body of one `expire_loop` round: expire, then edit each message. Errors are logged, never raised.
pub async fn expire_tick<C: CfApi, P: ProblemSource, E: Editor>(svc: &MatchService<C, P>, editor: &E) {
    let views = match svc.expire_due().await {
        Ok(views) => views,
        Err(e) => {
            tracing::error!("Match expiry failed: {e:?}");
            return;
        }
    };
    for view in views {
        if let Err(e) = edit_stored_message(editor, &view).await {
            tracing::error!("Match expiry failed: {e:?}");
            return;
        }
    }
}

// ---- the Discord side ----------------------------------------------------------------------------------

impl Editor for serenity::Http {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        let edit = serenity::EditMessage::new().embed(embed).components(row.into_iter().collect());
        ChannelId::new(channel as u64).edit_message(self, MessageId::new(message as u64), edit).await?;
        Ok(())
    }

    async fn retire_message(&self, channel: i64, message: i64, content: &str) -> anyhow::Result<()> {
        let edit = serenity::EditMessage::new().content(content).embeds(vec![]).components(vec![]);
        ChannelId::new(channel as u64).edit_message(self, MessageId::new(message as u64), edit).await?;
        Ok(())
    }
}

fn jump_url(guild: Option<serenity::GuildId>, channel: serenity::ChannelId, message: serenity::MessageId) -> String {
    match guild {
        Some(g) => format!("https://discord.com/channels/{g}/{channel}/{message}"),
        None => format!("https://discord.com/channels/@me/{channel}/{message}"),
    }
}

fn to_member(m: serenity::Member) -> Member {
    Member { id: m.user.id.get() as i64, name: m.display_name().to_string(), bot: m.user.bot }
}

/// The real output of a slash command.
struct SlashOut<'a>(Context<'a>);

impl Editor for SlashOut<'_> {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        Editor::edit_message(&*self.0.serenity_context().http, channel, message, embed, row).await
    }

    async fn retire_message(&self, channel: i64, message: i64, content: &str) -> anyhow::Result<()> {
        Editor::retire_message(&*self.0.serenity_context().http, channel, message, content).await
    }
}

impl PostOutput for SlashOut<'_> {
    fn user_id(&self) -> i64 {
        self.0.author().id.get() as i64
    }

    fn channel_id(&self) -> i64 {
        self.0.channel_id().get() as i64
    }

    async fn defer(&self, ephemeral: bool) -> anyhow::Result<()> {
        if ephemeral { self.0.defer_ephemeral().await?; } else { self.0.defer().await?; }
        Ok(())
    }

    async fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<Sent> {
        let reply = poise::CreateReply::default().embed(embed).components(row.into_iter().collect());
        let message = self.0.send(reply).await?.into_message().await?;
        Ok(Sent { id: message.id.get() as i64, url: jump_url(self.0.guild_id(), message.channel_id, message.id) })
    }

    async fn followup_text(&self, text: &str) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().content(text).ephemeral(true)).await?;
        Ok(())
    }
}

impl SlashOutput for SlashOut<'_> {
    async fn followup_embed(&self, embed: CreateEmbed) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().embed(embed).ephemeral(true)).await?;
        Ok(())
    }

    async fn send_pages(&self, pages: Vec<CreateEmbed>) -> anyhow::Result<()> {
        DiscordOutput::from_ctx(self.0)?.send_pages(pages).await
    }

    async fn fetch_member(&self, user: i64) -> anyhow::Result<Option<Member>> {
        let guild = self.0.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?;
        match guild.member(self.0, serenity::UserId::new(user as u64)).await {
            Ok(m) => Ok(Some(to_member(m))),
            Err(serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(r))) if r.status_code.as_u16() == 404 => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// A Match button press: the component interaction.
struct ButtonOut<'a> {
    ctx: &'a serenity::Context,
    press: &'a serenity::ComponentInteraction,
    done: AtomicBool,
}

impl Editor for ButtonOut<'_> {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        Editor::edit_message(&*self.ctx.http, channel, message, embed, row).await
    }

    async fn retire_message(&self, channel: i64, message: i64, content: &str) -> anyhow::Result<()> {
        Editor::retire_message(&*self.ctx.http, channel, message, content).await
    }
}

impl PostOutput for ButtonOut<'_> {
    fn user_id(&self) -> i64 {
        self.press.user.id.get() as i64
    }

    fn channel_id(&self) -> i64 {
        self.press.channel_id.get() as i64
    }

    async fn defer(&self, ephemeral: bool) -> anyhow::Result<()> {
        // a component's `defer()` acknowledges the press and keeps the message
        let response = if ephemeral {
            serenity::CreateInteractionResponse::Defer(serenity::CreateInteractionResponseMessage::new().ephemeral(true))
        } else {
            serenity::CreateInteractionResponse::Acknowledge
        };
        self.press.create_response(self.ctx, response).await?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<Sent> {
        let followup = serenity::CreateInteractionResponseFollowup::new().embed(embed).components(row.into_iter().collect());
        let message = self.press.create_followup(self.ctx, followup).await?;
        Ok(Sent { id: message.id.get() as i64, url: jump_url(self.press.guild_id, message.channel_id, message.id) })
    }

    async fn followup_text(&self, text: &str) -> anyhow::Result<()> {
        let followup = serenity::CreateInteractionResponseFollowup::new().content(text).ephemeral(true);
        self.press.create_followup(self.ctx, followup).await?;
        Ok(())
    }
}

impl ButtonOutput for ButtonOut<'_> {
    async fn edit_original(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        let edit = serenity::EditInteractionResponse::new().embed(embed).components(row.into_iter().collect());
        self.press.edit_response(self.ctx, edit).await?;
        Ok(())
    }
}

impl Interaction for ButtonOut<'_> {
    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn is_application_command(&self) -> bool {
        false
    }

    async fn send_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        let reply = serenity::CreateInteractionResponse::Message(
            serenity::CreateInteractionResponseMessage::new().content(message).ephemeral(true),
        );
        self.press.create_response(self.ctx, reply).await.map_err(|_| SendFailed)?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.followup_text(message).await.map_err(|_| SendFailed)
    }

    async fn delete_original(&self) -> Result<(), SendFailed> {
        Err(SendFailed) // never called for a component
    }
}

/// The persistent match button handler, called from the component branch of `FullEvent::InteractionCreate`
/// (Python `MatchButton`, registered with `bot.add_dynamic_items`, so it works after a restart: the custom id alone
/// carries the state). False when the custom id is not `match:<action>:<digits>` (not ours).
pub async fn on_component(ctx: &serenity::Context, press_: &serenity::ComponentInteraction, services: &Services) -> bool {
    let Some((action, match_id)) = parse_custom_id(&press_.data.custom_id) else { return false };
    let out = ButtonOut { ctx, press: press_, done: AtomicBool::new(false) };
    press(match_service(services), &out, action, match_id).await;
    true
}

/// One round of the expiry loop with the real Discord client; `bot::expire_loop` calls it every `EXPIRE_INTERVAL` once the
/// loops start (Python `before_loop`: `wait_until_ready`; `tasks.loop` runs the first round at once).
pub async fn expire_round(ctx: &serenity::Context, services: &Services) {
    expire_tick(match_service(services), ctx.http.as_ref()).await;
}

// ---- the services ------------------------------------------------------------------------------------

fn match_service(services: &Services) -> &MatchService<CodeforcesClient, ProblemCache> {
    &services.matches
}

impl MatchEnv for Services {
    fn problems(&self) -> Arc<Vec<Problem>> {
        crate::matches::ProblemSource::problems(self.problems.as_ref())
    }

    fn tags(&self) -> Vec<String> {
        self.problems.tags().iter().cloned().collect()
    }

    async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        self.cf.user_info(handle).await
    }

    async fn contest_history(&self, guild: i64, user: i64) -> anyhow::Result<Vec<MashupRecord>> {
        self.mashups.history(guild, user, 50).await
    }

    async fn contest_wins(&self, guild: i64, user: i64) -> anyhow::Result<i64> {
        self.mashups.wins(guild, user, 200).await
    }

    /// The contest half of `/ff` needs the Discord client; `ff` goes through `FfEnv`, never this.
    async fn forfeit_contest(&self, _guild: i64, _user: i64) -> anyhow::Result<Option<bool>> {
        anyhow::bail!("forfeit_contest needs the Discord context: use FfEnv")
    }
}

/// `Services` plus the Discord client, for `/ff`: the contest half calls `mashup::forfeit_contest_real`.
struct FfEnv<'a> {
    ctx: &'a serenity::Context,
    services: &'a Services,
}

impl MatchEnv for FfEnv<'_> {
    fn problems(&self) -> Arc<Vec<Problem>> {
        self.services.problems()
    }

    fn tags(&self) -> Vec<String> {
        MatchEnv::tags(self.services)
    }

    async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        self.services.user_info(handle).await
    }

    async fn contest_history(&self, guild: i64, user: i64) -> anyhow::Result<Vec<MashupRecord>> {
        self.services.contest_history(guild, user).await
    }

    async fn contest_wins(&self, guild: i64, user: i64) -> anyhow::Result<i64> {
        self.services.contest_wins(guild, user).await
    }

    async fn forfeit_contest(&self, guild: i64, user: i64) -> anyhow::Result<Option<bool>> {
        crate::cogs::mashup::forfeit_contest_real(self.ctx, self.services, guild, user).await
    }
}

// ---- the commands --------------------------------------------------------------------------------------

fn guild(ctx: Context<'_>) -> anyhow::Result<i64> {
    Ok(ctx.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?.get() as i64)
}

async fn invoker(ctx: Context<'_>) -> Member {
    let name = match ctx.author_member().await {
        Some(m) => m.display_name().to_string(),
        None => ctx.author().display_name().to_string(),
    };
    Member { id: ctx.author().id.get() as i64, name, bot: ctx.author().bot }
}

async fn tags_autocomplete(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    let s: &Arc<Services> = ctx.data();
    let choices = tag_autocomplete(s.as_ref(), partial).into_iter().map(|t| serenity::AutocompleteChoice::new(t.clone(), t));
    serenity::CreateAutocompleteResponse::new().set_choices(choices.collect())
}

/// Play a Codeforces match
#[poise::command(slash_command, guild_only, rename = "match", subcommands("match_solo", "match_duel", "match_custom", "match_history"))]
pub async fn match_group(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// Practice alone for points
#[poise::command(slash_command, guild_only, rename = "solo")]
pub async fn match_solo(
    ctx: Context<'_>,
    #[description = "Problem rating (default: your match rating)"]
    #[min = 800]
    #[max = 3500]
    rating: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tags_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tags_autocomplete"]
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    solo(match_service(s), &SlashOut(ctx), guild(ctx)?, rating, tags, exclude_tags).await
}

/// Duel one other player for points
#[poise::command(slash_command, guild_only, rename = "duel")]
pub async fn match_duel(
    ctx: Context<'_>,
    #[description = "Challenge this member (default: anyone can join)"] opponent: Option<serenity::Member>,
    #[description = "Problem rating (default: average match rating of the players)"]
    #[min = 800]
    #[max = 3500]
    rating: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tags_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tags_autocomplete"]
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let opponent = opponent.map(to_member);
    duel(match_service(s), &SlashOut(ctx), guild(ctx)?, opponent.as_ref(), rating, tags, exclude_tags).await
}

/// A match for several players
#[poise::command(slash_command, guild_only, rename = "custom")]
pub async fn match_custom(
    ctx: Context<'_>,
    #[description = "Maximum number of players"]
    #[min = 2]
    #[max = 10] // MATCH_MAX_PLAYERS
    players: i64,
    #[description = "Problem rating (default: average match rating of the players)"]
    #[min = 800]
    #[max = 3500]
    rating: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tags_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tags_autocomplete"]
    exclude_tags: Option<String>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    custom(match_service(s), &SlashOut(ctx), guild(ctx)?, players, rating, tags, exclude_tags).await
}

/// Show finished matches
#[poise::command(slash_command, guild_only, rename = "history")]
pub async fn match_history(
    ctx: Context<'_>,
    #[description = "Defaults to you; also shows your matches against them"] member: Option<serenity::Member>,
    #[description = "Show matches and contests (default), only matches, or only contests"] kind: Option<String>, // string choices set in `bot::commands`, like Python `Literal`
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let other = member.map(to_member);
    history(&s.db, s.as_ref(), &SlashOut(ctx), guild(ctx)?, &invoker(ctx).await, other.as_ref(), kind_of(kind)).await
}

/// Leave or give up your unfinished match or contest
#[poise::command(slash_command, guild_only)]
pub async fn ff(ctx: Context<'_>) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    forfeit(match_service(s), &FfEnv { ctx: ctx.serenity_context(), services: s.as_ref() }, &SlashOut(ctx), guild(ctx)?).await
}

/// Top match players in this server
#[poise::command(slash_command, guild_only, rename = "leaderboard")]
pub async fn leaderboard_command(
    ctx: Context<'_>,
    #[description = "Rank by match rating (default) or by daily streak"] sort: Option<String>,
    #[description = "Count matches and contests (default), only matches, or only contests"] kind: Option<String>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let sort = sort.and_then(|s| poise::ChoiceParameter::from_name(&s)).unwrap_or(Sort::Rating);
    leaderboard(&s.db, &SlashOut(ctx), guild(ctx)?, now(), sort, kind_of(kind)).await
}

/// A member's match rating, games and streak
#[poise::command(slash_command, guild_only, rename = "profile")]
pub async fn profile_command(
    ctx: Context<'_>,
    #[description = "Defaults to you"] member: Option<serenity::Member>,
    #[description = "Find the member who linked this Codeforces handle"] handle: Option<String>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let target = member.map(to_member);
    profile(&s.db, s.as_ref(), &SlashOut(ctx), guild(ctx)?, now(), &invoker(ctx).await, target, handle.as_deref()).await
}
