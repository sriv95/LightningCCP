import logging
import re

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.cogs.match import EXCLUDE_HELP, TAGS_HELP
from src.errors import BotError, report_error
from src.logic import MASHUP_MAX_PROBLEMS, MASHUP_MAX_TEAM_SIZE, MASHUP_MAX_TEAMS, tag_suggestions
from src.mashup import MashupView
from src.ui.embeds import mashup_embed

log = logging.getLogger(__name__)

Rating = app_commands.Range[int, 800, 3500]

BUTTONS = {
    "join": ("Join", discord.ButtonStyle.success),
    "start": ("Start", discord.ButtonStyle.primary),
    "leave": ("Leave", discord.ButtonStyle.secondary),
    "check": ("Check", discord.ButtonStyle.primary),
    "rematch": ("Rematch", discord.ButtonStyle.primary),
}
TEAMS = "Number of teams (default 10)"
TEAM_SIZE = "Players in each team, 1-5 (default 1: every player is a team of their own)"
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
}
VIRTUAL = {
    "contest_id": "Codeforces contest id to replay (it must have finished)",
    "penalty": OPTIONS["penalty"],
    "teams": TEAMS,
    "players": TEAM_SIZE,
}
ACTIONS = {
    "lobby": ("join", "start", "leave"),
    "active": ("check",),
    "finished": ("rematch",),
}


class MashupButton(
    discord.ui.DynamicItem[discord.ui.Button],
    template=r"mashup:(?P<action>join|start|leave|check|rematch):(?P<id>\d+)",
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
    host_rated = app_commands.Group(
        name="host-rated",
        description="Host a timed multi-problem contest that changes ratings",
        guild_only=True,
    )
    host_unrated = app_commands.Group(
        name="host-unrated",
        description="Host a timed multi-problem contest that leaves ratings alone",
        guild_only=True,
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

    @host_rated.command(
        name="contest", description="Open a rated timed contest lobby with a live scoreboard"
    )
    @app_commands.describe(**OPTIONS)
    async def rated_contest(
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
            True,
            tags=tags,
            exclude_tags=exclude_tags,
        )

    @host_rated.command(
        name="virtual", description="Open a rated lobby that replays a finished Codeforces contest"
    )
    @app_commands.describe(**VIRTUAL)
    async def rated_virtual(
        self,
        interaction: discord.Interaction,
        contest_id: int,
        penalty: app_commands.Range[int, 0, 120] = 0,
        teams: app_commands.Range[int, 2, MASHUP_MAX_TEAMS] = MASHUP_MAX_TEAMS,
        players: app_commands.Range[int, 1, MASHUP_MAX_TEAM_SIZE] = 1,
    ) -> None:
        await self._virtual(interaction, contest_id, penalty, teams, players, True)

    @host_unrated.command(
        name="contest", description="Open an unrated timed contest lobby with a live scoreboard"
    )
    @app_commands.describe(**OPTIONS)
    async def unrated_contest(
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
            False,
            tags=tags,
            exclude_tags=exclude_tags,
        )

    @host_unrated.command(
        name="virtual",
        description="Open an unrated lobby that replays a finished Codeforces contest",
    )
    @app_commands.describe(**VIRTUAL)
    async def unrated_virtual(
        self,
        interaction: discord.Interaction,
        contest_id: int,
        penalty: app_commands.Range[int, 0, 120] = 0,
        teams: app_commands.Range[int, 2, MASHUP_MAX_TEAMS] = MASHUP_MAX_TEAMS,
        players: app_commands.Range[int, 1, MASHUP_MAX_TEAM_SIZE] = 1,
    ) -> None:
        await self._virtual(interaction, contest_id, penalty, teams, players, False)

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

    @rated_contest.autocomplete("tags")
    @rated_contest.autocomplete("exclude_tags")
    @unrated_contest.autocomplete("tags")
    @unrated_contest.autocomplete("exclude_tags")
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
        )
        await self._post(interaction, view)

    async def _post(self, interaction: discord.Interaction, view: MashupView) -> None:
        message = await interaction.followup.send(
            embed=mashup_embed(view, self._by_key()), view=mashup_components(view), wait=True
        )
        await self.svc.set_message(view.mashup.id, interaction.channel_id, message.id)

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
        }
        view = await handlers[action](mashup_id, interaction.user.id)
        await interaction.edit_original_response(
            embed=mashup_embed(view, self._by_key()), view=mashup_components(view)
        )

    @tasks.loop(seconds=60)
    async def tick_loop(self) -> None:
        try:
            views = await self.svc.tick()
        except Exception:
            log.exception("Mashup tick failed")
            return
        for view in views:
            m = view.mashup
            if m.message_id is None:
                continue
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
