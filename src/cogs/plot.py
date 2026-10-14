import asyncio

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError, NotLinked
from src.logic import rating_series
from src.ui.plots import CF_BANDS, rating_plot

MAX_LINES = 5


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

    async def _linked_handle(
        self, interaction: discord.Interaction, target: discord.abc.User
    ) -> str:
        linked = await self.s.db.get_handle(interaction.guild_id, target.id)
        if linked is None:
            raise NotLinked(f"{target.display_name} has no linked handle.")
        return linked.handle

    async def _send(
        self,
        interaction: discord.Interaction,
        title: str,
        lines: list[tuple[str, list[tuple[int, int]]]],
        bands=(),
        note: str | None = None,
    ) -> None:
        image = await asyncio.to_thread(rating_plot, title, lines, bands)
        await interaction.followup.send(
            note, file=discord.File(image, "rating.png"), ephemeral=True
        )

    @plot.command(name="rating", description="Graph a member's match rating in this server")
    @app_commands.describe(
        member="Defaults to you",
        handle="Find the member who linked this Codeforces handle",
        member2="Compare with another member",
        member3="Compare with another member",
        member4="Compare with another member",
        member5="Compare with another member",
    )
    async def rating(
        self,
        interaction: discord.Interaction,
        member: discord.Member | None = None,
        handle: str | None = None,
        member2: discord.Member | None = None,
        member3: discord.Member | None = None,
        member4: discord.Member | None = None,
        member5: discord.Member | None = None,
    ) -> None:
        self._check_args(member, handle)
        extra = [m for m in (member2, member3, member4, member5) if m is not None]
        if extra and handle is not None:
            raise BotError("Compare members, not a handle.")
        await interaction.response.defer(ephemeral=True)
        db, guild_id = self.s.db, interaction.guild_id
        if extra:
            people = list({m.id: m for m in [member or interaction.user, *extra]}.values())
            lines, missing = [], []
            for person in people:
                points = rating_series(await db.rating_changes(guild_id, person.id))
                if points:
                    lines.append((person.display_name, points))
                else:
                    missing.append(person.display_name)
            if not lines:
                raise BotError("None of them has finished matches yet.")
            note = f"No finished matches yet: {', '.join(missing)}." if missing else None
            await self._send(interaction, "Match rating", lines, note=note)
            return
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
        await self._send(interaction, f"Match rating - {label}", [(label, points)])

    @plot.command(name="codeforces", description="Graph Codeforces rating histories")
    @app_commands.describe(
        member="Defaults to you",
        handle="Any Codeforces handle",
        handle2="Compare with another handle",
        handle3="Compare with another handle",
        handle4="Compare with another handle",
        handle5="Compare with another handle",
        member2="Compare with another member's linked handle",
        member3="Compare with another member's linked handle",
        member4="Compare with another member's linked handle",
        member5="Compare with another member's linked handle",
    )
    async def codeforces(
        self,
        interaction: discord.Interaction,
        member: discord.Member | None = None,
        handle: str | None = None,
        handle2: str | None = None,
        handle3: str | None = None,
        handle4: str | None = None,
        handle5: str | None = None,
        member2: discord.Member | None = None,
        member3: discord.Member | None = None,
        member4: discord.Member | None = None,
        member5: discord.Member | None = None,
    ) -> None:
        self._check_args(member, handle)
        await interaction.response.defer(ephemeral=True)
        s = self.s
        if handle is None:
            handle = await self._linked_handle(interaction, member or interaction.user)
        names = [handle, *(h for h in (handle2, handle3, handle4, handle5) if h)]
        for other in (member2, member3, member4, member5):
            if other is not None:
                names.append(await self._linked_handle(interaction, other))
        if len(names) > MAX_LINES:
            raise BotError(f"Compare at most {MAX_LINES} at once.")
        lines, missing, seen = [], [], set()
        for name in names:
            name = (await s.cf.user_info(name)).handle  # validates and fixes the casing
            if name.lower() in seen:
                continue
            seen.add(name.lower())
            changes = await s.cf.rating_history(name)
            if changes:
                lines.append((name, [(c.time, c.rating) for c in changes]))
            else:
                missing.append(name)
        if not lines:
            raise BotError(
                f"{', '.join(missing)} has not taken part in a rated contest."
                if len(missing) == 1
                else "None of them has taken part in a rated contest."
            )
        note = f"No rated contest yet: {', '.join(missing)}." if missing else None
        title = f"Codeforces rating - {lines[0][0]}" if len(lines) == 1 else "Codeforces rating"
        await self._send(interaction, title, lines, CF_BANDS, note)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(PlotCog(bot))
