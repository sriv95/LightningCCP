import logging
import re

import discord
from discord import app_commands
from discord.ext import commands, tasks

from src.errors import report_error
from src.logic import MASHUP_MAX_PROBLEMS
from src.mashup import MashupView
from src.ui.embeds import mashup_embed

log = logging.getLogger(__name__)

Rating = app_commands.Range[int, 800, 3500]

BUTTONS = {
    "join": ("Join", discord.ButtonStyle.success),
    "start": ("Start", discord.ButtonStyle.primary),
    "leave": ("Leave", discord.ButtonStyle.secondary),
    "check": ("Check", discord.ButtonStyle.primary),
}
ACTIONS = {"lobby": ("join", "start", "leave"), "active": ("check",)}


class MashupButton(
    discord.ui.DynamicItem[discord.ui.Button],
    template=r"mashup:(?P<action>join|start|leave|check):(?P<id>\d+)",
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


class MashupCog(commands.Cog, name="Mashup"):
    mashup = app_commands.Group(
        name="mashup", description="Timed multi-problem contests", guild_only=True
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

    @mashup.command(name="contest", description="Open a timed contest lobby with a live scoreboard")
    @app_commands.describe(
        min_rating="Easiest problem rating (default 800)",
        max_rating="Hardest problem rating (default 1500)",
        count="Number of problems (default 5)",
        hour="Hours the contest lasts (default 2)",
        minute="Extra minutes (default 0)",
        penalty="Minutes added per wrong attempt before a solve (default 0)",
    )
    async def contest(
        self,
        interaction: discord.Interaction,
        min_rating: Rating = 800,
        max_rating: Rating = 1500,
        count: app_commands.Range[int, 1, MASHUP_MAX_PROBLEMS] = 5,
        hour: app_commands.Range[int, 0, 24] = 2,
        minute: app_commands.Range[int, 0, 59] = 0,
        penalty: app_commands.Range[int, 0, 120] = 0,
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
        )
        message = await interaction.followup.send(
            embed=mashup_embed(view, self._by_key()), view=mashup_components(view), wait=True
        )
        await self.svc.set_message(view.mashup.id, interaction.channel_id, message.id)

    async def on_button(
        self, interaction: discord.Interaction, action: str, mashup_id: int
    ) -> None:
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
