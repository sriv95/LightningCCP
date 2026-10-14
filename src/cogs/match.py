import contextlib
import logging
import re

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.errors import BotError, report_error
from src.logic import MATCH_MAX_PLAYERS
from src.matches import MatchView
from src.ui.embeds import leaderboard_embed, match_embed

log = logging.getLogger(__name__)

Rating = app_commands.Range[int, 800, 3500]
Players = app_commands.Range[int, 2, MATCH_MAX_PLAYERS]

BUTTONS = {
    "join": ("Join", discord.ButtonStyle.success),
    "start": ("Start", discord.ButtonStyle.primary),
    "leave": ("Leave", discord.ButtonStyle.secondary),
    "check": ("Check", discord.ButtonStyle.success),
    "giveup": ("Give Up", discord.ButtonStyle.danger),
}
LOBBY_ACTIONS = ("join", "start", "leave")
ACTIVE_ACTIONS = ("check", "giveup")


class MatchButton(
    discord.ui.DynamicItem[discord.ui.Button],
    template=r"match:(?P<action>join|start|leave|check|giveup):(?P<id>\d+)",
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
            )
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
    @app_commands.describe(rating="Problem rating (default: your rating)")
    async def solo(self, interaction: discord.Interaction, rating: Rating | None = None) -> None:
        await self._create(interaction, "solo", 1, None, rating)

    @match.command(name="duel", description="Duel one other player for points")
    @app_commands.describe(
        opponent="Challenge this member (default: anyone can join)",
        rating="Problem rating (default: average of the players)",
    )
    async def duel(
        self,
        interaction: discord.Interaction,
        opponent: discord.Member | None = None,
        rating: Rating | None = None,
    ) -> None:
        if opponent is not None and opponent.bot:
            raise BotError("You cannot duel a bot.")
        await self._create(interaction, "duel", 2, opponent.id if opponent else None, rating)

    @match.command(name="custom", description="A match for several players")
    @app_commands.describe(
        players="Maximum number of players",
        rating="Problem rating (default: average of the players)",
    )
    async def custom(
        self, interaction: discord.Interaction, players: Players, rating: Rating | None = None
    ) -> None:
        await self._create(interaction, "custom", players, None, rating)

    @app_commands.command(name="stats", description="Top match players in this server")
    @app_commands.guild_only()
    async def leaderboard(self, interaction: discord.Interaction) -> None:
        rows = await self.bot.services.db.leaderboard(interaction.guild_id)
        await interaction.response.send_message(
            embed=leaderboard_embed(rows, interaction.user.id), ephemeral=True
        )

    async def on_button(self, interaction: discord.Interaction, action: str, match_id: int) -> None:
        await interaction.response.defer()
        svc = self.svc
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
