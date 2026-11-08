//! Port of src/cogs/problems.py: `/gimme` (with tag and contest autocomplete), `/daily` and `/stalk`.
//! Cores over `CfApi`, `ProblemSource`, the real `Store` and a narrow output trait, plus a private poise implementation.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use rand::Rng;
use rand::seq::IndexedRandom;

use crate::bot::Context;
use crate::cache::problems::ProblemCache;
use crate::cf::client::CodeforcesClient;
use crate::cf::models::{Contest, Problem};
use crate::cogs::plot::Person;
use crate::daily::{CfApi, DailyService, ProblemSource};
use crate::errors::BotError;
use crate::logic::base::{
    ProblemFilter, contest_suggestions, filter_problems, parse_tags, round_rating, solved_keys, tag_suggestions, unsolved_keys,
};
use crate::services::Services;
use crate::store::base::Store;
use crate::ui::embeds::a::{EmbedData, daily_embed, esc, problem_embed, stalk_pages};

pub const AUTOCOMPLETE_LIMIT: usize = 25;
pub const STALK_PER_PAGE: usize = 15;
pub type Daily = DailyService<CodeforcesClient, ProblemCache>;

fn plain(message: impl Into<String>) -> anyhow::Error {
    BotError::Plain(message.into()).into()
}

/// What the commands write to Discord.
pub trait ProblemsOutput: Sync {
    /// `interaction.followup.send(embed=embed)` of a public defer.
    fn followup_embed(&self, embed: EmbedData) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `send_pages(interaction, pages)` of an ephemeral defer.
    fn followup_pages(&self, pages: Vec<EmbedData>) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// The options of `/gimme`.
#[derive(Debug, Clone, Default)]
pub struct GimmeArgs {
    pub rating: Option<i64>,
    pub min_rating: Option<i64>,
    pub max_rating: Option<i64>,
    pub tags: Option<String>,
    pub exclude_tags: Option<String>,
    pub contest_id: Option<i64>,
}

/// `/gimme`, after the (public) defer.
#[allow(clippy::too_many_arguments)]
pub async fn gimme<C: CfApi, P: ProblemSource>(
    db: &Store,
    cf: &C,
    problems: &P,
    rng: &mut (impl Rng + Send),
    out: &impl ProblemsOutput,
    guild: i64,
    user_id: u64,
    args: GimmeArgs,
) -> anyhow::Result<()> {
    let GimmeArgs { mut rating, mut min_rating, mut max_rating, tags, exclude_tags, contest_id } = args;
    let problems = problems.require()?;
    let linked = db.get_handle(guild, user_id as i64).await?;
    let mut solved = HashSet::new();
    if let Some(linked) = &linked {
        solved = solved_keys(&cf.user_status(&linked.handle, None).await?);
    }
    let mut footer =
        if linked.is_some() { None } else { Some("Link your handle with /handle link to hide solved problems.".to_string()) };
    if rating.is_none() && min_rating.is_none() && max_rating.is_none() && contest_id.is_none() {
        // No range or contest given: aim at the caller's match rating.
        let r = round_rating(Some(db.rating(guild, user_id as i64).await?));
        rating = Some(r);
        if linked.is_some() {
            footer = Some(format!("Rating {r} (from your match rating)."));
        }
    }
    if let Some(r) = rating {
        min_rating = Some(r);
        max_rating = Some(r);
    }
    let pool = filter_problems(
        &problems,
        &ProblemFilter {
            min_rating,
            max_rating,
            tags: parse_tags(tags.as_deref()),
            exclude_tags: parse_tags(exclude_tags.as_deref()),
            solved,
            contest_id,
        },
    );
    let Some(problem) = pool.choose(rng) else {
        return Err(plain("No problems match those filters."));
    };
    out.followup_embed(problem_embed(problem, footer.as_deref())).await
}

/// Autocomplete of `tags` and `exclude_tags`: up to 25 tag suggestions, each a whole comma-separated value.
pub fn tag_choices(current: &str, tags: &HashSet<String>) -> Vec<String> {
    let all: Vec<String> = tags.iter().cloned().collect();
    tag_suggestions(current, &all, AUTOCOMPLETE_LIMIT)
}

/// Autocomplete of `contest_id`: (label, contest id) of finished contests that have rated problems in the cache.
pub fn contest_choices(contests: &[Contest], problems: &[Problem], current: &str) -> Vec<(String, i64)> {
    let known: HashSet<i64> = problems.iter().filter(|p| p.rating.is_some_and(|r| r != 0)).map(|p| p.contest_id).collect();
    contest_suggestions(contests, &known, current, AUTOCOMPLETE_LIMIT)
}

/// `/daily`, after the (public) defer.
pub async fn daily<C: CfApi, P: ProblemSource>(
    service: &DailyService<C, P>,
    out: &impl ProblemsOutput,
    guild: i64,
    channel: i64,
    user_id: u64,
    rating: Option<i64>,
) -> anyhow::Result<()> {
    let view = service.claim(guild, channel, user_id as i64, rating).await?;
    out.followup_embed(daily_embed(&view)).await
}

/// The argument check of `/stalk` that runs before the defer.
pub fn check_stalk_args(handle: bool, member: bool) -> anyhow::Result<()> {
    if handle && member {
        return Err(plain("Give a handle or a member, not both."));
    }
    Ok(())
}

/// `/stalk`, after the (ephemeral) defer.
#[allow(clippy::too_many_arguments)]
pub async fn stalk<C: CfApi, P: ProblemSource>(
    db: &Store,
    cf: &C,
    problems: &P,
    out: &impl ProblemsOutput,
    guild: i64,
    invoker: Person,
    member: Option<Person>,
    handle: Option<&str>,
) -> anyhow::Result<()> {
    let handle = match handle {
        None => {
            let target = member.unwrap_or(invoker);
            let Some(linked) = db.get_handle(guild, target.id as i64).await? else {
                return Err(BotError::NotLinked(format!("{} has no linked handle.", target.name)).into());
            };
            linked.handle
        }
        Some(h) => cf.user_info(h).await?.handle, // validates and fixes the casing
    };
    let keys = unsolved_keys(&cf.user_status(&handle, None).await?);
    let all = problems.problems();
    let by_key: HashMap<String, &Problem> = all.iter().map(|p| (p.key(), p)).collect();
    let items: Vec<(String, Option<Problem>)> =
        keys.into_iter().map(|k| (k.clone(), by_key.get(&k).map(|p| (*p).clone()))).collect();
    let pages = stalk_pages(&handle, &items, STALK_PER_PAGE)?;
    if pages.is_empty() {
        return Err(plain(format!("{} has no attempted-but-unsolved problems.", esc(&handle))));
    }
    out.followup_pages(pages).await
}

// --- the Discord side -----------------------------------------------------------------------------------------

fn problems_of(services: &Services) -> &ProblemCache {
    &services.problems
}

fn contests_of(services: &Services) -> &crate::cache::contests::ContestCache {
    &services.contests
}

fn daily_of(services: &Services) -> &Daily {
    &services.daily
}

fn rng() -> rand::rngs::StdRng {
    rand::SeedableRng::from_rng(&mut rand::rng())
}

struct SlashOut<'a>(Context<'a>);

impl ProblemsOutput for SlashOut<'_> {
    async fn followup_embed(&self, embed: EmbedData) -> anyhow::Result<()> {
        self.0.send(poise::CreateReply::default().embed(embed.into())).await?;
        Ok(())
    }
    async fn followup_pages(&self, pages: Vec<EmbedData>) -> anyhow::Result<()> {
        crate::bot::DiscordOutput::from_ctx(self.0)?.send_pages(pages.into_iter().map(Into::into).collect()).await
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

async fn tag_autocomplete(ctx: Context<'_>, current: &str) -> serenity::CreateAutocompleteResponse {
    let services: &Arc<Services> = ctx.data();
    let choices = tag_choices(current, &problems_of(services).tags()).into_iter().map(|v| serenity::AutocompleteChoice::new(v.clone(), v)).collect::<Vec<_>>();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

async fn contest_autocomplete(ctx: Context<'_>, current: &str) -> serenity::CreateAutocompleteResponse {
    let services: &Arc<Services> = ctx.data();
    let (problems, contests) = (problems_of(services).problems(), contests_of(services).contests());
    let choices = contest_choices(&contests, &problems, current).into_iter().map(|(label, id)| serenity::AutocompleteChoice::new(label, id)).collect::<Vec<_>>();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

/// Get a random Codeforces problem
#[poise::command(slash_command, guild_only, rename = "gimme")]
#[allow(clippy::too_many_arguments)]
pub async fn gimme_command(
    ctx: Context<'_>,
    #[description = "Exact rating"]
    #[min = 800]
    #[max = 3500]
    rating: Option<i64>,
    #[description = "Minimum rating"]
    #[min = 800]
    #[max = 3500]
    min_rating: Option<i64>,
    #[description = "Maximum rating"]
    #[min = 800]
    #[max = 3500]
    max_rating: Option<i64>,
    #[description = "Comma-separated tags; the problem must have all of them"]
    #[autocomplete = "tag_autocomplete"]
    tags: Option<String>,
    #[description = "Comma-separated tags to avoid"]
    #[autocomplete = "tag_autocomplete"]
    exclude_tags: Option<String>,
    #[description = "Only problems from this Codeforces contest (type a name or id)"]
    #[autocomplete = "contest_autocomplete"]
    contest_id: Option<i64>,
) -> anyhow::Result<()> {
    ctx.defer().await?;
    let s: &Arc<Services> = ctx.data();
    let args = GimmeArgs { rating, min_rating, max_rating, tags, exclude_tags, contest_id };
    let mut rng = rng();
    gimme(&s.db, s.cf.as_ref(), problems_of(s), &mut rng, &SlashOut(ctx), guild(ctx)?, ctx.author().id.get(), args).await
}

/// Your daily problem: solve it for double rating (resets 00:00 UTC)
#[poise::command(slash_command, guild_only, rename = "daily")]
pub async fn daily_command(
    ctx: Context<'_>,
    #[description = "Problem rating (default: your match rating, floored to 100). A different rating than today's…"] // Python's description cut to what discord.py sends (Discord rejects over 100 characters)
    #[min = 800]
    #[max = 3500]
    rating: Option<i64>,
) -> anyhow::Result<()> {
    ctx.defer().await?;
    let s: &Arc<Services> = ctx.data();
    let channel = ctx.channel_id().get() as i64;
    daily(daily_of(s), &SlashOut(ctx), guild(ctx)?, channel, ctx.author().id.get(), rating).await
}

/// List problems someone tried but never solved
#[poise::command(slash_command, guild_only, rename = "stalk")]
pub async fn stalk_command(
    ctx: Context<'_>,
    #[description = "A Codeforces handle"] handle: Option<String>,
    #[description = "A server member with a linked handle (default: you)"] member: Option<serenity::Member>,
) -> anyhow::Result<()> {
    check_stalk_args(handle.is_some(), member.is_some())?;
    ctx.defer_ephemeral().await?;
    let s: &Arc<Services> = ctx.data();
    stalk(&s.db, s.cf.as_ref(), problems_of(s), &SlashOut(ctx), guild(ctx)?, invoker(ctx).await, member.map(person), handle.as_deref())
        .await
}
