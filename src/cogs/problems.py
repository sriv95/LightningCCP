import random

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError, NotLinked
from src.logic import (
    filter_problems,
    parse_tags,
    round_rating,
    solved_keys,
    tag_suggestions,
    unsolved_keys,
)
from src.ui.embeds import daily_embed, problem_embed, stalk_pages
from src.ui.paginator import send_pages

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
        footer = None if linked else "Link your handle with /handle link to hide solved problems."
        if rating is None and min_rating is None and max_rating is None:
            # No range given: aim at the caller's match rating.
            mine = await s.db.rating(interaction.guild_id, interaction.user.id)
            rating = round_rating(mine)
            footer = f"Rating {rating} (from your match rating)." if linked else footer
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

    @app_commands.command(
        name="daily",
        description="Your daily problem: solve it for double rating (resets 00:00 UTC)",
    )
    @app_commands.guild_only()
    @app_commands.describe(
        rating="Problem rating (default: your match rating, floored to 100). "
        "A different rating than today's unsolved daily replaces it"
    )
    async def daily(self, interaction: discord.Interaction, rating: Rating | None = None) -> None:
        await interaction.response.defer()
        view = await self.s.daily.claim(
            interaction.guild_id, interaction.channel_id, interaction.user.id, rating
        )
        await interaction.followup.send(embed=daily_embed(view))

    @app_commands.command(name="stalk", description="List problems someone tried but never solved")
    @app_commands.guild_only()
    @app_commands.describe(
        handle="A Codeforces handle",
        member="A server member with a linked handle (default: you)",
    )
    async def stalk(
        self,
        interaction: discord.Interaction,
        handle: str | None = None,
        member: discord.Member | None = None,
    ) -> None:
        if handle is not None and member is not None:
            raise BotError("Give a handle or a member, not both.")
        await interaction.response.defer(ephemeral=True)
        s = self.s
        if handle is None:
            target = member or interaction.user
            linked = await s.db.get_handle(interaction.guild_id, target.id)
            if linked is None:
                raise NotLinked(f"{target.display_name} has no linked handle.")
            handle = linked.handle
        else:
            handle = (await s.cf.user_info(handle)).handle  # validates and fixes the casing
        keys = unsolved_keys(await s.cf.user_status(handle))
        by_key = {p.key: p for p in s.problems.problems}
        pages = stalk_pages(handle, [(k, by_key.get(k)) for k in keys])
        if not pages:
            raise BotError(f"{handle} has no attempted-but-unsolved problems.")
        await send_pages(interaction, pages)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ProblemsCog(bot))
