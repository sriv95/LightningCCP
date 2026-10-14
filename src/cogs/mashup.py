import logging
import re
from typing import Literal

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.cogs.match import EXCLUDE_HELP, TAGS_HELP
from src.errors import BotError, report_error
from src.logic import (
    BINGO_MAX_SIZE,
    BINGO_MIN_SIZE,
    MASHUP_MAX_PROBLEMS,
    MASHUP_MAX_TEAM_SIZE,
    MASHUP_MAX_TEAMS,
    contest_suggestions,
    tag_suggestions,
)
from src.mashup import MashupView
from src.ui.embeds import (
    final_message,
    give_up_message,
    log_board_embed,
    log_message,
    mashup_embed,
    start_message,
)

log = logging.getLogger(__name__)

Rating = app_commands.Range[int, 800, 3500]

BUTTONS = {
    "join": ("Join", discord.ButtonStyle.success),
    "start": ("Start", discord.ButtonStyle.primary),
    "leave": ("Leave", discord.ButtonStyle.secondary),
    "check": ("Check", discord.ButtonStyle.primary),
    "giveup": ("Give Up", discord.ButtonStyle.danger),
    "rematch": ("Rematch", discord.ButtonStyle.primary),
}
TEAMS = "Number of teams (default 10)"
TEAM_SIZE = "Players in each team, 1-5 (default 1: every player is a team of their own)"
RATED = "True (default) changes match ratings; False leaves them alone (unrated)"
OPTIONS = {
    "min_rating": "Easiest problem rating (default 800)",
    "max_rating": "Hardest problem rating (default 1500)",
    "count": "Number of problems (default 5)",
    "hour": "Hours the contest lasts (default 2)",
    "minute": "Extra minutes (default 0)",
    "penalty": "Minutes added per wrong attempt before a solve (default 0)",
    "teams": TEAMS,
    "players": TEAM_SIZE,
    "tags": TAGS_HELP,
    "exclude_tags": EXCLUDE_HELP,
    "rated": RATED,
}
VIRTUAL = {
    "contest_id": "Codeforces contest id to replay (it must have finished)",
    "penalty": OPTIONS["penalty"],
    "teams": TEAMS,
    "players": TEAM_SIZE,
    "rated": RATED,
}
BINGO = {
    "size": f"Squares on each side of the board, {BINGO_MIN_SIZE}-{BINGO_MAX_SIZE} (default 5)",
    "mode": "classic: a claimed square stays; replace: a claimed square gets a +100 problem to steal",
    "win": "line (default): the first full line wins; continuous: play to the end, rank by lines",
    "min_rating": OPTIONS["min_rating"],
    "max_rating": OPTIONS["max_rating"],
    "hour": OPTIONS["hour"],
    "minute": OPTIONS["minute"],
    "teams": TEAMS,
    "players": TEAM_SIZE,
    "tags": TAGS_HELP,
    "exclude_tags": EXCLUDE_HELP,
    "rated": RATED,
}
ACTIONS = {
    "lobby": ("join", "start", "leave"),
    "active": ("check", "giveup"),
    "finished": ("rematch",),
}


class MashupButton(
    discord.ui.DynamicItem[discord.ui.Button],
    template=r"mashup:(?P<action>join|start|leave|check|giveup|rematch):(?P<id>\d+)",
):
    """Persistent button keyed by contest id, so contests keep working across restarts."""

    def __init__(self, action: str, mashup_id: int):
        label, style = BUTTONS[action]
        super().__init__(
            discord.ui.Button(label=label, style=style, custom_id=f"mashup:{action}:{mashup_id}")
        )
        self.action = action
        self.mashup_id = mashup_id

    @classmethod
    async def from_custom_id(
        cls, interaction: discord.Interaction, item: discord.ui.Button, match: re.Match[str]
    ) -> "MashupButton":
        return cls(match["action"], int(match["id"]))

    async def callback(self, interaction: discord.Interaction) -> None:
        # discord.py never calls DynamicItem.on_error, so report here.
        try:
            cog = interaction.client.get_cog("Mashup")
            await cog.on_button(interaction, self.action, self.mashup_id)
        except Exception as exc:
            await report_error(interaction, exc)


def mashup_components(view: MashupView) -> discord.ui.View | None:
    actions = ACTIONS.get(view.mashup.status)
    if actions is None:
        return None
    ui = discord.ui.View(timeout=None)
    for action in actions:
        ui.add_item(MashupButton(action, view.mashup.id))
    return ui


class TeamModal(discord.ui.Modal):
    """Asks which team a joining player wants; Join in a team contest opens it."""

    def __init__(self, cog: "MashupCog", mashup_id: int, teams: int):
        super().__init__(title="Join a team", timeout=300)
        self.cog = cog
        self.mashup_id = mashup_id
        self.team = discord.ui.TextInput(placeholder="1", min_length=1, max_length=2)
        self.add_item(discord.ui.Label(text=f"Team number (1-{teams})", component=self.team))

    async def on_submit(self, interaction: discord.Interaction) -> None:
        try:
            number = int(self.team.value.strip())
        except ValueError:
            raise BotError("The team must be a number.") from None
        await interaction.response.defer()
        view = await self.cog.svc.join(self.mashup_id, interaction.user.id, number)
        await interaction.edit_original_response(
            embed=mashup_embed(view, self.cog._by_key()), view=mashup_components(view)
        )

    async def on_error(self, interaction: discord.Interaction, error: Exception) -> None:
        await report_error(interaction, error)


class MashupCog(commands.Cog, name="Mashup"):
    host = app_commands.Group(
        name="host", description="Host a timed multi-problem contest", guild_only=True
    )

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @property
    def svc(self):
        return self.bot.services.mashups

    def _by_key(self):
        return {p.key: p for p in self.bot.services.problems.problems}

    async def cog_load(self) -> None:
        self.bot.add_dynamic_items(MashupButton)
        self.tick_loop.start()

    async def cog_unload(self) -> None:
        self.tick_loop.cancel()
        self.bot.remove_dynamic_items(MashupButton)

    @host.command(name="contest", description="Open a timed contest lobby with a live scoreboard")
    @app_commands.describe(**OPTIONS)
    async def contest(
        self,
        interaction: discord.Interaction,
        min_rating: Rating = 800,
        max_rating: Rating = 1500,
        count: app_commands.Range[int, 1, MASHUP_MAX_PROBLEMS] = 5,
        hour: app_commands.Range[int, 0, 24] = 2,
        minute: app_commands.Range[int, 0, 59] = 0,
        penalty: app_commands.Range[int, 0, 120] = 0,
        teams: app_commands.Range[int, 2, MASHUP_MAX_TEAMS] = MASHUP_MAX_TEAMS,
        players: app_commands.Range[int, 1, MASHUP_MAX_TEAM_SIZE] = 1,
        tags: str | None = None,
        exclude_tags: str | None = None,
        rated: bool = True,
    ) -> None:
        await self._open(
            interaction,
            min_rating,
            max_rating,
            count,
            hour,
            minute,
            penalty,
            teams,
            players,
            rated,
            tags=tags,
            exclude_tags=exclude_tags,
        )

    @host.command(
        name="virtual", description="Open a lobby that replays a finished Codeforces contest"
    )
    @app_commands.describe(**VIRTUAL)
    async def virtual(
        self,
        interaction: discord.Interaction,
        contest_id: int,
        penalty: app_commands.Range[int, 0, 120] = 0,
        teams: app_commands.Range[int, 2, MASHUP_MAX_TEAMS] = MASHUP_MAX_TEAMS,
        players: app_commands.Range[int, 1, MASHUP_MAX_TEAM_SIZE] = 1,
        rated: bool = True,
    ) -> None:
        await self._virtual(interaction, contest_id, penalty, teams, players, rated)

    @host.command(name="bingo", description="Open a bingo lobby: teams race for the squares")
    @app_commands.describe(**BINGO)
    async def bingo(
        self,
        interaction: discord.Interaction,
        size: app_commands.Range[int, BINGO_MIN_SIZE, BINGO_MAX_SIZE] = 5,
        mode: Literal["classic", "replace"] = "classic",
        win: Literal["line", "continuous"] = "line",
        min_rating: Rating = 800,
        max_rating: Rating = 1500,
        hour: app_commands.Range[int, 0, 24] = 2,
        minute: app_commands.Range[int, 0, 59] = 0,
        teams: app_commands.Range[int, 2, MASHUP_MAX_TEAMS] = MASHUP_MAX_TEAMS,
        players: app_commands.Range[int, 1, MASHUP_MAX_TEAM_SIZE] = 1,
        tags: str | None = None,
        exclude_tags: str | None = None,
        rated: bool = True,
    ) -> None:
        await self._open(
            interaction,
            min_rating,
            max_rating,
            size * size,
            hour,
            minute,
            0,
            teams,
            players,
            rated,
            tags=tags,
            exclude_tags=exclude_tags,
            mode=mode,
            size=size,
            win=win,
        )

    @virtual.autocomplete("contest_id")
    async def contest_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[int]]:
        s = self.bot.services
        known = {p.contest_id for p in s.problems.problems if p.rating}
        return [
            app_commands.Choice(name=label, value=contest_id)
            for label, contest_id in contest_suggestions(s.contests.contests, known, current)
        ]

    async def _virtual(
        self,
        interaction: discord.Interaction,
        contest_id: int,
        penalty: int,
        teams: int,
        team_size: int,
        rated: bool,
    ) -> None:
        contest = next((c for c in self.bot.services.contests.contests if c.id == contest_id), None)
        if contest is None:
            raise BotError("I do not know that contest id. See /contests recent.")
        if contest.phase != "FINISHED":
            raise BotError("That contest has not finished yet.")
        await self._open(
            interaction,
            800,
            3500,
            1,
            contest.duration // 3600,
            contest.duration % 3600 // 60,
            penalty,
            teams,
            team_size,
            rated,
            contest_id=contest_id,
        )

    @contest.autocomplete("tags")
    @contest.autocomplete("exclude_tags")
    @bingo.autocomplete("tags")
    @bingo.autocomplete("exclude_tags")
    async def tag_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        return [
            app_commands.Choice(name=value, value=value)
            for value in tag_suggestions(current, self.bot.services.problems.tags)
        ]

    async def _open(
        self,
        interaction: discord.Interaction,
        min_rating: int,
        max_rating: int,
        count: int,
        hour: int,
        minute: int,
        penalty: int,
        teams: int,
        team_size: int,
        rated: bool,
        tags: str | None = None,
        exclude_tags: str | None = None,
        contest_id: int | None = None,
        mode: str = "contest",
        size: int = 0,
        win: str = "line",
    ) -> None:
        await interaction.response.defer()
        view = await self.svc.create(
            guild_id=interaction.guild_id,
            channel_id=interaction.channel_id,
            host_id=interaction.user.id,
            min_rating=min_rating,
            max_rating=max_rating,
            count=count,
            duration_seconds=hour * 3600 + minute * 60,
            penalty_minutes=penalty,
            teams=teams,
            team_size=team_size,
            rated=rated,
            tags=tags,
            exclude_tags=exclude_tags,
            contest_id=contest_id,
            mode=mode,
            size=size,
            win=win,
        )
        await self._post(interaction, view)

    async def _post(self, interaction: discord.Interaction, view: MashupView) -> None:
        message = await interaction.followup.send(
            embed=mashup_embed(view, self._by_key()), view=mashup_components(view), wait=True
        )
        await self.svc.set_message(view.mashup.id, interaction.channel_id, message.id)
        await self._open_thread(interaction, view, message)

    async def _open_thread(
        self, interaction: discord.Interaction, view: MashupView, message: discord.Message
    ) -> None:
        """The message gets a thread where every solve is logged."""
        m = view.mashup
        kind = "Bingo" if m.mode != "contest" else "Virtual contest" if m.contest_id else "Contest"
        try:
            thread = await interaction.channel.get_partial_message(message.id).create_thread(
                name=f"{kind} {view.mashup.id} solve log"
            )
        except (discord.HTTPException, AttributeError, ValueError):
            log.warning("Could not create the thread of mashup %s", view.mashup.id, exc_info=True)
            await interaction.followup.send(
                "I could not open the solve log thread here. "
                "Give me the Create Public Threads and Send Messages in Threads permissions.",
                ephemeral=True,
            )
            return
        await self.svc.set_thread(view.mashup.id, thread.id)

    async def refresh(self, view: MashupView) -> None:
        """Brings the stored message and the thread up to date."""
        await self.edit_message(view)
        await self._log(view)

    async def log_started(self, view: MashupView) -> None:
        """Pings every player in the thread when the host starts the contest."""
        m = view.mashup
        if m.thread_id is None:
            return
        try:
            await self.bot.get_partial_messageable(
                m.thread_id, guild_id=m.guild_id, type=discord.ChannelType.public_thread
            ).send(start_message(view), allowed_mentions=discord.AllowedMentions(users=True))
        except Exception:  # a side channel, like the solve log
            log.warning("Could not log the start of mashup %s", m.id, exc_info=True)

    async def log_gave_up(self, view: MashupView, user_id: int) -> None:
        """Tells the thread that a player left the running contest."""
        m = view.mashup
        if m.thread_id is None:
            return
        try:
            await self.bot.get_partial_messageable(
                m.thread_id, guild_id=m.guild_id, type=discord.ChannelType.public_thread
            ).send(give_up_message(view, user_id), allowed_mentions=discord.AllowedMentions.none())
        except Exception:  # a side channel, like the solve log
            log.warning("Could not log a give-up of mashup %s", m.id, exc_info=True)

    async def _log(self, view: MashupView) -> None:
        """Posts the solves that the thread does not have yet."""
        m = view.mashup
        if m.thread_id is None:
            return
        try:
            thread = self.bot.get_partial_messageable(
                m.thread_id, guild_id=m.guild_id, type=discord.ChannelType.public_thread
            )
            entries = await self.svc.claims_to_log(view)
            final = await self.svc.final_due(view)
            if not entries and not final:
                return
            old = (await self.bot.services.db.get_mashup(m.id)).log_board_id
            by_key = self._by_key()
            last = None
            for i, entry in enumerate(entries):
                # Only the newest message shows the board; the closing message is the newest.
                show = i == len(entries) - 1 and not final
                last = await thread.send(
                    log_message(view, entry, by_key),
                    embed=log_board_embed(view) if show else discord.utils.MISSING,
                    allowed_mentions=discord.AllowedMentions.none(),
                )
            if final:
                last = await thread.send(
                    final_message(view),
                    embed=log_board_embed(view)
                    if m.status == "finished"
                    else discord.utils.MISSING,
                    allowed_mentions=discord.AllowedMentions.none(),
                )
            await self.svc.set_log_board(m.id, last.id)
            if old is not None:
                try:
                    await thread.get_partial_message(old).edit(embeds=[])
                except discord.HTTPException:
                    log.info("Could not clear the board of message %s", old, exc_info=True)
        except Exception:  # the log is a side channel: it must not break the button or the loop
            log.warning("Could not log to the thread of mashup %s", m.id, exc_info=True)

    async def on_button(
        self, interaction: discord.Interaction, action: str, mashup_id: int
    ) -> None:
        if action == "rematch":
            # A new lobby in a new message; the final standings stay as they are.
            await interaction.response.defer()
            await self._post(
                interaction,
                await self.svc.rematch(mashup_id, interaction.user.id, interaction.channel_id),
            )
            return
        if action == "join":
            mashup = await self.bot.services.db.get_mashup(mashup_id)
            if mashup is not None and mashup.status == "lobby" and mashup.team_size != 1:
                # A team contest asks for the team first; the modal then joins.
                await interaction.response.send_modal(TeamModal(self, mashup_id, mashup.teams))
                return
        await interaction.response.defer()
        handlers = {
            "join": self.svc.join,
            "start": self.svc.start,
            "leave": self.svc.leave,
            "check": self.svc.check,
            "giveup": self.svc.give_up,
        }
        view = await handlers[action](mashup_id, interaction.user.id)
        await interaction.edit_original_response(
            embed=mashup_embed(view, self._by_key()), view=mashup_components(view)
        )
        await self._log(view)
        if action == "giveup":
            await self.log_gave_up(view, interaction.user.id)
        elif action == "start":
            await self.log_started(view)

    @tasks.loop(seconds=60)
    async def tick_loop(self) -> None:
        try:
            views = await self.svc.tick()
        except Exception:
            log.exception("Mashup tick failed")
            return
        self.tick_loop.change_interval(seconds=self.svc.interval)
        for view in views:
            await self.edit_message(view)
            await self._log(view)

    async def edit_message(self, view: MashupView) -> None:
        """Brings the stored contest message up to date."""
        m = view.mashup
        if m.message_id is not None:
            try:
                await (
                    self.bot.get_partial_messageable(m.channel_id)
                    .get_partial_message(m.message_id)
                    .edit(embed=mashup_embed(view, self._by_key()), view=mashup_components(view))
                )
            except discord.HTTPException:
                log.warning("Could not edit mashup message %s", m.message_id, exc_info=True)

    @tick_loop.before_loop
    async def _before_tick_loop(self) -> None:
        await self.bot.wait_until_ready()


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(MashupCog(bot))
