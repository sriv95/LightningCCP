import asyncio

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError, NotLinked
from src.logic import rating_series
from src.ui.plots import CF_BANDS, rating_plot


class PlotCog(commands.Cog, name="Plot"):
    plot = app_commands.Group(name="plot", description="Rating graphs", guild_only=True)

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @property
    def s(self):
        return self.bot.services

    @staticmethod
    def _check_args(member: discord.Member | None, handle: str | None) -> None:
        if member is not None and handle is not None:
            raise BotError("Give a member or a handle, not both.")

    async def _send(
        self, interaction: discord.Interaction, title: str, points: list[tuple[int, int]], bands=()
    ) -> None:
        image = await asyncio.to_thread(rating_plot, title, points, bands)
        await interaction.followup.send(file=discord.File(image, "rating.png"), ephemeral=True)

    @plot.command(name="rating", description="Graph a member's match rating in this server")
    @app_commands.describe(
        member="Defaults to you", handle="Find the member who linked this Codeforces handle"
    )
    async def rating(
        self,
        interaction: discord.Interaction,
        member: discord.Member | None = None,
        handle: str | None = None,
    ) -> None:
        self._check_args(member, handle)
        await interaction.response.defer(ephemeral=True)
        db, guild_id = self.s.db, interaction.guild_id
        if handle is not None:
            user_id = await db.user_for_handle(guild_id, handle)
            if user_id is None:
                raise BotError(f"No member is linked to {handle}.")
            label = handle
        else:
            target = member or interaction.user
            user_id, label = target.id, target.display_name
        points = rating_series(await db.rating_changes(guild_id, user_id))
        if not points:
            raise BotError(f"{label} has no finished matches yet.")
        await self._send(interaction, f"Match rating - {label}", points)

    @plot.command(name="codeforces", description="Graph a Codeforces rating history")
    @app_commands.describe(member="Defaults to you", handle="Any Codeforces handle")
    async def codeforces(
        self,
        interaction: discord.Interaction,
        member: discord.Member | None = None,
        handle: str | None = None,
    ) -> None:
        self._check_args(member, handle)
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
        changes = await s.cf.rating_history(handle)
        if not changes:
            raise BotError(f"{handle} has not taken part in a rated contest.")
        points = [(c.time, c.rating) for c in changes]
        await self._send(interaction, f"Codeforces rating - {handle}", points, CF_BANDS)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(PlotCog(bot))
