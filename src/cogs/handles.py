import random
import re
import time

import discord
from discord import app_commands
from discord.ext import commands

from src.errors import BotError, HandleTaken, NotLinked, report_error
from src.logic import (
    LINK_MAX_RATING,
    LINK_WINDOW_SECONDS,
    filter_problems,
    has_compile_error,
    solved_keys,
)
from src.ui.embeds import profile_embed


class VerifyButton(discord.ui.DynamicItem[discord.ui.Button], template=r"verify:(?P<user>\d+)"):
    """Persistent button so a pending link survives a bot restart."""

    def __init__(self, user_id: int):
        super().__init__(
            discord.ui.Button(
                label="Verify", style=discord.ButtonStyle.success, custom_id=f"verify:{user_id}"
            )
        )
        self.user_id = user_id

    @classmethod
    async def from_custom_id(
        cls, interaction: discord.Interaction, item: discord.ui.Button, match: re.Match[str]
    ) -> "VerifyButton":
        return cls(int(match["user"]))

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.user_id

    async def callback(self, interaction: discord.Interaction) -> None:
        # discord.py never calls DynamicItem.on_error, so report here.
        try:
            cog = interaction.client.get_cog("Handles")
            await cog.verify(interaction)
        except Exception as exc:
            await report_error(interaction, exc)


class HandlesCog(commands.Cog, name="Handles"):
    handle = app_commands.Group(
        name="handle", description="Link Codeforces handles", guild_only=True
    )

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @property
    def s(self):
        return self.bot.services

    async def cog_load(self) -> None:
        self.bot.add_dynamic_items(VerifyButton)

    async def cog_unload(self) -> None:
        self.bot.remove_dynamic_items(VerifyButton)

    @handle.command(
        name="link", description="Link your Codeforces handle (proved by a compile error)"
    )
    @app_commands.describe(handle="Your Codeforces handle")
    async def link(self, interaction: discord.Interaction, handle: str) -> None:
        await interaction.response.defer(ephemeral=True)
        s = self.s
        user = await s.cf.user_info(handle)
        owner = await s.db.user_for_handle(interaction.guild_id, user.handle)
        if owner is not None and owner != interaction.user.id:
            raise HandleTaken(f"{user.handle} is already linked to <@{owner}>.")
        problems = s.problems.require()
        solved = solved_keys(await s.cf.user_status(user.handle))
        pool = filter_problems(problems, max_rating=LINK_MAX_RATING, solved=solved)
        pool = pool or filter_problems(problems, max_rating=LINK_MAX_RATING)
        if not pool:
            raise BotError("No suitable problem found. Try again later.")
        problem = random.choice(pool)
        await s.db.save_pending(
            interaction.guild_id, interaction.user.id, user.handle, problem.key, int(time.time())
        )
        view = discord.ui.View(timeout=None)
        view.add_item(VerifyButton(interaction.user.id))
        await interaction.followup.send(
            f"To prove you own **{user.handle}**, submit a **compile error** to "
            f"[{problem.key} - {problem.name}]({problem.url}) within "
            f"{LINK_WINDOW_SECONDS // 60} minutes, then press **Verify**.",
            view=view,
            ephemeral=True,
        )

    async def verify(self, interaction: discord.Interaction) -> None:
        await interaction.response.defer(ephemeral=True)
        s = self.s
        guild_id, user_id = interaction.guild_id, interaction.user.id
        pending = await s.db.get_pending(guild_id, user_id)
        if pending is None:
            raise BotError("No link in progress. Run /handle link again.")
        if int(time.time()) > pending.issued_at + LINK_WINDOW_SECONDS:
            await s.db.delete_pending(guild_id, user_id)
            raise BotError("That link attempt expired. Run /handle link again.")
        subs = await s.cf.user_status(pending.handle, count=20)
        if not has_compile_error(subs, pending.problem_key, pending.issued_at):
            raise BotError(
                f"No compile error found on {pending.problem_key} yet. "
                "Submit one on Codeforces, then press Verify again."
            )
        user = await s.cf.user_info(pending.handle)
        await s.db.link_handle(guild_id, user_id, user.handle, user.rating)
        await s.db.delete_pending(guild_id, user_id)
        await interaction.followup.send(f"Linked you to **{user.handle}**.", ephemeral=True)

    @handle.command(name="show", description="Show a member's Codeforces profile")
    @app_commands.describe(member="Defaults to you")
    async def show(
        self, interaction: discord.Interaction, member: discord.Member | None = None
    ) -> None:
        await interaction.response.defer(ephemeral=True)
        target = member or interaction.user
        linked = await self.s.db.get_handle(interaction.guild_id, target.id)
        if linked is None:
            raise NotLinked(f"{target.display_name} has no linked handle.")
        user = await self.s.cf.user_info(linked.handle)
        await interaction.followup.send(embed=profile_embed(user, target), ephemeral=True)

    @handle.command(name="whois", description="Find which member owns a handle")
    @app_commands.describe(handle="Codeforces handle")
    async def whois(self, interaction: discord.Interaction, handle: str) -> None:
        owner = await self.s.db.user_for_handle(interaction.guild_id, handle)
        if owner is None:
            raise BotError(f"No member is linked to {handle}.")
        await interaction.response.send_message(
            f"**{handle}** is <@{owner}>.",
            allowed_mentions=discord.AllowedMentions.none(),
            ephemeral=True,
        )

    @handle.command(name="unlink", description="Remove your linked handle")
    async def unlink(self, interaction: discord.Interaction) -> None:
        removed = await self.s.db.unlink(interaction.guild_id, interaction.user.id)
        if not removed:
            raise NotLinked("You have no linked handle.")
        await interaction.response.send_message("Unlinked.", ephemeral=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(HandlesCog(bot))
