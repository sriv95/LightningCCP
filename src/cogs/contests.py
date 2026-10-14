import logging
import time

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.errors import BotError, CacheNotReady
from src.logic import parse_minutes, reminders_due, select_contests
from src.ui.embeds import contest_pages
from src.ui.paginator import send_pages

log = logging.getLogger(__name__)

TITLES = {
    "upcoming": "Upcoming contests",
    "running": "Running contests",
    "recent": "Recently finished contests",
}


class ContestsCog(commands.Cog, name="Contests"):
    contests = app_commands.Group(
        name="contests", description="List Codeforces contests", guild_only=True
    )
    remind = app_commands.Group(
        name="remind",
        description="Contest reminders",
        guild_only=True,
        default_permissions=discord.Permissions(manage_guild=True),
    )

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        self._warned_channels: set[tuple[int, int]] = set()

    @property
    def s(self):
        return self.bot.services

    async def cog_load(self) -> None:
        self.remind_loop.start()

    async def cog_unload(self) -> None:
        self.remind_loop.cancel()

    async def _list(self, interaction: discord.Interaction, kind: str) -> None:
        if not self.s.contests.contests:
            raise CacheNotReady()
        pages = contest_pages(select_contests(self.s.contests.contests, kind), TITLES[kind])
        if not pages:
            raise BotError(f"No {kind} contests.")
        await interaction.response.defer(ephemeral=True)
        await send_pages(interaction, pages)

    @contests.command(name="upcoming", description="List future contests")
    async def upcoming(self, interaction: discord.Interaction) -> None:
        await self._list(interaction, "upcoming")

    @contests.command(name="running", description="List contests running now")
    async def running(self, interaction: discord.Interaction) -> None:
        await self._list(interaction, "running")

    @contests.command(name="recent", description="List recently finished contests")
    async def recent(self, interaction: discord.Interaction) -> None:
        await self._list(interaction, "recent")

    @remind.command(name="set", description="Post contest reminders in a channel")
    @app_commands.describe(
        channel="Where to post",
        role="Role to ping (optional)",
        before="Minutes before the start, comma-separated (default 10,60)",
    )
    @app_commands.checks.has_permissions(manage_guild=True)
    async def set_reminder(
        self,
        interaction: discord.Interaction,
        channel: discord.TextChannel,
        role: discord.Role | None = None,
        before: str = "10,60",
    ) -> None:
        mins = parse_minutes(before)
        if not channel.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {channel.mention}.")
        await self.s.db.set_reminder(
            interaction.guild_id, channel.id, role.id if role else None, mins
        )
        await interaction.response.send_message(
            f"Reminders go to {channel.mention} {', '.join(str(m) for m in mins)} minutes before start.",
            ephemeral=True,
        )

    @remind.command(name="show", description="Show reminder settings")
    @app_commands.checks.has_permissions(manage_guild=True)
    async def show_reminder(self, interaction: discord.Interaction) -> None:
        reminder = await self.s.db.get_reminder(interaction.guild_id)
        if reminder is None:
            raise BotError("No reminders configured. Use /remind set.")
        role = f" pinging <@&{reminder.role_id}>" if reminder.role_id else ""
        await interaction.response.send_message(
            f"<#{reminder.channel_id}>{role}, "
            f"{', '.join(str(m) for m in reminder.before_mins)} minutes before start.",
            ephemeral=True,
        )

    @remind.command(name="clear", description="Stop contest reminders")
    @app_commands.checks.has_permissions(manage_guild=True)
    async def clear_reminder(self, interaction: discord.Interaction) -> None:
        if not await self.s.db.clear_reminder(interaction.guild_id):
            raise BotError("No reminders configured.")
        await interaction.response.send_message("Reminders cleared.", ephemeral=True)

    @tasks.loop(seconds=60)
    async def remind_loop(self) -> None:
        try:
            await self._send_due_reminders()
        except Exception:
            log.exception("Reminder loop failed")

    @remind_loop.before_loop
    async def _before_remind_loop(self) -> None:
        await self.bot.wait_until_ready()

    async def _send_due_reminders(self) -> None:
        s = self.s
        now = int(time.time())
        for reminder in await s.db.all_reminders():
            channel = self.bot.get_channel(reminder.channel_id)
            if channel is None:
                key = (reminder.guild_id, reminder.channel_id)
                if key not in self._warned_channels:
                    self._warned_channels.add(key)
                    log.warning(
                        "Reminder channel %s not found for guild %s",
                        reminder.channel_id,
                        reminder.guild_id,
                    )
                continue
            sent = await s.db.sent_for_guild(reminder.guild_id)
            offsets: dict[int, list[int]] = {}
            contests = {}
            for contest, before in reminders_due(
                s.contests.contests, reminder.before_mins, sent, now
            ):
                contests[contest.id] = contest
                offsets.setdefault(contest.id, []).append(before)
            for contest_id, befores in offsets.items():
                contest = contests[contest_id]
                # mark all due offsets so catch-up posts one message; mark the smallest last
                # (at most once per (guild, contest, offset), even across restarts)
                befores.sort(reverse=True)
                for before in befores[:-1]:
                    await s.db.mark_sent(reminder.guild_id, contest_id, before)
                if not await s.db.mark_sent(reminder.guild_id, contest_id, befores[-1]):
                    continue
                ping = f"<@&{reminder.role_id}> " if reminder.role_id else ""
                try:
                    await channel.send(
                        f"{ping}**[{contest.name}]({contest.url})** starts "
                        f"<t:{contest.start_time}:R>.",
                        allowed_mentions=discord.AllowedMentions(roles=True),
                    )
                except discord.HTTPException:
                    log.warning("Could not send reminder to %s", reminder.channel_id, exc_info=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ContestsCog(bot))
