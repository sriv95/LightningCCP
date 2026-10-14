from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

from src.cogs.handles import VerifyButton
from src.cogs.mashup import MashupButton
from src.cogs.match import MatchButton, MatchCog, match_components
from src.errors import BotError
from src.matches import MatchView
from tests.test_match_embeds import player, row


def button_interaction(cog):
    return SimpleNamespace(
        type=discord.InteractionType.component,
        client=SimpleNamespace(get_cog=lambda name: cog),
        response=SimpleNamespace(is_done=lambda: True),
        followup=SimpleNamespace(send=AsyncMock()),
        delete_original_response=AsyncMock(),
    )


class Boom:
    def __init__(self, exc):
        self.exc = exc

    async def on_button(self, interaction, action, match_id):
        raise self.exc

    async def verify(self, interaction):
        raise self.exc


@pytest.mark.parametrize(
    "make",
    [lambda: MatchButton("start", 1), lambda: MashupButton("join", 1), lambda: VerifyButton(5)],
)
@pytest.mark.parametrize(
    ("exc", "message"),
    [(BotError("boom"), "boom"), (RuntimeError("x"), "Something went wrong. Try again later.")],
)
async def test_button_callback_reports_errors_privately(make, exc, message):
    i = button_interaction(Boom(exc))
    await make().callback(i)
    i.followup.send.assert_awaited_once_with(message, ephemeral=True)
    i.delete_original_response.assert_not_awaited()


def make_cog(svc, bot_extra=None):
    bot = SimpleNamespace(services=SimpleNamespace(matches=svc), **(bot_extra or {}))
    return MatchCog(bot)


class FakeSvc:
    def __init__(self, view):
        self.view = view
        self.set_message = AsyncMock()
        self.check = AsyncMock(return_value=view)
        self.join = self.start = self.leave = self.give_up = AsyncMock()

    async def current(self, guild_id, user_id):
        return self.view


async def test_repost_retires_the_old_message():
    old = MagicMock()
    old.edit = AsyncMock()
    messageable = MagicMock()
    messageable.get_partial_message.return_value = old
    bot_extra = {"get_partial_messageable": MagicMock(return_value=messageable)}
    view = MatchView(row(message_id=111, channel_id=5), [player(10)])
    svc = FakeSvc(view)
    cog = make_cog(svc, bot_extra)
    new = SimpleNamespace(id=222, jump_url="https://discord.com/channels/1/6/222")
    interaction = SimpleNamespace(
        guild_id=1,
        channel_id=6,
        user=SimpleNamespace(id=10),
        response=SimpleNamespace(defer=AsyncMock()),
        followup=SimpleNamespace(send=AsyncMock(return_value=new)),
    )
    await cog._create(interaction, "duel", 2)
    svc.set_message.assert_awaited_once_with(1, 6, 222)
    cog.bot.get_partial_messageable.assert_called_once_with(5)
    messageable.get_partial_message.assert_called_once_with(111)
    old.edit.assert_awaited_once_with(
        content=f"This match moved to {new.jump_url}", embed=None, view=None
    )


@pytest.mark.parametrize(
    ("solved_at", "expected"),
    [
        (2100, "You solved it. Waiting for the other players."),
        (None, "No accepted submission found yet."),
    ],
)
async def test_check_without_finish_gives_presser_feedback(solved_at, expected):
    view = MatchView(
        row(status="active", problem_key="1700A", rating=1000, mode="custom"),
        [player(10, solved_at=solved_at), player(11)],
    )
    cog = make_cog(FakeSvc(view))
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=10),
        response=SimpleNamespace(defer=AsyncMock()),
        edit_original_response=AsyncMock(),
        followup=SimpleNamespace(send=AsyncMock()),
    )
    await cog.on_button(interaction, "check", 1)
    (msg,), kwargs = interaction.followup.send.await_args
    assert msg.startswith(expected) and kwargs == {"ephemeral": True}


def test_rematch_button_only_on_a_finished_duel():
    def actions(**kw):
        ui = match_components(MatchView(row(**kw), [player(10), player(11)]))
        return None if ui is None else [item.action for item in ui.children]

    assert actions(status="finished", mode="duel") == ["rematch"]
    assert actions(status="finished", mode="solo") is None
    assert actions(status="finished", mode="custom") is None
    assert actions(status="cancelled", mode="duel") is None


async def test_rematch_button_posts_a_new_lobby():
    lobby = MatchView(row(id=9, message_id=None, channel_id=6), [player(10)])
    svc = FakeSvc(lobby)
    svc.rematch = AsyncMock(return_value=lobby)
    cog = make_cog(svc)
    new = SimpleNamespace(id=222, jump_url="x")
    interaction = SimpleNamespace(
        guild_id=1,
        channel_id=6,
        user=SimpleNamespace(id=10),
        response=SimpleNamespace(defer=AsyncMock()),
        edit_original_response=AsyncMock(),
        followup=SimpleNamespace(send=AsyncMock(return_value=new)),
    )
    await cog.on_button(interaction, "rematch", 3)
    svc.rematch.assert_awaited_once_with(3, 10, 6)
    svc.set_message.assert_awaited_once_with(9, 6, 222)
    interaction.edit_original_response.assert_not_awaited()  # the finished result stays as it is
    assert MatchButton.__discord_ui_compiled_template__.fullmatch("match:rematch:3")
