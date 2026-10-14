from typing import Literal

import discord
from discord import app_commands
from discord.ext import commands

from src.ui.embeds import status_embed
from src.ui.help import help_embed


class HelpCog(commands.Cog, name="Help"):
    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @app_commands.command(name="help", description="How to use this bot")
    @app_commands.describe(topic="What to read about; leave empty for an overview")
    async def help(
        self,
        interaction: discord.Interaction,
        topic: Literal["start", "problems", "matches", "contests", "ratings", "contest-list"]
        | None = None,
    ) -> None:
        await interaction.response.send_message(embed=help_embed(topic), ephemeral=True)

    @app_commands.command(name="status", description="How the bot is doing")
    async def status(self, interaction: discord.Interaction) -> None:
        s = self.bot.services
        matches, mashups = await s.db.unfinished_counts()
        embed = status_embed(
            latency_ms=round(self.bot.latency * 1000),
            started_at=self.bot.started_at,
            problems=len(s.problems.problems),
            problems_at=s.problems.refreshed_at,
            contests=len(s.contests.contests),
            contests_at=s.contests.refreshed_at,
            requests=s.cf.requests,
            rate_limited=s.cf.rate_limited,
            matches=matches,
            mashups=mashups,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(HelpCog(bot))
