from typing import Literal

import discord
from discord import app_commands
from discord.ext import commands

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


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(HelpCog(bot))
