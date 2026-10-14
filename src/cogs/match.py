import contextlib
import logging
import re
import time
from typing import Literal

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.errors import BotError, report_error
from src.logic import MATCH_MAX_PLAYERS, daily_streak, tag_suggestions, utc_day
from src.matches import MatchView
from src.ui.embeds import (
    head_to_head_pages,
    history_pages,
    match_embed,
    number_pages,
    stats_embed,
)
from src.ui.paginator import send_pages

log = logging.getLogger(__name__)

Rating = app_commands.Range[int, 800, 3500]
Players = app_commands.Range[int, 2, MATCH_MAX_PLAYERS]
TAGS_HELP = "Comma-separated tags; the problem must have all of them"
EXCLUDE_HELP = "Comma-separated tags to avoid"

BUTTONS = {
    "join": ("Join", discord.ButtonStyle.success),
    "start": ("Start", discord.ButtonStyle.primary),
    "leave": ("Leave", discord.ButtonStyle.secondary),
    "check": ("Check", discord.ButtonStyle.success),
    "giveup": ("Give Up", discord.ButtonStyle.danger),
    "rematch": ("Rematch", discord.ButtonStyle.primary),
}
LOBBY_ACTIONS = ("join", "start", "leave")
ACTIVE_ACTIONS = ("check", "giveup")
FINISHED_DUEL_ACTIONS = ("rematch",)


class MatchButton(
    discord.ui.DynamicItem[discord.ui.Button],
    template=r"match:(?P<action>join|start|leave|check|giveup|rematch):(?P<id>\d+)",
):
    """Persistent button keyed by match id, so matches keep working across restarts."""

    def __init__(self, action: str, match_id: int):
        label, style = BUTTONS[action]
        super().__init__(
            discord.ui.Button(label=label, style=style, custom_id=f"match:{action}:{match_id}")
        )
        self.action = action
        self.match_id = match_id

    @classmethod
    async def from_custom_id(
        cls, interaction: discord.Interaction, item: discord.ui.Button, match: re.Match[str]
    ) -> "MatchButton":
        return cls(match["action"], int(match["id"]))

    async def callback(self, interaction: discord.Interaction) -> None:
        # discord.py never calls DynamicItem.on_error, so report here.
        try:
            cog = interaction.client.get_cog("Match")
            await cog.on_button(interaction, self.action, self.match_id)
        except Exception as exc:
            await report_error(interaction, exc)


def match_components(view: MatchView) -> discord.ui.View | None:
    actions = {"lobby": LOBBY_ACTIONS, "active": ACTIVE_ACTIONS}.get(view.match.status)
    if view.match.status == "finished" and view.match.mode == "duel":
        actions = FINISHED_DUEL_ACTIONS
    if actions is None:
        return None
    ui = discord.ui.View(timeout=None)
    for action in actions:
        ui.add_item(MatchButton(action, view.match.id))
    return ui


class MatchCog(commands.Cog, name="Match"):
    match = app_commands.Group(name="match", description="Play a Codeforces match", guild_only=True)

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @property
    def svc(self):
        return self.bot.services.matches

    async def cog_load(self) -> None:
        self.bot.add_dynamic_items(MatchButton)
        self.expire_loop.start()

    async def cog_unload(self) -> None:
        self.expire_loop.cancel()
        self.bot.remove_dynamic_items(MatchButton)

    async def _create(
        self,
        interaction: discord.Interaction,
        mode: str,
        max_players: int,
        invited_id: int | None = None,
        rating: int | None = None,
        tags: str | None = None,
        exclude_tags: str | None = None,
    ) -> None:
        await interaction.response.defer()
        # Already in a match: re-post it with fresh buttons instead of creating another.
        view = await self.svc.current(interaction.guild_id, interaction.user.id)
        if view is None:
            view = await self.svc.create(
                guild_id=interaction.guild_id,
                channel_id=interaction.channel_id,
                host_id=interaction.user.id,
                mode=mode,
                max_players=max_players,
                invited_id=invited_id,
                rating_arg=rating,
                tags=tags,
                exclude_tags=exclude_tags,
            )
        await self._post(interaction, view)

    async def _post(self, interaction: discord.Interaction, view: MatchView) -> None:
        """Send the match as a new message and remember it; retire the old one on a re-post."""
        ui = match_components(view)
        message = await interaction.followup.send(
            embed=match_embed(view), view=ui if ui is not None else discord.utils.MISSING, wait=True
        )
        old = view.match
        await self.svc.set_message(old.id, interaction.channel_id, message.id)
        if old.message_id is not None and old.message_id != message.id:
            # Re-post: retire the old message so its buttons do not go stale.
            with contextlib.suppress(discord.HTTPException):
                await (
                    self.bot.get_partial_messageable(old.channel_id)
                    .get_partial_message(old.message_id)
                    .edit(content=f"This match moved to {message.jump_url}", embed=None, view=None)
                )

    @match.command(name="solo", description="Practice alone for points")
    @app_commands.describe(
        rating="Problem rating (default: your match rating)",
        tags=TAGS_HELP,
        exclude_tags=EXCLUDE_HELP,
    )
    async def solo(
        self,
        interaction: discord.Interaction,
        rating: Rating | None = None,
        tags: str | None = None,
        exclude_tags: str | None = None,
    ) -> None:
        await self._create(interaction, "solo", 1, None, rating, tags, exclude_tags)

    @match.command(name="duel", description="Duel one other player for points")
    @app_commands.describe(
        opponent="Challenge this member (default: anyone can join)",
        rating="Problem rating (default: average match rating of the players)",
        tags=TAGS_HELP,
        exclude_tags=EXCLUDE_HELP,
    )
    async def duel(
        self,
        interaction: discord.Interaction,
        opponent: discord.Member | None = None,
        rating: Rating | None = None,
        tags: str | None = None,
        exclude_tags: str | None = None,
    ) -> None:
        if opponent is not None and opponent.bot:
            raise BotError("You cannot duel a bot.")
        await self._create(
            interaction, "duel", 2, opponent.id if opponent else None, rating, tags, exclude_tags
        )

    @match.command(name="custom", description="A match for several players")
    @app_commands.describe(
        players="Maximum number of players",
        rating="Problem rating (default: average match rating of the players)",
        tags=TAGS_HELP,
        exclude_tags=EXCLUDE_HELP,
    )
    async def custom(
        self,
        interaction: discord.Interaction,
        players: Players,
        rating: Rating | None = None,
        tags: str | None = None,
        exclude_tags: str | None = None,
    ) -> None:
        await self._create(interaction, "custom", players, None, rating, tags, exclude_tags)

    @solo.autocomplete("tags")
    @solo.autocomplete("exclude_tags")
    @duel.autocomplete("tags")
    @duel.autocomplete("exclude_tags")
    @custom.autocomplete("tags")
    @custom.autocomplete("exclude_tags")
    async def tag_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        return [
            app_commands.Choice(name=value, value=value)
            for value in tag_suggestions(current, self.bot.services.problems.tags)
        ]

    @match.command(name="history", description="Show finished matches")
    @app_commands.describe(member="Defaults to you; also shows your matches against them")
    async def history(
        self, interaction: discord.Interaction, member: discord.Member | None = None
    ) -> None:
        await interaction.response.defer(ephemeral=True)
        s = self.bot.services
        me, guild_id = interaction.user, interaction.guild_id
        by_key = {p.key: p for p in s.problems.problems}
        mine = await s.db.finished_matches(guild_id, me.id)
        if member is None or member.id == me.id:
            pages = history_pages(
                f"Match history - {me.display_name}",
                mine,
                me.id,
                by_key,
                await s.mashups.history(guild_id, me.id),
            )
            if not pages:
                raise BotError("You have no finished matches or mashups yet.")
        else:
            theirs = await s.db.finished_matches(guild_id, member.id)
            pages = head_to_head_pages(
                f"You vs {member.display_name}", mine, me.id, member.id, by_key
            ) + (
                history_pages(
                    f"Match history - {member.display_name}",
                    theirs,
                    member.id,
                    by_key,
                    await s.mashups.history(guild_id, member.id),
                )
                or [
                    discord.Embed(
                        title=f"Match history - {member.display_name}",
                        description="No finished matches or mashups yet.",
                    )
                ]
            )
        await send_pages(interaction, number_pages(pages))

    @app_commands.command(name="stats", description="Top match players in this server")
    @app_commands.guild_only()
    @app_commands.describe(sort="Rank by match rating (default) or by daily streak")
    async def stats(
        self,
        interaction: discord.Interaction,
        sort: Literal["rating", "streak"] = "rating",
    ) -> None:
        db = self.bot.services.db
        rows = await db.stats(interaction.guild_id)
        today = utc_day(int(time.time()))
        streaks = {
            uid: daily_streak(days, today)
            for uid, days in (await db.daily_days_by_user(interaction.guild_id)).items()
        }
        await interaction.response.send_message(
            embed=stats_embed(rows, interaction.user.id, streaks, sort == "streak"), ephemeral=True
        )

    async def on_button(self, interaction: discord.Interaction, action: str, match_id: int) -> None:
        await interaction.response.defer()
        svc = self.svc
        if action == "rematch":
            # A new lobby in a new message; the finished result stays as it is.
            await self._post(
                interaction,
                await svc.rematch(match_id, interaction.user.id, interaction.channel_id),
            )
            return
        handlers = {
            "join": svc.join,
            "start": svc.start,
            "leave": svc.leave,
            "check": svc.check,
            "giveup": svc.give_up,
        }
        view = await handlers[action](match_id, interaction.user.id)
        await interaction.edit_original_response(
            embed=match_embed(view), view=match_components(view)
        )
        if action == "check" and view.match.status == "active":
            me = next((p for p in view.players if p.user_id == interaction.user.id), None)
            if me is not None and me.solved_at is not None:
                note = "You solved it. Waiting for the other players."
            else:
                note = (
                    "No accepted submission found yet. "
                    "Solve the problem on Codeforces, then press Check again."
                )
            await interaction.followup.send(note, ephemeral=True)

    @tasks.loop(seconds=60)
    async def expire_loop(self) -> None:
        try:
            views = await self.svc.expire_due()
        except Exception:
            log.exception("Match expiry failed")
            return
        for view in views:
            try:
                await self._edit_stored_message(view)
            except Exception:
                log.exception("Could not update expired match %s", view.match.id)

    @expire_loop.before_loop
    async def _before_expire_loop(self) -> None:
        await self.bot.wait_until_ready()

    async def _edit_stored_message(self, view: MatchView) -> None:
        m = view.match
        if m.message_id is None:
            return
        try:
            await (
                self.bot.get_partial_messageable(m.channel_id)
                .get_partial_message(m.message_id)
                .edit(embed=match_embed(view), view=match_components(view))
            )
        except discord.HTTPException:
            log.warning("Could not edit match message %s", m.message_id, exc_info=True)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(MatchCog(bot))
