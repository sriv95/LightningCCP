import random
import time

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError, ChallengeConflict, NoChallenge, NotLinked
from src.logic import (
    filter_problems,
    gitgud_points,
    has_solved_since,
    parse_tags,
    round_rating,
    solved_keys,
    tag_suggestions,
)
from src.ui.embeds import problem_embed

Rating = app_commands.Range[int, 800, 3500]


class ProblemsCog(commands.Cog, name="Problems"):
    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @property
    def s(self):
        return self.bot.services

    @app_commands.command(name="gimme", description="Get a random Codeforces problem")
    @app_commands.guild_only()
    @app_commands.describe(
        rating="Exact rating",
        min_rating="Minimum rating",
        max_rating="Maximum rating",
        tags="Comma-separated tags; the problem must have all of them",
        exclude_tags="Comma-separated tags to avoid",
    )
    async def gimme(
        self,
        interaction: discord.Interaction,
        rating: Rating | None = None,
        min_rating: Rating | None = None,
        max_rating: Rating | None = None,
        tags: str | None = None,
        exclude_tags: str | None = None,
    ) -> None:
        await interaction.response.defer()
        s = self.s
        problems = s.problems.require()
        linked = await s.db.get_handle(interaction.guild_id, interaction.user.id)
        solved = frozenset()
        if linked:
            solved = solved_keys(await s.cf.user_status(linked.handle))
        if rating is not None:
            min_rating = max_rating = rating
        pool = filter_problems(
            problems,
            min_rating=min_rating,
            max_rating=max_rating,
            tags=parse_tags(tags),
            exclude_tags=parse_tags(exclude_tags),
            solved=solved,
        )
        if not pool:
            raise BotError("No problems match those filters.")
        footer = None if linked else "Link your handle with /handle link to hide solved problems."
        await interaction.followup.send(embed=problem_embed(random.choice(pool), footer=footer))

    @gimme.autocomplete("tags")
    @gimme.autocomplete("exclude_tags")
    async def tag_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        return [
            app_commands.Choice(name=value, value=value)
            for value in tag_suggestions(current, self.s.problems.tags)
        ]

    @app_commands.command(name="gitgud", description="Start a problem challenge")
    @app_commands.guild_only()
    @app_commands.describe(rating="Defaults to your rating rounded to 100")
    async def gitgud(self, interaction: discord.Interaction, rating: Rating | None = None) -> None:
        await interaction.response.defer()
        s = self.s
        guild_id, user_id = interaction.guild_id, interaction.user.id
        linked = await s.db.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        if await s.db.get_open_challenge(guild_id, user_id) is not None:
            raise ChallengeConflict()
        problems = s.problems.require()
        if rating is None:
            rating = round_rating((await s.cf.user_info(linked.handle)).rating)
        solved = solved_keys(await s.cf.user_status(linked.handle))
        pool = filter_problems(problems, min_rating=rating, max_rating=rating, solved=solved)
        if not pool:
            raise BotError(f"No unsolved problems rated {rating}.")
        problem = random.choice(pool)
        await s.db.open_challenge(guild_id, user_id, problem.key, rating, int(time.time()))
        await interaction.followup.send(
            embed=problem_embed(problem, footer="Solve it, then run /gotgud. Give up with /nogud.")
        )

    @app_commands.command(name="gotgud", description="Claim points for your solved challenge")
    @app_commands.guild_only()
    async def gotgud(self, interaction: discord.Interaction) -> None:
        await interaction.response.defer()
        s = self.s
        guild_id, user_id = interaction.guild_id, interaction.user.id
        linked = await s.db.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        challenge = await s.db.get_open_challenge(guild_id, user_id)
        if challenge is None:
            raise NoChallenge()
        subs = await s.cf.user_status(linked.handle, count=100)
        if not has_solved_since(subs, challenge.problem_key, challenge.issued_at):
            raise BotError(f"No accepted submission for {challenge.problem_key} yet.")
        await s.db.close_challenge(challenge.id, int(time.time()))
        earned = gitgud_points(challenge.rating)
        total = sum(gitgud_points(r) for r in await s.db.solved_ratings(guild_id, user_id))
        await interaction.followup.send(
            f"{interaction.user.mention} solved {challenge.problem_key}: +{earned} points "
            f"(total {total})."
        )

    @app_commands.command(name="nogud", description="Cancel your open challenge")
    @app_commands.guild_only()
    async def nogud(self, interaction: discord.Interaction) -> None:
        if not await self.s.db.cancel_challenge(interaction.guild_id, interaction.user.id):
            raise NoChallenge()
        await interaction.response.send_message("Challenge cancelled.", ephemeral=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ProblemsCog(bot))
