//! Port of src/cogs/plot.py: `/plot rating` and `/plot codeforces`. The data step is `ui::plots::plan`; the
//! render step runs on a blocking thread (Python `asyncio.to_thread`).
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use poise::serenity_prelude as serenity;

use crate::bot::Context;
use crate::errors::BotError;
use crate::logic::base::rating_series;
use crate::services::Services;
use crate::ui::plots::{CF_BANDS, rating_plot};

pub const MAX_LINES: usize = 5;
pub const FILENAME: &str = "rating.png";

pub type Lines = Vec<(String, Vec<(i64, i64)>)>;

/// A guild member as the cog sees one: id and `display_name`.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: u64,
    pub name: String,
}

/// What the cog writes to Discord: the followup of a deferred ephemeral reply.
pub trait PlotOutput: Sync {
    /// `interaction.followup.send(note, file=discord.File(png, "rating.png"), ephemeral=True)`
    fn send_plot(&self, note: Option<String>, filename: &str, png: Vec<u8>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// The store and Codeforces reads the cog needs.
pub trait PlotData: Sync {
    fn linked_handle(&self, guild: i64, user: u64) -> impl Future<Output = anyhow::Result<Option<String>>> + Send;
    fn user_for_handle(&self, guild: i64, handle: &str) -> impl Future<Output = anyhow::Result<Option<i64>>> + Send;
    fn rating_changes(&self, guild: i64, user: i64) -> impl Future<Output = anyhow::Result<Vec<(i64, i64)>>> + Send;
    /// `(await cf.user_info(name)).handle`: validates and fixes the casing.
    fn cf_handle(&self, name: &str) -> impl Future<Output = anyhow::Result<String>> + Send;
    fn cf_history(&self, name: &str) -> impl Future<Output = anyhow::Result<Vec<(i64, i64)>>> + Send;
}

impl PlotData for Services {
    async fn linked_handle(&self, guild: i64, user: u64) -> anyhow::Result<Option<String>> {
        Ok(self.db.get_handle(guild, user as i64).await?.map(|l| l.handle))
    }
    async fn user_for_handle(&self, guild: i64, handle: &str) -> anyhow::Result<Option<i64>> {
        self.db.user_for_handle(guild, handle).await
    }
    async fn rating_changes(&self, guild: i64, user: i64) -> anyhow::Result<Vec<(i64, i64)>> {
        self.db.rating_changes(guild, user).await
    }
    async fn cf_handle(&self, name: &str) -> anyhow::Result<String> {
        Ok(self.cf.user_info(name).await?.handle)
    }
    async fn cf_history(&self, name: &str) -> anyhow::Result<Vec<(i64, i64)>> {
        Ok(self.cf.rating_history(name).await?.into_iter().map(|c| (c.time, c.rating)).collect())
    }
}

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

pub use crate::ui::embeds::a::esc;

/// Python `_check_args`.
pub fn check_args(member: bool, handle: bool) -> anyhow::Result<()> {
    if member && handle {
        return Err(plain("Give a member or a handle, not both."));
    }
    Ok(())
}

/// The argument checks of `/plot rating` that run before the defer.
pub fn check_rating_args(member: bool, handle: bool, extra: usize) -> anyhow::Result<()> {
    check_args(member, handle)?;
    if extra > 0 && handle {
        return Err(plain("Compare members, not a handle."));
    }
    Ok(())
}

/// What one plot command draws and says: the data step's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub title: String,
    pub lines: Lines,
    pub bands: &'static [(i64, i64, &'static str)],
    pub note: Option<String>,
}

fn request(title: impl Into<String>, lines: Lines, bands: &'static [(i64, i64, &'static str)], note: Option<String>) -> Request {
    Request { title: title.into(), lines, bands, note }
}

/// Render on a blocking thread (Python `asyncio.to_thread(rating_plot, ...)`), then send.
pub async fn reply<O: PlotOutput>(out: &O, req: Request) -> anyhow::Result<()> {
    let Request { title, lines, bands, note } = req;
    let png = tokio::task::spawn_blocking(move || rating_plot(&title, &lines, bands)).await??;
    out.send_plot(note, FILENAME, png).await
}

/// `/plot rating`, after the defer: the request to draw. `member` is the explicit member option, `invoker` the caller.
pub async fn rating<D: PlotData>(
    data: &D,
    guild: i64,
    invoker: Person,
    member: Option<Person>,
    handle: Option<&str>,
    extra: Vec<Person>,
) -> anyhow::Result<Request> {
    if !extra.is_empty() {
        // dedupe by id, first position kept
        let mut seen = HashSet::new();
        let people: Vec<Person> = std::iter::once(member.unwrap_or(invoker))
            .chain(extra)
            .filter(|p| seen.insert(p.id))
            .collect();
        let (mut lines, mut missing) = (vec![], vec![]);
        for person in people {
            let points = rating_series(&data.rating_changes(guild, person.id as i64).await?);
            if points.is_empty() {
                missing.push(person.name);
            } else {
                lines.push((person.name, points));
            }
        }
        if lines.is_empty() {
            return Err(plain("None of them has finished matches yet."));
        }
        let note = (!missing.is_empty()).then(|| format!("No finished matches yet: {}.", missing.join(", ")));
        return Ok(request("Match rating", lines, &[], note));
    }
    let (user_id, label) = match handle {
        Some(handle) => match data.user_for_handle(guild, handle).await? {
            Some(id) => (id, handle.to_string()),
            None => return Err(plain(format!("No member is linked to {}.", esc(handle)))),
        },
        None => {
            let target = member.unwrap_or(invoker);
            (target.id as i64, target.name)
        }
    };
    let points = rating_series(&data.rating_changes(guild, user_id).await?);
    if points.is_empty() {
        return Err(plain(format!("{label} has no finished matches yet.")));
    }
    Ok(request(format!("Match rating - {label}"), vec![(label, points)], &[], None))
}

async fn linked_handle<D: PlotData>(data: &D, guild: i64, target: &Person) -> anyhow::Result<String> {
    data.linked_handle(guild, target.id)
        .await?
        .ok_or_else(|| BotError::NotLinked(format!("{} has no linked handle.", target.name)).into())
}

/// `/plot codeforces`, after the defer: the request to draw. `others` are `handle2..handle5` (empty strings are skipped, like
/// Python's `if h`), `members` are `member2..member5`.
pub async fn codeforces<D: PlotData>(
    data: &D,
    guild: i64,
    invoker: Person,
    member: Option<Person>,
    handle: Option<String>,
    others: Vec<Option<String>>,
    members: Vec<Person>,
) -> anyhow::Result<Request> {
    let handle = match handle {
        Some(h) => h,
        None => linked_handle(data, guild, &member.unwrap_or(invoker)).await?,
    };
    let mut names = vec![handle];
    names.extend(others.into_iter().flatten().filter(|h| !h.is_empty()));
    for other in &members {
        names.push(linked_handle(data, guild, other).await?);
    }
    if names.len() > MAX_LINES {
        return Err(plain(format!("Compare at most {MAX_LINES} at once.")));
    }
    let (mut lines, mut missing, mut seen) = (vec![], vec![], HashSet::new());
    for name in names {
        let name = data.cf_handle(&name).await?; // validates and fixes the casing
        if !seen.insert(name.to_lowercase()) {
            continue;
        }
        let changes = data.cf_history(&name).await?;
        if changes.is_empty() {
            missing.push(name);
        } else {
            lines.push((name, changes));
        }
    }
    if lines.is_empty() {
        return Err(plain(if missing.len() == 1 {
            format!("{} has not taken part in a rated contest.", missing.join(", "))
        } else {
            "None of them has taken part in a rated contest.".to_string()
        }));
    }
    let note = (!missing.is_empty()).then(|| format!("No rated contest yet: {}.", missing.join(", ")));
    let title = if lines.len() == 1 { format!("Codeforces rating - {}", lines[0].0) } else { "Codeforces rating".into() };
    Ok(request(title, lines, &CF_BANDS, note))
}

/// The real output: the followup of a deferred ephemeral slash command.
struct CtxOutput<'a>(Context<'a>);

impl PlotOutput for CtxOutput<'_> {
    async fn send_plot(&self, note: Option<String>, filename: &str, png: Vec<u8>) -> anyhow::Result<()> {
        let mut reply = poise::CreateReply::default()
            .attachment(serenity::CreateAttachment::bytes(png, filename))
            .ephemeral(true);
        if let Some(note) = note {
            reply = reply.content(note);
        }
        self.0.send(reply).await?;
        Ok(())
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

/// Rating graphs
#[poise::command(slash_command, guild_only, subcommands("plot_rating", "plot_codeforces"))]
pub async fn plot(_ctx: Context<'_>) -> anyhow::Result<()> {
    Ok(())
}

/// Graph a member's match rating in this server
#[poise::command(slash_command, guild_only, rename = "rating")]
#[allow(clippy::too_many_arguments)]
pub async fn plot_rating(
    ctx: Context<'_>,
    #[description = "Defaults to you"] member: Option<serenity::Member>,
    #[description = "Find the member who linked this Codeforces handle"] handle: Option<String>,
    #[description = "Compare with another member"] member2: Option<serenity::Member>,
    #[description = "Compare with another member"] member3: Option<serenity::Member>,
    #[description = "Compare with another member"] member4: Option<serenity::Member>,
    #[description = "Compare with another member"] member5: Option<serenity::Member>,
) -> anyhow::Result<()> {
    let extra: Vec<_> = [member2, member3, member4, member5].into_iter().flatten().collect();
    check_rating_args(member.is_some(), handle.is_some(), extra.len())?;
    ctx.defer_ephemeral().await?;
    let extra_people = extra.into_iter().map(person).collect();
    let member = member.map(person);
    let services: &Arc<Services> = ctx.data();
    let req = rating(services.as_ref(), guild(ctx)?, invoker(ctx).await, member, handle.as_deref(), extra_people).await?;
    reply(&CtxOutput(ctx), req).await
}

/// Graph Codeforces rating histories
#[poise::command(slash_command, guild_only, rename = "codeforces")]
#[allow(clippy::too_many_arguments)]
pub async fn plot_codeforces(
    ctx: Context<'_>,
    #[description = "Defaults to you"] member: Option<serenity::Member>,
    #[description = "Any Codeforces handle"] handle: Option<String>,
    #[description = "Compare with another handle"] handle2: Option<String>,
    #[description = "Compare with another handle"] handle3: Option<String>,
    #[description = "Compare with another handle"] handle4: Option<String>,
    #[description = "Compare with another handle"] handle5: Option<String>,
    #[description = "Compare with another member's linked handle"] member2: Option<serenity::Member>,
    #[description = "Compare with another member's linked handle"] member3: Option<serenity::Member>,
    #[description = "Compare with another member's linked handle"] member4: Option<serenity::Member>,
    #[description = "Compare with another member's linked handle"] member5: Option<serenity::Member>,
) -> anyhow::Result<()> {
    check_args(member.is_some(), handle.is_some())?;
    ctx.defer_ephemeral().await?;
    let members = [member2, member3, member4, member5].into_iter().flatten().map(person).collect();
    let member = member.map(person);
    let services: &Arc<Services> = ctx.data();
    let others = vec![handle2, handle3, handle4, handle5];
    let req = codeforces(services.as_ref(), guild(ctx)?, invoker(ctx).await, member, handle, others, members).await?;
    reply(&CtxOutput(ctx), req).await
}
