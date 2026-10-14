import random

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError
from src.logic import (
    filter_problems,
    parse_tags,
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


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ProblemsCog(bot))
