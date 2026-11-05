import io
import logging
import re
import time

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.db.store import Reminder
from src.errors import BotError, CacheNotReady, report_error
from src.logic import (
    contest_ics,
    new_contests,
    parse_minutes,
    recaps_due,
    reminders_due,
    select_contests,
    week_start,
    weekly_board,
)
from src.ui.embeds import (
    contest_pages,
    esc,
    new_contest_embed,
    ranklist_pages,
    recap_embed,
    weekly_embed,
)
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


def _not_excluded(contests, pattern: str, keep=()):
    """Drop contests whose name matches the regex, except the ids in `keep`."""
    if not pattern:
        return contests
    rx = re.compile(pattern, re.IGNORECASE)
    return [c for c in contests if c.id in keep or not rx.search(c.name)]


def _reminder_text(r: Reminder) -> str:
    where = f"<#{r.channel_id}>" if r.channel_id else "no channel (use set)"
    role = f", pinging <@&{r.role_id}>" if r.role_id else ""
    skip = f"\nExcluding names matching `{r.exclude}`." if r.exclude else ""
    mins = ", ".join(str(m) for m in r.before_mins)
    return f"Reminders: {where}{role}, {mins} minutes before start.{skip}"


RECAP_MAX_BACKOFF = 1800  # seconds between tries of a failing recap, at most
MAX_PINGED_MEMBERS = 50  # a message holds 2000 characters, a mention takes about 22


BUTTONS = {
    "cal": ("Add to calendar", "\N{SPIRAL CALENDAR PAD}"),
    "notify": ("Notify me", "\N{BELL}"),
}


class ContestButton(
    discord.ui.DynamicItem[discord.ui.Button], template=r"(?P<action>cal|notify):(?P<contest>\d+)"
):
    """Persistent Add to calendar and Notify me buttons of a new-contest post."""

    def __init__(self, action: str, contest_id: int):
        label, emoji = BUTTONS[action]
        super().__init__(
            discord.ui.Button(
                label=label,
                emoji=emoji,
                style=discord.ButtonStyle.primary
                if action == "cal"
                else discord.ButtonStyle.secondary,
                custom_id=f"{action}:{contest_id}",
            )
        )
        self.action = action
        self.contest_id = contest_id

    @classmethod
    async def from_custom_id(
        cls, interaction: discord.Interaction, item: discord.ui.Button, match: re.Match[str]
    ) -> "ContestButton":
        return cls(match["action"], int(match["contest"]))

    async def callback(self, interaction: discord.Interaction) -> None:
        # discord.py never calls DynamicItem.on_error, so report here.
        try:
            cog = interaction.client.get_cog("Contests")
            if self.action == "cal":
                await cog.calendar(interaction, self.contest_id)
            else:
                await cog.notify(interaction, self.contest_id)
        except Exception as exc:
            await report_error(interaction, exc)


class ContestsCog(commands.Cog, name="Contests"):
    contests = app_commands.Group(
        name="contests", description="List Codeforces contests", guild_only=True
    )
    channel = app_commands.Group(
        name="channel",
        description="Channels for contest reminders, recaps and the weekly leaderboard",
        guild_only=True,
        default_permissions=discord.Permissions(manage_guild=True),
    )

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        self._warned_channels: set[tuple[int, int]] = set()
        # (guild, contest) -> (next try, delay) of a recap that failed; in memory only
        self._recap_retry: dict[tuple[int, int], tuple[int, int]] = {}

    @property
    def s(self):
        return self.bot.services

    async def cog_load(self) -> None:
        self.bot.add_dynamic_items(ContestButton)
        self.remind_loop.start()
        self.new_contest_loop.start()

    async def cog_unload(self) -> None:
        self.bot.remove_dynamic_items(ContestButton)
        self.remind_loop.cancel()
        self.new_contest_loop.cancel()

    def _upcoming(self, contest_id: int):
        contest = next((c for c in self.s.contests.contests if c.id == contest_id), None)
        if contest is None or contest.phase != "BEFORE" or contest.start_time is None:
            raise BotError("That contest is not upcoming any more.")
        return contest

    async def calendar(self, interaction: discord.Interaction, contest_id: int) -> None:
        """Send the .ics file."""
        contest = self._upcoming(contest_id)
        ics = contest_ics(contest, int(time.time()))
        await interaction.response.send_message(
            "Open the file to add the contest to your calendar.",
            file=discord.File(io.BytesIO(ics.encode()), filename=f"contest-{contest_id}.ics"),
            ephemeral=True,
        )

    async def notify(self, interaction: discord.Interaction, contest_id: int) -> None:
        """Ping the member in the server's reminder channel when the contest reminder is posted."""
        self._upcoming(contest_id)
        reminder = await self.s.db.get_reminder(interaction.guild_id)
        if not reminder or not reminder.channel_id:
            raise BotError(
                "This server has no reminder channel. An admin can set one with /channel reminder."
            )
        added = await self.s.db.add_contest_remind(
            interaction.guild_id, contest_id, interaction.user.id
        )
        text = (
            f"You will be pinged in <#{reminder.channel_id}> when the reminder is posted."
            if added
            else "You are already on the list."
        )
        await interaction.response.send_message(text, ephemeral=True)

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
        clear_role="Stop pinging the role",
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
        clear_role: bool = False,
    ) -> None:
        guild_id = interaction.guild_id
        current = await self.s.db.get_reminder(guild_id) or Reminder(guild_id, None, None, (60, 10))
        if clear:
            if set or role or before or exclude or clear_role:
                raise BotError("Use clear on its own.")
            if not await self.s.db.clear_reminder(guild_id):
                raise BotError("No reminder channel set.")
            await interaction.response.send_message("Reminders stopped.", ephemeral=True)
            return
        if set is None and role is None and before is None and exclude is None and not clear_role:
            await interaction.response.send_message(_reminder_text(current), ephemeral=True)
            return
        if set and not set.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {set.mention}.")
        if clear_role and role:
            raise BotError("Give a role or clear_role, not both.")
        pattern = current.exclude
        if exclude is not None:
            pattern = "" if exclude.strip().lower() == "clear" else _check_regex(exclude)
        current = Reminder(
            guild_id,
            set.id if set else current.channel_id,
            None if clear_role else role.id if role else current.role_id,
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

    @channel.command(
        name="leaderboard",
        description="Where to post the leaderboard every Sunday 00:00 UTC; no options shows it",
    )
    @app_commands.describe(set="Channel to post the weekly leaderboard in", clear="Stop it")
    @app_commands.checks.has_permissions(manage_guild=True)
    async def leaderboard_config(
        self,
        interaction: discord.Interaction,
        set: discord.TextChannel | None = None,
        clear: bool = False,
    ) -> None:
        guild_id = interaction.guild_id
        if clear:
            if set:
                raise BotError("Use clear on its own.")
            if not await self.s.db.clear_weekly(guild_id):
                raise BotError("No weekly leaderboard channel set.")
            await interaction.response.send_message("Weekly leaderboard stopped.", ephemeral=True)
            return
        if set is None:
            channel_id = await self.s.db.get_weekly(guild_id)
            text = (
                f"The leaderboard goes to <#{channel_id}> every Sunday 00:00 UTC."
                if channel_id
                else "No weekly leaderboard channel set."
            )
            await interaction.response.send_message(text, ephemeral=True)
            return
        if not set.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {set.mention}.")
        await self.s.db.set_weekly(guild_id, set.id, week_start(int(time.time())))
        await interaction.response.send_message(
            f"The leaderboard goes to {set.mention} every Sunday 00:00 UTC, starting next Sunday.",
            ephemeral=True,
        )

    @channel.command(
        name="new-contest",
        description="Where to announce new contests with an Add to calendar button; no options shows it",
    )
    @app_commands.describe(set="Channel to announce new contests in", clear="Stop announcements")
    @app_commands.checks.has_permissions(manage_guild=True)
    async def new_contest_config(
        self,
        interaction: discord.Interaction,
        set: discord.TextChannel | None = None,
        clear: bool = False,
    ) -> None:
        guild_id = interaction.guild_id
        if clear:
            if set:
                raise BotError("Use clear on its own.")
            if not await self.s.db.clear_newcontest(guild_id):
                raise BotError("No new-contest channel set.")
            await interaction.response.send_message("New-contest posts stopped.", ephemeral=True)
            return
        if set is None:
            channel_id = await self.s.db.get_newcontest(guild_id)
            text = (
                f"New contests are posted in <#{channel_id}>."
                if channel_id
                else "No new-contest channel set."
            )
            await interaction.response.send_message(text, ephemeral=True)
            return
        if not set.permissions_for(interaction.guild.me).send_messages:
            raise BotError(f"I cannot send messages in {set.mention}.")
        # Contests already listed are not "new": post only the ones that appear from now on.
        known = [c.id for c in self.s.contests.contests]
        await self.s.db.set_newcontest(guild_id, set.id, known)
        await interaction.response.send_message(
            f"New contests are posted in {set.mention}, checked every 30 minutes.", ephemeral=True
        )

    @tasks.loop(minutes=30)
    async def new_contest_loop(self) -> None:
        try:
            await self._send_new_contests()
        except Exception:
            log.exception("New contest loop failed")

    @new_contest_loop.before_loop
    async def _before_new_contest_loop(self) -> None:
        await self.bot.wait_until_ready()

    async def _send_new_contests(self) -> None:
        """Announce each upcoming contest once per server in its new-contest channel. The
        contest cache refreshes at least every 30 minutes, so this check reads the cache and sees new ones."""
        s = self.s
        now = int(time.time())
        for guild_id, channel_id in await s.db.all_newcontests():
            channel = self.bot.get_channel(channel_id)
            if channel is None:
                continue
            for contest in new_contests(s.contests.contests, await s.db.posted(guild_id), now):
                if not await s.db.mark_posted(guild_id, contest.id):
                    continue
                view = discord.ui.View(timeout=None)
                view.add_item(ContestButton("cal", contest.id))
                view.add_item(ContestButton("notify", contest.id))
                try:
                    await channel.send(embed=new_contest_embed(contest), view=view)
                except discord.HTTPException:
                    log.warning("Could not post new contest to %s", channel_id, exc_info=True)

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
        try:
            await self._send_weekly()
        except Exception:
            log.exception("Weekly leaderboard loop failed")

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
            asked = await s.db.contest_reminders(reminder.guild_id)
            offsets: dict[int, list[int]] = {}
            contests = {}
            for contest, before in reminders_due(
                _not_excluded(s.contests.contests, reminder.exclude, asked),
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
                members = asked.get(contest_id, [])[:MAX_PINGED_MEMBERS]
                ping += "".join(f"<@{m}> " for m in members)
                try:
                    await channel.send(
                        f"{ping}**[{esc(contest.name)}]({contest.url})** starts "
                        f"<t:{contest.start_time}:R>.",
                        allowed_mentions=discord.AllowedMentions(roles=True, users=True),
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
                key = (guild_id, contest.id)
                if self._recap_retry.get(key, (0, 0))[0] > now:
                    continue  # failed lately: wait out the back-off
                try:
                    entries = await self._entries(guild_id, contest.id)
                except Exception:
                    delay = min(RECAP_MAX_BACKOFF, self._recap_retry.get(key, (0, 30))[1] * 2)
                    self._recap_retry[key] = (now + delay, delay)  # 1 min, 2 min ... 30 min
                    log.warning("Could not fetch recap for contest %s", contest.id, exc_info=True)
                    continue  # not marked: try again after the back-off
                self._recap_retry.pop(key, None)
                if not await s.db.mark_recapped(guild_id, contest.id) or not entries:
                    continue
                try:
                    await channel.send(embed=recap_embed(contest, entries))
                except discord.HTTPException:
                    log.warning("Could not send recap to %s", channel_id, exc_info=True)

    async def _send_weekly(self) -> None:
        """Post the leaderboard of the week that just ended in every server that set a channel.
        At most once per server and week."""
        s = self.s
        week = week_start(int(time.time()))
        for guild_id, channel_id, last_sent in await s.db.all_weeklies():
            channel = self.bot.get_channel(channel_id)
            if channel is None or last_sent >= week:
                continue
            if not await s.db.mark_weekly(guild_id, week):
                continue
            rows = weekly_board(await s.db.rating_events(guild_id), week)
            if not rows:
                continue
            try:
                await channel.send(embed=weekly_embed(rows))
            except discord.HTTPException:
                log.warning("Could not send leaderboard to %s", channel_id, exc_info=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(ContestsCog(bot))
