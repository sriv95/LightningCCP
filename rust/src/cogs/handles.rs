//! Port of src/cogs/handles.py: `/handle link|show|list|unlink` and the persistent Verify button (`verify:<user_id>`).
//! The commands are cores over `CfApi`, `ProblemSource`, the real `Store` and a narrow output trait (fake in tests),
//! plus a private poise implementation, as in `cogs/plot.rs`.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use poise::serenity_prelude as serenity;
use rand::Rng;
use rand::seq::IndexedRandom;
use serenity::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, GuildId, UserId,
};

use crate::bot::Context;
use crate::cogs::plot::Person;
use crate::daily::{CfApi, ProblemSource};
use crate::errors::{self, BotError, Interaction, SendFailed};
use crate::logic::base::{LINK_MAX_RATING, LINK_WINDOW_SECONDS, ProblemFilter, filter_problems, has_compile_error, solved_keys};
use crate::services::Services;
use crate::store::base::Store;
use crate::ui::embeds::a::{EmbedData, esc, handle_list_pages, profile_embed};

pub const VERIFY_PREFIX: &str = "verify:";
pub const PER_PAGE: usize = 10;

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

/// The custom id of the Verify button (Python `VerifyButton.custom_id`).
pub fn verify_custom_id(user_id: u64) -> String {
    format!("{VERIFY_PREFIX}{user_id}")
}

/// Python `DynamicItem` template `verify:(?P<user>\d+)` with `fullmatch`: the user id of a Verify button.
/// shortcut: ASCII digits only and u64 range (Python's `\d` also takes other Unicode digits and ints are unbounded);
/// Discord ids are ASCII and below 2^64, and the bot only creates ids from real user ids.
pub fn parse_verify_id(custom_id: &str) -> Option<u64> {
    let digits = custom_id.strip_prefix(VERIFY_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A followup that only carries text (what `verify` needs; a button press has no slash-command output).
pub trait Reply: Sync {
    /// `interaction.followup.send(text, ephemeral=True)`
    fn followup_text(&self, content: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// What the slash commands write to Discord.
pub trait HandlesOutput: Reply {
    /// `interaction.followup.send(text, view=<VerifyButton(user_id)>, ephemeral=True)`
    fn followup_verify(&self, content: &str, user_id: u64) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `interaction.followup.send(embed=embed, ephemeral=True)`
    fn followup_embed(&self, embed: EmbedData) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `send_pages(interaction, pages)`: one embed, or Prev/Next buttons for the command user.
    fn followup_pages(&self, pages: Vec<EmbedData>) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `interaction.response.send_message(text, ephemeral=True)` (a command that did not defer).
    fn respond_text(&self, content: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `guild.fetch_member(id)`; `None` when Discord says the member is not found.
    fn fetch_member(&self, id: u64) -> impl Future<Output = anyhow::Result<Option<Person>>> + Send;
}

/// `/handle link`, after the defer.
#[allow(clippy::too_many_arguments)]
pub async fn link<C: CfApi, P: ProblemSource>(
    db: &Store,
    cf: &C,
    problems: &P,
    rng: &mut (impl Rng + Send),
    out: &impl HandlesOutput,
    now: i64,
    guild: i64,
    user_id: u64,
    handle: &str,
) -> anyhow::Result<()> {
    let user = cf.user_info(handle).await?;
    if let Some(owner) = db.user_for_handle(guild, &user.handle).await?
        && owner != user_id as i64
    {
        return Err(BotError::HandleTaken(format!("{} is already linked to <@{owner}>.", esc(&user.handle))).into());
    }
    let problems = problems.require()?;
    let solved = solved_keys(&cf.user_status(&user.handle, None).await?);
    let easy = ProblemFilter { max_rating: Some(LINK_MAX_RATING), ..Default::default() };
    let mut pool = filter_problems(&problems, &ProblemFilter { solved, ..easy.clone() });
    if pool.is_empty() {
        pool = filter_problems(&problems, &easy);
    }
    let Some(problem) = pool.choose(rng) else {
        return Err(plain("No suitable problem found. Try again later."));
    };
    db.save_pending(guild, user_id as i64, &user.handle, &problem.key(), now).await?;
    let text = format!(
        "To prove you own **{}**, submit a **compile error** to [{} - {}]({}) within {} minutes, then press **Verify**.",
        esc(&user.handle),
        problem.key(),
        esc(&problem.name),
        problem.url(),
        LINK_WINDOW_SECONDS / 60
    );
    out.followup_verify(&text, user_id).await
}

/// The Verify button's work (Python `HandlesCog.verify`), after the defer.
pub async fn verify<C: CfApi>(db: &Store, cf: &C, out: &impl Reply, now: i64, guild: i64, user_id: u64) -> anyhow::Result<()> {
    let user_id = user_id as i64;
    let Some(pending) = db.get_pending(guild, user_id).await? else {
        return Err(plain("No link in progress. Run /handle link again."));
    };
    if now > pending.issued_at + LINK_WINDOW_SECONDS {
        db.delete_pending(guild, user_id).await?;
        return Err(plain("That link attempt expired. Run /handle link again."));
    }
    let subs = cf.user_status(&pending.handle, Some(20)).await?;
    if !has_compile_error(&subs, &pending.problem_key, pending.issued_at, LINK_WINDOW_SECONDS) {
        return Err(plain(format!(
            "No compile error found on {} yet. Submit one on Codeforces, then press Verify again.",
            pending.problem_key
        )));
    }
    let user = cf.user_info(&pending.handle).await?;
    db.link_handle(guild, user_id, &user.handle, user.rating).await?;
    db.delete_pending(guild, user_id).await?;
    out.followup_text(&format!("Linked you to **{}**.", esc(&user.handle))).await
}

/// The argument check of `/handle show` that runs before the defer.
pub fn check_show_args(member: bool, handle: bool) -> anyhow::Result<()> {
    if member && handle {
        return Err(plain("Give a member or a handle, not both."));
    }
    Ok(())
}

/// `/handle show`, after the defer. `member` is the option, `invoker` the caller.
pub async fn show<C: CfApi>(
    db: &Store,
    cf: &C,
    out: &impl HandlesOutput,
    guild: i64,
    invoker: Person,
    member: Option<Person>,
    handle: Option<&str>,
) -> anyhow::Result<()> {
    let mut member = member;
    if let Some(handle) = handle {
        let Some(owner) = db.user_for_handle(guild, handle).await? else {
            return Err(plain(format!("No member is linked to {}.", esc(handle))));
        };
        member = out.fetch_member(owner as u64).await?;
        if member.is_none() {
            return Err(plain(format!("{} is linked to a member who left this server.", esc(handle))));
        }
    }
    let target = member.unwrap_or(invoker);
    let Some(linked) = db.get_handle(guild, target.id as i64).await? else {
        return Err(BotError::NotLinked(format!("{} has no linked handle.", target.name)).into());
    };
    let user = cf.user_info(&linked.handle).await?;
    out.followup_embed(profile_embed(&user, &target.name)).await
}

/// `/handle list`, after the defer.
pub async fn list<C: CfApi>(db: &Store, cf: &C, out: &impl HandlesOutput, guild: i64) -> anyhow::Result<()> {
    let linked = db.linked_handles(guild).await?;
    if linked.is_empty() {
        return Err(plain("No member has linked a handle yet."));
    }
    let owners: HashMap<String, i64> = linked.iter().map(|(uid, h)| (h.to_lowercase(), *uid)).collect();
    let users = cf.users_info(&linked.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>()).await?;
    let mut entries = Vec::new();
    for u in users {
        let owner = *owners.get(&u.handle.to_lowercase()).ok_or_else(|| anyhow::anyhow!("KeyError: {}", u.handle))?;
        entries.push((owner, u.handle, u.rating, u.rank));
    }
    entries.sort_by_key(|e| (e.2.is_none(), -e.2.unwrap_or(0), e.1.to_lowercase()));
    let pages = handle_list_pages(&entries, PER_PAGE)?;
    // Python `send_pages` with no page at all (every linked handle gone from Codeforces): IndexError, a generic error
    anyhow::ensure!(!pages.is_empty(), "list index out of range");
    out.followup_pages(pages).await
}

/// `/handle unlink`; this command does not defer.
pub async fn unlink(db: &Store, out: &impl HandlesOutput, guild: i64, user_id: u64) -> anyhow::Result<()> {
    if !db.unlink(guild, user_id as i64).await? {
        return Err(BotError::NotLinked("You have no linked handle.".into()).into());
    }
    out.respond_text("Unlinked.").await
}

// --- the Discord side -----------------------------------------------------------------------------------------

fn verify_row(user_id: u64) -> CreateActionRow {
    CreateActionRow::Buttons(vec![CreateButton::new(verify_custom_id(user_id)).label("Verify").style(ButtonStyle::Success)])
}

fn now() -> i64 {
    crate::daily::system_clock() as i64 // Python int(time.time())
}

fn problems_of(services: &Services) -> &crate::cache::problems::ProblemCache {
    &services.problems
}

fn rng() -> rand::rngs::StdRng {
    rand::SeedableRng::from_rng(&mut rand::rng())
}

struct SlashOut<'a>(Context<'a>);

impl Reply for SlashOut<'_> {
    async fn followup_text(&self, content: &str) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().content(content).ephemeral(true)).await?;
        Ok(())
    }
}

impl HandlesOutput for SlashOut<'_> {
    async fn followup_verify(&self, content: &str, user_id: u64) -> anyhow::Result<()> {
        let reply = poise::CreateReply::default().content(content).components(vec![verify_row(user_id)]).ephemeral(true);
        self.0.send(reply).await?;
        Ok(())
    }
    async fn followup_embed(&self, embed: EmbedData) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().embed(embed.into()).ephemeral(true)).await?;
        Ok(())
    }
    async fn followup_pages(&self, pages: Vec<EmbedData>) -> anyhow::Result<()> {
        crate::bot::DiscordOutput::from_ctx(self.0)?.send_pages(pages.into_iter().map(Into::into).collect()).await
    }
    async fn respond_text(&self, content: &str) -> anyhow::Result<()> {
        self.followup_text(content).await
    }
    async fn fetch_member(&self, id: u64) -> anyhow::Result<Option<Person>> {
        let guild = self.0.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?;
        match self.0.http().get_member(guild, UserId::new(id)).await {
            Ok(m) => Ok(Some(person(m))),
            Err(serenity::Error::Http(e)) if e.status_code().is_some_and(|s| s.as_u16() == 404) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn person(member: serenity::Member) -> Person {
    Person { id: member.user.id.get(), name: member.display_name().to_string() }
}

async fn invoker(ctx: Context<'_>) -> Person {
    let name = match ctx.author_member().await {
        Some(m) => m.display_name().to_string(),
        None => ctx.author().display_name().to_string(),
    };
    Person { id: ctx.author().id.get(), name }
}

fn guild(ctx: Context<'_>) -> anyhow::Result<i64> {
    Ok(ctx.guild_id().ok_or_else(|| anyhow::anyhow!("guild only"))?.get() as i64)
}

/// Link Codeforces handles
#[poise::command(slash_command, guild_only, subcommands("handle_link", "handle_show", "handle_list", "handle_unlink"))]
pub async fn handle(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// Link your Codeforces handle (proved by a compile error)
#[poise::command(slash_command, guild_only, rename = "link")]
pub async fn handle_link(ctx: Context<'_>, #[description = "Your Codeforces handle"] handle: String) -> anyhow::Result<()> {
    ctx.defer_ephemeral().await?;
    let s: &Arc<Services> = ctx.data();
    let mut rng = rng();
    link(&s.db, s.cf.as_ref(), problems_of(s), &mut rng, &SlashOut(ctx), now(), guild(ctx)?, ctx.author().id.get(), &handle).await
}

/// Show a member's Codeforces profile
#[poise::command(slash_command, guild_only, rename = "show")]
pub async fn handle_show(
    ctx: Context<'_>,
    #[description = "Defaults to you"] member: Option<serenity::Member>,
    #[description = "Find the member who linked this Codeforces handle"] handle: Option<String>,
) -> anyhow::Result<()> {
    check_show_args(member.is_some(), handle.is_some())?;
    ctx.defer_ephemeral().await?;
    let s: &Arc<Services> = ctx.data();
    show(&s.db, s.cf.as_ref(), &SlashOut(ctx), guild(ctx)?, invoker(ctx).await, member.map(person), handle.as_deref()).await
}

/// Linked members ranked by Codeforces rating
#[poise::command(slash_command, guild_only, rename = "list")]
pub async fn handle_list(ctx: Context<'_>) -> anyhow::Result<()> {
    ctx.defer_ephemeral().await?;
    let s: &Arc<Services> = ctx.data();
    list(&s.db, s.cf.as_ref(), &SlashOut(ctx), guild(ctx)?).await
}

/// Remove your linked handle
#[poise::command(slash_command, guild_only, rename = "unlink")]
pub async fn handle_unlink(ctx: Context<'_>) -> anyhow::Result<()> {
    let s: &Arc<Services> = ctx.data();
    unlink(&s.db, &SlashOut(ctx), guild(ctx)?, ctx.author().id.get()).await
}

/// A Verify button press: the reply side of a component interaction.
struct ButtonOut<'a> {
    ctx: &'a serenity::Context,
    press: &'a ComponentInteraction,
    done: AtomicBool,
}

impl ButtonOut<'_> {
    async fn defer(&self) -> anyhow::Result<()> {
        let defer = CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true));
        self.press.create_response(self.ctx, defer).await?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Reply for ButtonOut<'_> {
    async fn followup_text(&self, content: &str) -> anyhow::Result<()> {
        let followup = CreateInteractionResponseFollowup::new().content(content).ephemeral(true);
        self.press.create_followup(self.ctx, followup).await?;
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
        let reply = CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(message).ephemeral(true));
        self.press.create_response(self.ctx, reply).await.map_err(|_| SendFailed)?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.followup_text(message).await.map_err(|_| SendFailed)
    }
    async fn delete_original(&self) -> Result<(), SendFailed> {
        self.press.delete_response(&self.ctx.http).await.map_err(|_| SendFailed)
    }
}

/// The persistent Verify button handler, for s18 to call from the `FullEvent::InteractionCreate` component branch
/// (Python `VerifyButton`, registered with `bot.add_dynamic_items`, so it works after a restart). Returns false when the
/// custom id is not `verify:<digits>` (not ours). A press by anyone but the user the id names gets no reply, like
/// `interaction_check` returning False. Errors are reported here, privately, like `VerifyButton.callback`.
pub async fn on_component(ctx: &serenity::Context, press: &ComponentInteraction, services: &Services) -> bool {
    let Some(owner) = parse_verify_id(&press.data.custom_id) else { return false };
    if press.user.id.get() != owner {
        return true;
    }
    let out = ButtonOut { ctx, press, done: AtomicBool::new(false) };
    let result = async {
        let guild: GuildId = press.guild_id.ok_or_else(|| anyhow::anyhow!("guild only"))?;
        out.defer().await?;
        verify(&services.db, services.cf.as_ref(), &out, now(), guild.get() as i64, owner).await
    }
    .await;
    if let Err(e) = result {
        errors::report_error(&out, &e).await;
    }
    true
}
