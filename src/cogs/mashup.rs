//! Port of src/cogs/mashup.py: `/host contest|virtual|bingo`, the persistent `mashup:<action>:<id>` buttons, the
//! team modal, the solve-log thread, and the mashup loop. Like cogs/matches.rs, each flow is a function over the
//! `MashupService` and narrow output traits (fakes in tests); the poise commands and the serenity impls at the
//! bottom are thin. `bot.rs` registers the commands, routes components and modals, and starts the loop.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use poise::serenity_prelude as serenity;
use serenity::{
    ButtonStyle, ChannelId, CreateActionRow, CreateAllowedMentions, CreateButton, CreateEmbed, CreateInputText,
    CreateInteractionResponse, CreateMessage, CreateModal, CreateThread, InputTextStyle, MessageId,
};

use crate::bot::Context;
use crate::cache::problems::ProblemCache;
use crate::cf::client::CodeforcesClient;
use crate::cf::models::{Contest, Problem};
use crate::cogs::matches::{ButtonOutput, Editor, PostOutput, Sent};
use crate::errors::{BotError, Interaction, SendFailed, report_error};
use crate::logic::base::{
    BINGO_MAX_SIZE, BINGO_MIN_SIZE, MASHUP_MAX_PROBLEMS, MASHUP_MAX_TEAM_SIZE, MASHUP_MAX_TEAMS, contest_suggestions, parse_digits, py_int, tag_suggestions,
};
use crate::mashup::service::{CreateMashup, MashupService, MashupView};
use crate::matches::{CfApi, ProblemSource};
use crate::services::Services;
use crate::ui::embeds::b::{final_message, give_up_message, log_board_embed, log_message, mashup_embed, start_message};

/// `@tasks.loop(seconds=60)`: the first interval; every successful tick sets the next from `service.interval`.
pub const TICK_INTERVAL: Duration = Duration::from_secs(60);

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

fn by_key<C, P: ProblemSource>(svc: &MashupService<C, P>) -> HashMap<String, Problem> {
    svc.problems.problems().iter().map(|p| (p.key(), p.clone())).collect()
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

const ALL_ACTIONS: [Action; 6] = [Action::Join, Action::Start, Action::Leave, Action::Check, Action::GiveUp, Action::Rematch];

impl Action {
    pub fn id(self) -> &'static str {
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
            Action::Check => ("Check", ButtonStyle::Primary),
            Action::GiveUp => ("Give Up", ButtonStyle::Danger),
            Action::Rematch => ("Rematch", ButtonStyle::Primary),
        }
    }
}

/// `MashupButton.template` `mashup:(?P<action>join|start|leave|check|giveup|rematch):(?P<id>\d+)`, full match, then
/// `int()`. An id past `i64` saturates: no contest has it, so the service answers "no longer exists".
pub fn parse_custom_id(custom_id: &str) -> Option<(Action, i64)> {
    let (action, number) = custom_id.strip_prefix("mashup:")?.split_once(':')?;
    let action = ALL_ACTIONS.into_iter().find(|a| a.id() == action)?;
    Some((action, parse_digits(number)?))
}

pub fn mashup_button(action: Action, mashup_id: i64) -> CreateButton {
    let (label, style) = action.label_style();
    CreateButton::new(format!("mashup:{}:{mashup_id}", action.id())).label(label).style(style)
}

/// Python `ACTIONS[status]`: none for a cancelled contest.
pub fn mashup_actions(view: &MashupView) -> Option<&'static [Action]> {
    match view.mashup.status.as_str() {
        "lobby" => Some(&[Action::Join, Action::Start, Action::Leave]),
        "active" => Some(&[Action::Check, Action::GiveUp]),
        "finished" => Some(&[Action::Rematch]),
        _ => None,
    }
}

pub fn mashup_components(view: &MashupView) -> Option<CreateActionRow> {
    let id = view.mashup.id;
    mashup_actions(view).map(|actions| CreateActionRow::Buttons(actions.iter().map(|&a| mashup_button(a, id)).collect()))
}

// ---- the team modal ------------------------------------------------------------------------------------

/// The modal's custom id. Python uses a random id and keeps the modal in memory (lost on a restart, 300 s timeout);
/// carrying the contest id lets the submit work statelessly.
pub fn team_modal_id(mashup_id: i64) -> String {
    format!("mashup_team:{mashup_id}")
}

pub fn parse_team_modal_id(custom_id: &str) -> Option<i64> {
    custom_id.strip_prefix("mashup_team:")?.parse().ok()
}

/// Python `TeamModal`: title "Join a team", one short text input "Team number (1-N)" with placeholder 1 and length 1-2.
pub fn team_modal(mashup_id: i64, teams: i64) -> CreateModal {
    let input = CreateInputText::new(InputTextStyle::Short, format!("Team number (1-{teams})"), "team")
        .placeholder("1")
        .min_length(1)
        .max_length(2);
    CreateModal::new(team_modal_id(mashup_id), "Join a team").components(vec![CreateActionRow::InputText(input)])
}

/// Python `TeamModal.on_submit` (after the Join press opened the modal).
pub async fn submit_team<C: CfApi, P: ProblemSource, O: ButtonOutput>(
    svc: &MashupService<C, P>,
    out: &O,
    mashup_id: i64,
    value: &str,
) -> anyhow::Result<()> {
    // Python `.strip()` also trims U+001C..U+001F; `int()` then trims the rest
    let stripped = value.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    let number = py_int(stripped).ok_or_else(|| plain("The team must be a number."))?;
    out.defer(false).await?;
    let view = svc.join(mashup_id, out.user_id(), Some(number)).await?;
    out.edit_original(mashup_embed(&view, &by_key(svc))?.into(), mashup_components(&view)).await
}

/// `TeamModal.on_error`: report privately.
pub async fn submit_modal<C: CfApi, P: ProblemSource, O: ButtonOutput>(svc: &MashupService<C, P>, out: &O, mashup_id: i64, value: &str) {
    if let Err(e) = submit_team(svc, out, mashup_id, value).await {
        report_error(out, &e).await;
    }
}

// ---- what the flows read and write -----------------------------------------------------------------

/// The caches the cog reads besides the problem source of the service.
pub trait MashupEnv: Sync {
    /// `services.problems.tags`
    fn tags(&self) -> Vec<String>;
    /// `services.contests.contests`
    fn contests(&self) -> Arc<Vec<Contest>>;
}

/// The thread and message side: edits of the stored message, the solve-log thread (Python `get_partial_messageable`
/// for a public thread, `create_thread` of a partial message).
pub trait Threads: Editor {
    /// `channel.get_partial_message(message).create_thread(name=..)`; the thread's id.
    fn create_thread(&self, channel: i64, message: i64, name: &str) -> impl Future<Output = anyhow::Result<i64>> + Send;
    /// `thread.send(text, embed=..., allowed_mentions=...)`: `ping` is `AllowedMentions(users=True)`, otherwise
    /// `AllowedMentions.none()`. The id of the new message.
    fn send(&self, thread: i64, text: &str, embed: Option<CreateEmbed>, ping: bool) -> impl Future<Output = anyhow::Result<i64>> + Send;
    /// `thread.get_partial_message(message).edit(embeds=[])`
    fn clear_board(&self, thread: i64, message: i64) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// A button press on a contest message.
pub trait PressOutput: ButtonOutput + Threads {
    /// `interaction.message` (its id)
    fn message_id(&self) -> Option<i64>;
    /// `interaction.response.send_modal(TeamModal(..))`
    fn send_modal(&self, mashup_id: i64, teams: i64) -> impl Future<Output = anyhow::Result<()>> + Send;
}

// ---- the slash commands ----------------------------------------------------------------------------

/// Python `_post`: the contest as a new message, remembered for later edits.
async fn post<C: CfApi, P: ProblemSource, O: PostOutput>(svc: &MashupService<C, P>, out: &O, view: &MashupView) -> anyhow::Result<()> {
    let sent = out.post(mashup_embed(view, &by_key(svc))?.into(), mashup_components(view)).await?;
    svc.set_message(view.mashup.id, out.channel_id(), sent.id).await
}

/// Python `_open`: `spec` carries the options; the place and host come from the interaction.
pub async fn open<C: CfApi, P: ProblemSource, O: PostOutput>(
    svc: &MashupService<C, P>,
    out: &O,
    guild: i64,
    spec: CreateMashup,
) -> anyhow::Result<()> {
    out.defer(false).await?;
    let spec = CreateMashup { guild_id: guild, channel_id: out.channel_id(), host_id: out.user_id(), ..spec };
    let view = svc.create(spec).await?;
    post(svc, out, &view).await
}

/// `/host contest`
#[allow(clippy::too_many_arguments)]
pub fn contest_spec(
    min_rating: i64,
    max_rating: i64,
    count: i64,
    hour: i64,
    minute: i64,
    penalty: i64,
    teams: i64,
    players: i64,
    rated: bool,
    tags: Option<String>,
    exclude_tags: Option<String>,
) -> CreateMashup {
    CreateMashup {
        min_rating,
        max_rating,
        count,
        duration_seconds: hour * 3600 + minute * 60,
        penalty_minutes: penalty,
        teams,
        team_size: players,
        rated,
        tags,
        exclude_tags,
        ..Default::default()
    }
}

/// `/host virtual` (Python `_virtual`): the checks, then the spec of the replay.
pub fn virtual_spec(
    contests: &[Contest],
    contest_id: i64,
    penalty: i64,
    teams: i64,
    players: i64,
    rated: bool,
) -> anyhow::Result<CreateMashup> {
    let Some(contest) = contests.iter().find(|c| c.id == contest_id) else {
        return Err(plain("I do not know that contest id. See /contests recent."));
    };
    if contest.phase != "FINISHED" {
        return Err(plain("That contest has not finished yet."));
    }
    // Python `//` and `%` floor
    let (hour, minute) = (contest.duration.div_euclid(3600), contest.duration.rem_euclid(3600) / 60);
    Ok(CreateMashup {
        min_rating: 800,
        max_rating: 3500,
        count: 1,
        duration_seconds: hour * 3600 + minute * 60,
        penalty_minutes: penalty,
        teams,
        team_size: players,
        rated,
        contest_id: Some(contest_id),
        ..Default::default()
    })
}

/// `/host bingo`: `count` is `size * size`, no penalty.
#[allow(clippy::too_many_arguments)]
pub fn bingo_spec(
    size: i64,
    mode: &str,
    win: &str,
    min_rating: i64,
    max_rating: i64,
    hour: i64,
    minute: i64,
    teams: i64,
    players: i64,
    rated: bool,
    tags: Option<String>,
    exclude_tags: Option<String>,
) -> CreateMashup {
    CreateMashup {
        min_rating,
        max_rating,
        count: size * size,
        duration_seconds: hour * 3600 + minute * 60,
        penalty_minutes: 0,
        teams,
        team_size: players,
        rated,
        tags,
        exclude_tags,
        mode: mode.into(),
        size,
        win: win.into(),
        ..Default::default()
    }
}

/// The autocomplete of `contest_id` on `/host virtual`: up to 25 `(label, id)`.
pub fn contest_autocomplete<C, P: ProblemSource, E: MashupEnv>(svc: &MashupService<C, P>, env: &E, current: &str) -> Vec<(String, i64)> {
    let known = svc.problems.problems().iter().filter(|p| p.rating.is_some_and(|r| r != 0)).map(|p| p.contest_id).collect();
    contest_suggestions(&env.contests(), &known, current, 25)
}

/// The autocomplete of `tags` and `exclude_tags` on `/host contest` and `/host bingo`: up to 25 values.
pub fn tag_autocomplete<E: MashupEnv>(env: &E, current: &str) -> Vec<String> {
    tag_suggestions(current, &env.tags(), 25)
}

// ---- the thread, the log, the message ---------------------------------------------------------------

/// Python `edit_message`: a message that cannot be edited is logged, never an error.
pub async fn edit_message<C, P: ProblemSource, E: Editor>(svc: &MashupService<C, P>, editor: &E, view: &MashupView) {
    let m = &view.mashup;
    let Some(message) = m.message_id else { return };
    let result = async { editor.edit_message(m.channel_id, message, mashup_embed(view, &by_key(svc))?.into(), mashup_components(view)).await };
    if let Err(e) = result.await {
        tracing::warn!("Could not edit mashup message {message}: {e:?}");
    }
}

/// Python `refresh`: the stored message and the thread up to date.
pub async fn refresh<C: CfApi, P: ProblemSource, T: Threads>(svc: &MashupService<C, P>, threads: &T, view: &MashupView) {
    edit_message(svc, threads, view).await;
    log_board(svc, threads, view).await;
}

/// Python `log_started`: pings every player in the thread when the host starts the contest.
pub async fn log_started<T: Threads>(threads: &T, view: &MashupView) {
    let m = &view.mashup;
    let Some(thread) = m.thread_id else { return };
    if let Err(e) = threads.send(thread, &start_message(view), None, true).await {
        tracing::warn!("Could not log the start of mashup {}: {e:?}", m.id); // a side channel, like the solve log
    }
}

/// Python `log_gave_up`: tells the thread that a player left the running contest.
pub async fn log_gave_up<T: Threads>(threads: &T, view: &MashupView, user: i64) {
    let m = &view.mashup;
    let Some(thread) = m.thread_id else { return };
    let result = async { threads.send(thread, &give_up_message(view, user)?, None, false).await };
    if let Err(e) = result.await {
        tracing::warn!("Could not log a give-up of mashup {}: {e:?}", m.id);
    }
}

/// Python `_log`: posts the solves that the thread does not have yet. A side channel: it must not break the button
/// or the loop, so errors are logged.
pub async fn log_board<C: CfApi, P: ProblemSource, T: Threads>(svc: &MashupService<C, P>, threads: &T, view: &MashupView) {
    if let Err(e) = try_log_board(svc, threads, view).await {
        tracing::warn!("Could not log to the thread of mashup {}: {e:?}", view.mashup.id);
    }
}

async fn try_log_board<C: CfApi, P: ProblemSource, T: Threads>(svc: &MashupService<C, P>, threads: &T, view: &MashupView) -> anyhow::Result<()> {
    let m = &view.mashup;
    let Some(thread) = m.thread_id else { return Ok(()) };
    let entries = svc.claims_to_log(view).await?;
    let final_due = svc.final_due(view).await?;
    if entries.is_empty() && !final_due {
        return Ok(());
    }
    let old = svc.store.get_mashup(m.id).await?.ok_or_else(|| anyhow::anyhow!("mashup {} is gone", m.id))?.log_board_id;
    let by_key = by_key(svc);
    let mut last = 0;
    for (i, entry) in entries.iter().enumerate() {
        // Only the newest message shows the board; the closing message is the newest.
        let board = if i + 1 == entries.len() && !final_due { Some(log_board_embed(view)?.into()) } else { None };
        last = threads.send(thread, &log_message(view, entry, &by_key)?, board, false).await?;
    }
    if final_due {
        let board = if m.status == "finished" { Some(log_board_embed(view)?.into()) } else { None };
        last = threads.send(thread, &final_message(view)?, board, false).await?;
    }
    svc.set_log_board(m.id, last).await?;
    if let Some(old) = old
        && let Err(e) = threads.clear_board(thread, old).await
    {
        tracing::info!("Could not clear the board of message {old}: {e:?}");
    }
    Ok(())
}

/// Python `_open_thread`: the message gets a thread where every solve is logged (opened when the contest starts).
/// A failure is logged and the presser is told what permission is missing.
async fn open_thread<C: CfApi, P: ProblemSource, O: PressOutput>(svc: &MashupService<C, P>, out: &O, view: &MashupView, message: i64) -> anyhow::Result<()> {
    let m = &view.mashup;
    let kind = if m.mode != "contest" {
        "Bingo"
    } else if m.contest_id.is_some() {
        "Virtual contest"
    } else {
        "Contest"
    };
    match out.create_thread(out.channel_id(), message, &format!("{kind} {} solve log", m.id)).await {
        Ok(thread) => svc.set_thread(m.id, thread).await,
        Err(e) => {
            tracing::warn!("Could not create the thread of mashup {}: {e:?}", m.id);
            out.followup_text(
                "I could not open the solve log thread here. Give me the Create Public Threads and Send Messages in Threads permissions.",
            )
            .await
        }
    }
}

// ---- the buttons ---------------------------------------------------------------------------------------

/// Python `on_button`.
pub async fn on_button<C: CfApi, P: ProblemSource, O: PressOutput>(
    svc: &MashupService<C, P>,
    out: &O,
    action: Action,
    mashup_id: i64,
) -> anyhow::Result<()> {
    let user = out.user_id();
    if action == Action::Rematch {
        // A new lobby in a new message; the final standings stay as they are.
        out.defer(false).await?;
        let view = svc.rematch(mashup_id, user, out.channel_id()).await?;
        return post(svc, out, &view).await;
    }
    if action == Action::Join
        && let Some(m) = svc.store.get_mashup(mashup_id).await?
        && m.status == "lobby"
        && m.team_size != 1
    {
        // A team contest asks for the team first; the modal then joins.
        return out.send_modal(mashup_id, m.teams).await;
    }
    out.defer(false).await?;
    let mut view = match action {
        Action::Join => svc.join(mashup_id, user, None).await?,
        Action::Start => svc.start(mashup_id, user).await?,
        Action::Leave => svc.leave(mashup_id, user).await?,
        Action::Check => svc.check(mashup_id, user).await?,
        Action::GiveUp => svc.give_up(mashup_id, user).await?,
        Action::Rematch => unreachable!("handled above"),
    };
    out.edit_original(mashup_embed(&view, &by_key(svc))?.into(), mashup_components(&view)).await?;
    if action == Action::Start
        && let Some(message) = out.message_id()
    {
        open_thread(svc, out, &view, message).await?;
        view = svc.view(mashup_id, None).await?;
    }
    log_board(svc, out, &view).await;
    match action {
        Action::GiveUp => log_gave_up(out, &view, user).await,
        Action::Start => log_started(out, &view).await,
        _ => {}
    }
    Ok(())
}

/// `MashupButton.callback`: errors are reported here, privately (discord.py never calls `DynamicItem.on_error`).
pub async fn press<C: CfApi, P: ProblemSource, O: PressOutput>(svc: &MashupService<C, P>, out: &O, action: Action, mashup_id: i64) {
    if let Err(e) = on_button(svc, out, action, mashup_id).await {
        report_error(out, &e).await;
    }
}

// ---- /ff (the contest half) and the loop -------------------------------------------------------------------

/// The contest half of `/ff` (match.py:281): leave the lobby or give up the contest the user is in, refresh its message
/// and the thread, and log the give-up. `Some(true)` for a lobby left, `Some(false)` for a give-up, `None` when the
/// user is in no unfinished contest. `MatchEnv::forfeit_contest` calls it.
pub async fn forfeit_contest<C: CfApi, P: ProblemSource, T: Threads>(
    svc: &MashupService<C, P>,
    threads: &T,
    guild: i64,
    user: i64,
) -> anyhow::Result<Option<bool>> {
    let Some(current) = svc.current(guild, user).await? else { return Ok(None) };
    let lobby = current.mashup.status == "lobby";
    let id = current.mashup.id;
    let view = if lobby { svc.leave(id, user).await? } else { svc.give_up(id, user).await? };
    refresh(svc, threads, &view).await;
    if !lobby {
        log_gave_up(threads, &view, user).await;
    }
    Ok(Some(lobby))
}

/// The body of one `tick_loop` round: tick, then post each change. `Some(next interval)` after a successful tick;
/// `None` when the tick failed (logged), so the interval stays as it was, like Python.
pub async fn tick_round<C: CfApi, P: ProblemSource, T: Threads>(svc: &MashupService<C, P>, threads: &T) -> Option<Duration> {
    let views = match svc.tick().await {
        Ok(views) => views,
        Err(e) => {
            tracing::error!("Mashup tick failed: {e:?}");
            return None;
        }
    };
    let next = Duration::from_secs(svc.interval.load(Ordering::SeqCst).max(0) as u64);
    for view in views {
        edit_message(svc, threads, &view).await;
        log_board(svc, threads, &view).await;
    }
    Some(next)
}

// ---- the Discord side ----------------------------------------------------------------------------------

impl Threads for serenity::Http {
    async fn create_thread(&self, channel: i64, message: i64, name: &str) -> anyhow::Result<i64> {
        let thread = ChannelId::new(channel as u64)
            .create_thread_from_message(self, MessageId::new(message as u64), CreateThread::new(name))
            .await?;
        Ok(thread.id.get() as i64)
    }

    async fn send(&self, thread: i64, text: &str, embed: Option<CreateEmbed>, ping: bool) -> anyhow::Result<i64> {
        // discord.py keeps `everyone` and `roles` True by default, so `AllowedMentions(users=True)` allows all three
        let mentions = if ping { CreateAllowedMentions::new().everyone(true).all_users(true).all_roles(true) } else { CreateAllowedMentions::new() };
        let mut message = CreateMessage::new().content(text).allowed_mentions(mentions);
        if let Some(embed) = embed {
            message = message.embed(embed);
        }
        Ok(ChannelId::new(thread as u64).send_message(self, message).await?.id.get() as i64)
    }

    async fn clear_board(&self, thread: i64, message: i64) -> anyhow::Result<()> {
        let edit = serenity::EditMessage::new().embeds(vec![]);
        ChannelId::new(thread as u64).edit_message(self, MessageId::new(message as u64), edit).await?;
        Ok(())
    }
}

fn jump_url(guild: Option<serenity::GuildId>, channel: serenity::ChannelId, message: serenity::MessageId) -> String {
    match guild {
        Some(g) => format!("https://discord.com/channels/{g}/{channel}/{message}"),
        None => format!("https://discord.com/channels/@me/{channel}/{message}"),
    }
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

/// A Mashup button press or a team modal submit.
enum Source<'a> {
    Button(&'a serenity::ComponentInteraction),
    Modal(&'a serenity::ModalInteraction),
}

struct ComponentOut<'a> {
    ctx: &'a serenity::Context,
    source: Source<'a>,
    done: AtomicBool,
}

impl ComponentOut<'_> {
    async fn respond(&self, response: CreateInteractionResponse) -> anyhow::Result<()> {
        match self.source {
            Source::Button(p) => p.create_response(self.ctx, response).await?,
            Source::Modal(m) => m.create_response(self.ctx, response).await?,
        }
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn followup(&self, followup: serenity::CreateInteractionResponseFollowup) -> anyhow::Result<serenity::Message> {
        Ok(match self.source {
            Source::Button(p) => p.create_followup(self.ctx, followup).await?,
            Source::Modal(m) => m.create_followup(self.ctx, followup).await?,
        })
    }
}

impl Editor for ComponentOut<'_> {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        Editor::edit_message(&*self.ctx.http, channel, message, embed, row).await
    }

    async fn retire_message(&self, channel: i64, message: i64, content: &str) -> anyhow::Result<()> {
        Editor::retire_message(&*self.ctx.http, channel, message, content).await
    }
}

impl Threads for ComponentOut<'_> {
    async fn create_thread(&self, channel: i64, message: i64, name: &str) -> anyhow::Result<i64> {
        Threads::create_thread(&*self.ctx.http, channel, message, name).await
    }

    async fn send(&self, thread: i64, text: &str, embed: Option<CreateEmbed>, ping: bool) -> anyhow::Result<i64> {
        Threads::send(&*self.ctx.http, thread, text, embed, ping).await
    }

    async fn clear_board(&self, thread: i64, message: i64) -> anyhow::Result<()> {
        self.ctx.http.clear_board(thread, message).await
    }
}

impl PostOutput for ComponentOut<'_> {
    fn user_id(&self) -> i64 {
        match self.source {
            Source::Button(p) => p.user.id.get() as i64,
            Source::Modal(m) => m.user.id.get() as i64,
        }
    }

    fn channel_id(&self) -> i64 {
        match self.source {
            Source::Button(p) => p.channel_id.get() as i64,
            Source::Modal(m) => m.channel_id.get() as i64,
        }
    }

    async fn defer(&self, ephemeral: bool) -> anyhow::Result<()> {
        // a component's `defer()` acknowledges the press and keeps the message
        self.respond(if ephemeral {
            CreateInteractionResponse::Defer(serenity::CreateInteractionResponseMessage::new().ephemeral(true))
        } else {
            CreateInteractionResponse::Acknowledge
        })
        .await
    }

    async fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<Sent> {
        let message = self.followup(serenity::CreateInteractionResponseFollowup::new().embed(embed).components(row.into_iter().collect())).await?;
        let guild = match self.source {
            Source::Button(p) => p.guild_id,
            Source::Modal(m) => m.guild_id,
        };
        Ok(Sent { id: message.id.get() as i64, url: jump_url(guild, message.channel_id, message.id) })
    }

    async fn followup_text(&self, text: &str) -> anyhow::Result<()> {
        self.followup(serenity::CreateInteractionResponseFollowup::new().content(text).ephemeral(true)).await?;
        Ok(())
    }
}

impl ButtonOutput for ComponentOut<'_> {
    async fn edit_original(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        let edit = serenity::EditInteractionResponse::new().embed(embed).components(row.into_iter().collect());
        match self.source {
            Source::Button(p) => p.edit_response(self.ctx, edit).await?,
            Source::Modal(m) => m.edit_response(self.ctx, edit).await?,
        };
        Ok(())
    }
}

impl PressOutput for ComponentOut<'_> {
    fn message_id(&self) -> Option<i64> {
        match self.source {
            Source::Button(p) => Some(p.message.id.get() as i64),
            Source::Modal(m) => m.message.as_ref().map(|m| m.id.get() as i64),
        }
    }

    async fn send_modal(&self, mashup_id: i64, teams: i64) -> anyhow::Result<()> {
        self.respond(CreateInteractionResponse::Modal(team_modal(mashup_id, teams))).await
    }
}

impl Interaction for ComponentOut<'_> {
    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn is_application_command(&self) -> bool {
        false
    }

    async fn send_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        let reply = CreateInteractionResponse::Message(serenity::CreateInteractionResponseMessage::new().content(message).ephemeral(true));
        self.respond(reply).await.map_err(|_| SendFailed)
    }

    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.followup_text(message).await.map_err(|_| SendFailed)
    }

    async fn delete_original(&self) -> Result<(), SendFailed> {
        Err(SendFailed) // never called for a component
    }
}

/// The persistent mashup button handler, called from the component branch of `FullEvent::InteractionCreate`
/// (Python `MashupButton`: the custom id alone carries the state, so it works after a restart). False when the custom
/// id is not `mashup:<action>:<digits>` (not ours).
pub async fn on_component(ctx: &serenity::Context, press_: &serenity::ComponentInteraction, services: &Services) -> bool {
    let Some((action, mashup_id)) = parse_custom_id(&press_.data.custom_id) else { return false };
    let out = ComponentOut { ctx, source: Source::Button(press_), done: AtomicBool::new(false) };
    press(mashup_service(services), &out, action, mashup_id).await;
    true
}

/// The team modal submit, called from the modal branch of `FullEvent::InteractionCreate`. False when the
/// custom id is not `mashup_team:<id>`.
pub async fn on_modal(ctx: &serenity::Context, modal: &serenity::ModalInteraction, services: &Services) -> bool {
    let Some(mashup_id) = parse_team_modal_id(&modal.data.custom_id) else { return false };
    let value = modal
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            serenity::ActionRowComponent::InputText(t) if t.custom_id == "team" => t.value.clone(),
            _ => None,
        })
        .unwrap_or_default();
    let out = ComponentOut { ctx, source: Source::Modal(modal), done: AtomicBool::new(false) };
    submit_modal(mashup_service(services), &out, mashup_id, &value).await;
    true
}

/// One round of the mashup loop with the real Discord client. `bot::mashup_loop` runs it once the loops start
/// (Python `before_loop`: `wait_until_ready`), then again after each returned interval (60 s, `TICK_INTERVAL`, until the
/// first success); `None` keeps the previous interval.
pub async fn tick_loop_round(ctx: &serenity::Context, services: &Services) -> Option<Duration> {
    tick_round(mashup_service(services), ctx.http.as_ref()).await
}

/// The contest half of `/ff` with the real Discord client, for `MatchEnv::forfeit_contest`.
pub async fn forfeit_contest_real(ctx: &serenity::Context, services: &Services, guild: i64, user: i64) -> anyhow::Result<Option<bool>> {
    forfeit_contest(mashup_service(services), ctx.http.as_ref(), guild, user).await
}

// ---- the services ------------------------------------------------------------------------------------

fn mashup_service(services: &Services) -> &MashupService<CodeforcesClient, ProblemCache> {
    &services.mashups
}

impl MashupEnv for Services {
    fn tags(&self) -> Vec<String> {
        self.problems.tags().iter().cloned().collect()
    }

    fn contests(&self) -> Arc<Vec<Contest>> {
        self.contests.contests()
    }
}

// ---- the commands --------------------------------------------------------------------------------------

fn guild(ctx: Context<'_>) -> anyhow::Result<i64> {
    Ok(ctx.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?.get() as i64)
}

async fn tags_autocomplete(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    let s: &Arc<Services> = ctx.data();
    let choices = tag_autocomplete(s.as_ref(), partial).into_iter().map(|t| serenity::AutocompleteChoice::new(t.clone(), t));
    serenity::CreateAutocompleteResponse::new().set_choices(choices.collect())
}

async fn contests_autocomplete(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    let s: &Arc<Services> = ctx.data();
    let choices = contest_autocomplete(mashup_service(s), s.as_ref(), partial).into_iter().map(|(label, id)| serenity::AutocompleteChoice::new(label, id));
    serenity::CreateAutocompleteResponse::new().set_choices(choices.collect())
}

/// Host a timed multi-problem contest
#[poise::command(slash_command, guild_only, rename = "host", subcommands("host_contest", "host_virtual", "host_bingo"))]
pub async fn host_group(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// Open a timed contest lobby with a live scoreboard
#[allow(clippy::too_many_arguments)]
#[poise::command(slash_command, guild_only, rename = "contest")]
pub async fn host_contest(
    ctx: Context<'_>,
    #[description = "Easiest problem rating (default 800)"]
    #[min = 800]
    #[max = 3500]
    min_rating: Option<i64>,
    #[description = "Hardest problem rating (default 1500)"]
    #[min = 800]
    #[max = 3500]
    max_rating: Option<i64>,
    #[description = "Number of problems (default 5)"]
    #[min = 1]
    #[max = 10] // MASHUP_MAX_PROBLEMS
    count: Option<i64>,
    #[description = "Hours the contest lasts (default 2)"]
    #[min = 0]
    #[max = 24]
    hour: Option<i64>,
    #[description = "Extra minutes (default 0)"]
    #[min = 0]
    #[max = 59]
    minute: Option<i64>,
    #[description = "Minutes added per wrong attempt before a solve (default 0)"]
    #[min = 0]
    #[max = 120]
    penalty: Option<i64>,
    #[description = "Number of teams (default 10)"]
    #[min = 2]
    #[max = 10] // MASHUP_MAX_TEAMS
    teams: Option<i64>,
    #[description = "Players in each team, 1-5 (default 1: every player is a team of their own)"]
    #[min = 1]
    #[max = 5] // MASHUP_MAX_TEAM_SIZE
    players: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tags_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tags_autocomplete"]
    exclude_tags: Option<String>,
    #[description = "True (default) changes match ratings; False leaves them alone (unrated)"] rated: Option<bool>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let spec = contest_spec(
        min_rating.unwrap_or(800),
        max_rating.unwrap_or(1500),
        count.unwrap_or(5),
        hour.unwrap_or(2),
        minute.unwrap_or(0),
        penalty.unwrap_or(0),
        teams.unwrap_or(MASHUP_MAX_TEAMS as i64),
        players.unwrap_or(1),
        rated.unwrap_or(true),
        tags,
        exclude_tags,
    );
    open(mashup_service(s), &SlashOut(ctx), guild(ctx)?, spec).await
}

/// Open a lobby that replays a finished Codeforces contest
#[poise::command(slash_command, guild_only, rename = "virtual")]
pub async fn host_virtual(
    ctx: Context<'_>,
    #[description = "Codeforces contest id to replay (it must have finished)"]
    #[autocomplete = "contests_autocomplete"]
    contest_id: i64,
    #[description = "Minutes added per wrong attempt before a solve (default 0)"]
    #[min = 0]
    #[max = 120]
    penalty: Option<i64>,
    #[description = "Number of teams (default 10)"]
    #[min = 2]
    #[max = 10] // MASHUP_MAX_TEAMS
    teams: Option<i64>,
    #[description = "Players in each team, 1-5 (default 1: every player is a team of their own)"]
    #[min = 1]
    #[max = 5] // MASHUP_MAX_TEAM_SIZE
    players: Option<i64>,
    #[description = "True (default) changes match ratings; False leaves them alone (unrated)"] rated: Option<bool>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let spec = virtual_spec(
        &s.contests(),
        contest_id,
        penalty.unwrap_or(0),
        teams.unwrap_or(MASHUP_MAX_TEAMS as i64),
        players.unwrap_or(1),
        rated.unwrap_or(true),
    )?;
    open(mashup_service(s), &SlashOut(ctx), guild(ctx)?, spec).await
}

/// Open a bingo lobby: teams race for the squares
#[allow(clippy::too_many_arguments)]
#[poise::command(slash_command, guild_only, rename = "bingo")]
async fn host_bingo_raw(
    ctx: Context<'_>,
    #[description = "Squares on each side of the board, 3-7 (default 5)"]
    #[min = 3]
    #[max = 7] // BINGO_MIN_SIZE, BINGO_MAX_SIZE
    size: Option<i64>,
    #[description = "classic: a claimed square stays; replace: a claimed square gets a +100 problem to steal"] mode: Option<String>,
    #[description = "line (default): the first full line wins; continuous: play to the end, rank by lines"] win: Option<String>,
    #[description = "Easiest problem rating (default 800)"]
    #[min = 800]
    #[max = 3500]
    min_rating: Option<i64>,
    #[description = "Hardest problem rating (default 1500)"]
    #[min = 800]
    #[max = 3500]
    max_rating: Option<i64>,
    #[description = "Hours the contest lasts (default 2)"]
    #[min = 0]
    #[max = 24]
    hour: Option<i64>,
    #[description = "Extra minutes (default 0)"]
    #[min = 0]
    #[max = 59]
    minute: Option<i64>,
    #[description = "Number of teams (default 10)"]
    #[min = 2]
    #[max = 10] // MASHUP_MAX_TEAMS
    teams: Option<i64>,
    #[description = "Players in each team, 1-5 (default 1: every player is a team of their own)"]
    #[min = 1]
    #[max = 5] // MASHUP_MAX_TEAM_SIZE
    players: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tags_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tags_autocomplete"]
    exclude_tags: Option<String>,
    #[description = "True (default) changes match ratings; False leaves them alone (unrated)"] rated: Option<bool>,
) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    let mode = if mode.as_deref() == Some("replace") { "replace" } else { "classic" };
    let win = if win.as_deref() == Some("continuous") { "continuous" } else { "line" };
    let spec = bingo_spec(
        size.unwrap_or(5),
        mode,
        win,
        min_rating.unwrap_or(800),
        max_rating.unwrap_or(1500),
        hour.unwrap_or(2),
        minute.unwrap_or(0),
        teams.unwrap_or(MASHUP_MAX_TEAMS as i64),
        players.unwrap_or(1),
        rated.unwrap_or(true),
        tags,
        exclude_tags,
    );
    open(mashup_service(s), &SlashOut(ctx), guild(ctx)?, spec).await
}

/// `/host bingo`. poise turns every choice into an integer option; Python's `Literal["classic", "replace"]` and
/// `Literal["line", "continuous"]` are string options with string choices, so the two option types are set here.
pub fn host_bingo() -> poise::Command<Arc<Services>, crate::bot::Error> {
    use serenity::CommandOptionType::String as Str;
    let mut command = host_bingo_raw();
    command.parameters[1].type_setter =
        Some(|o| o.kind(Str).add_string_choice("classic", "classic").add_string_choice("replace", "replace"));
    command.parameters[2].type_setter =
        Some(|o| o.kind(Str).add_string_choice("line", "line").add_string_choice("continuous", "continuous"));
    command
}

// The constants the literals above mirror, so a change in logic::base breaks the build here.
const _: () = {
    assert!(MASHUP_MAX_PROBLEMS == 10 && MASHUP_MAX_TEAMS == 10 && MASHUP_MAX_TEAM_SIZE == 5);
    assert!(BINGO_MIN_SIZE == 3 && BINGO_MAX_SIZE == 7);
};
