import logging
import re
import time

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.db.store import Reminder
from src.errors import BotError, CacheNotReady
from src.logic import parse_minutes, recaps_due, reminders_due, select_contests
from src.ui.embeds import contest_pages, ranklist_pages, recap_embed
from src.ui.paginator import send_pages

log = logging.getLogger(__name__)

TITLES = {
    "upcoming": "Upcoming contests",
    "running": "Running contests",
    "recent": "Recently finished contests",
}

MAX_EXCLUDE_LENGTH = 100


def _check_regex(raw: str) -> str:
    pattern = raw.strip()
    if len(pattern) > MAX_EXCLUDE_LENGTH:
        raise BotError(f"The regex can be at most {MAX_EXCLUDE_LENGTH} characters.")
    try:
        re.compile(pattern)
    except re.error as e:
        raise BotError(f"Invalid regex: {e}") from None
    return pattern


def _not_excluded(contests, pattern: str):
    if not pattern:
        return contests
    rx = re.compile(pattern, re.IGNORECASE)
    return [c for c in contests if not rx.search(c.name)]


def _reminder_text(r: Reminder) -> str:
    where = f"<#{r.channel_id}>" if r.channel_id else "no channel (use set)"
    role = f", pinging <@&{r.role_id}>" if r.role_id else ""
    skip = f"\nExcluding names matching `{r.exclude}`." if r.exclude else ""
    mins = ", ".join(str(m) for m in r.before_mins)
    return f"Reminders: {where}{role}, {mins} minutes before start.{skip}"


class ContestsCog(commands.Cog, name="Contests"):
    contests = app_commands.Group(
        name="contests", description="List Codeforces contests", guild_only=True
    )
    channel = app_commands.Group(
        name="channel",
        description="Channels for contest reminders and recaps",
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

    async def _entries(
        self, guild_id: int, contest_id: int
    ) -> list[tuple[int, int, str, float, int]]:
        """(rank, user_id, handle, points, penalty) of this server's linked members, best first."""
        s = self.s
        linked = await s.db.linked_handles(guild_id)
        if not linked:
            return []
        owners = {handle.lower(): user_id for user_id, handle in linked}
        entries = []
        for row in await s.cf.standings(contest_id, [handle for _, handle in linked]):
            # A team row counts for the first member who is linked here.
            handle = next((h for h in row.handles if h.lower() in owners), None)
            if handle is not None:
                entries.append((row.rank, owners[handle.lower()], handle, row.points, row.penalty))
        entries.sort()  # chunked Codeforces calls arrive in separate rank order
        return entries

    @app_commands.command(name="ranklist", description="Standings of linked members in a contest")
    @app_commands.describe(contest_id="Codeforces contest id")
    @app_commands.guild_only()
    async def ranklist(self, interaction: discord.Interaction, contest_id: int) -> None:
        await interaction.response.defer(ephemeral=True)
        s = self.s
        contest = next((c for c in s.contests.contests if c.id == contest_id), None)
        if contest is not None and contest.phase == "BEFORE":
            raise BotError("That contest has not started yet.")
        entries = await self._entries(interaction.guild_id, contest_id)
        title = f"Ranklist - {contest.name}" if contest else f"Ranklist - contest {contest_id}"
        pages = ranklist_pages(entries, title)
        if not pages:
            raise BotError("No linked member took part in that contest.")
        await send_pages(interaction, pages)

    @channel.command(
        name="reminder", description="Where to remind about upcoming contests; no options shows it"
    )
    @app_commands.describe(
        set="Channel to post reminders in",
        role="Role to ping",
        before="Minutes before the start, comma-separated (default 10,60)",
        exclude="Regex on the contest name: matching contests get no reminder; clear removes it",
        clear="Stop reminders (forgets the channel only)",
    )
    @app_commands.checks.has_permissions(manage_guild=True)
    async def reminder_config(
        self,
        interaction: discord.Interaction,
        set: discord.TextChannel | None = None,
        role: discord.Role | None = None,
        before: str | None = None,
        exclude: str | None = None,
        clear: bool = False,
    ) -> None:
        guild_id = interaction.guild_id
        current = await self.s.db.get_reminder(guild_id) or Reminder(guild_id, None, None, (60, 10))
        if clear:
            if set or role or before or exclude:
                raise BotError("Use clear on its own.")
            if not await self.s.db.clear_reminder(guild_id):
                raise BotError("No reminder channel set.")
            await interaction.response.send_message("Reminders stopped.", ephemeral=True)
            return
        if set is None and role is None and before is None and exclude is None:
            await interaction.response.send_message(_reminder_text(current), ephemeral=True)
            return
        if set and not set.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {set.mention}.")
        pattern = current.exclude
        if exclude is not None:
            pattern = "" if exclude.strip().lower() == "clear" else _check_regex(exclude)
        current = Reminder(
            guild_id,
            set.id if set else current.channel_id,
            role.id if role else current.role_id,
            parse_minutes(before) if before else current.before_mins,
            pattern,
        )
        await self.s.db.set_reminder(
            guild_id, current.channel_id, current.role_id, current.before_mins, current.exclude
        )
        await interaction.response.send_message(_reminder_text(current), ephemeral=True)

    @channel.command(
        name="recap",
        description="Where to post how linked members placed after a contest; no options shows it",
    )
    @app_commands.describe(set="Channel to post recaps in", clear="Stop recaps")
    @app_commands.checks.has_permissions(manage_guild=True)
    async def recap_config(
        self,
        interaction: discord.Interaction,
        set: discord.TextChannel | None = None,
        clear: bool = False,
    ) -> None:
        guild_id = interaction.guild_id
        if clear:
            if set:
                raise BotError("Use clear on its own.")
            if not await self.s.db.clear_recap(guild_id):
                raise BotError("No recap channel set.")
            await interaction.response.send_message("Recaps stopped.", ephemeral=True)
            return
        if set is None:
            channel_id = await self.s.db.get_recap(guild_id)
            text = f"Recaps go to <#{channel_id}>." if channel_id else "No recap channel set."
            await interaction.response.send_message(text, ephemeral=True)
            return
        if not set.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {set.mention}.")
        await self.s.db.set_recap(guild_id, set.id)
        await interaction.response.send_message(f"Recaps go to {set.mention}.", ephemeral=True)

    @tasks.loop(seconds=60)
    async def remind_loop(self) -> None:
        try:
            await self._send_due_reminders()
        except Exception:
            log.exception("Reminder loop failed")
        try:
            await self._send_recaps()
        except Exception:
            log.exception("Recap loop failed")

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
                _not_excluded(s.contests.contests, reminder.exclude),
                reminder.before_mins,
                sent,
                now,
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

    async def _send_recaps(self) -> None:
        """Post how linked members placed in each contest that just ended, in the recap channel
        of every server that has one. At most once per server and contest."""
        s = self.s
        now = int(time.time())
        for guild_id, channel_id in await s.db.all_recaps():
            channel = self.bot.get_channel(channel_id)
            if channel is None:
                continue
            for contest in recaps_due(s.contests.contests, await s.db.recapped(guild_id), now):
                try:
                    entries = await self._entries(guild_id, contest.id)
                except Exception:
                    log.warning("Could not fetch recap for contest %s", contest.id, exc_info=True)
                    continue  # not marked: try again next tick
                if not await s.db.mark_recapped(guild_id, contest.id) or not entries:
                    continue
                try:
                    await channel.send(embed=recap_embed(contest, entries))
                except discord.HTTPException:
                    log.warning("Could not send recap to %s", channel_id, exc_info=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ContestsCog(bot))
