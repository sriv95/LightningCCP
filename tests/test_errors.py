import logging
from types import SimpleNamespace

import discord
from discord import app_commands

from src.errors import BotError, HandleNotFound, report_error


class FakeInteraction:
    def __init__(self, *, done=False, type=discord.InteractionType.application_command, fail=False):
        self.type = type
        self.calls: list[tuple] = []
        self._done = done
        self._fail = fail
        self.response = SimpleNamespace(is_done=lambda: self._done, send_message=self._send("send"))
        self.followup = SimpleNamespace(send=self._send("followup"))

    def _send(self, name):
        async def send(message, ephemeral=False):
            if self._fail:
                raise discord.HTTPException(SimpleNamespace(status=500, reason="x"), "boom")
            self.calls.append((name, message, ephemeral))

        return send

    async def delete_original_response(self):
        self.calls.append(("delete",))


async def test_bot_error_before_defer_is_ephemeral_send_message():
    i = FakeInteraction()
    await report_error(i, HandleNotFound())
    assert i.calls == [("send", "No Codeforces user matches that handle.", True)]


async def test_bot_error_wrapped_in_command_invoke_error():
    i = FakeInteraction()
    err = app_commands.CommandInvokeError(SimpleNamespace(name="x"), BotError("nope"))
    await report_error(i, err)
    assert i.calls == [("send", "nope", True)]


async def test_after_defer_on_slash_command_deletes_placeholder_then_followup():
    i = FakeInteraction(done=True)
    await report_error(i, BotError("nope"))
    assert i.calls == [("delete",), ("followup", "nope", True)]


async def test_after_defer_on_component_keeps_original_message():
    i = FakeInteraction(done=True, type=discord.InteractionType.component)
    await report_error(i, BotError("nope"))
    assert i.calls == [("followup", "nope", True)]


async def test_missing_permissions_message():
    i = FakeInteraction()
    await report_error(i, app_commands.MissingPermissions(["manage_guild"]))
    assert i.calls[0][1] == "You need the Manage Server permission to use this command."


async def test_unknown_exception_is_generic_and_logged(caplog):
    i = FakeInteraction()
    with caplog.at_level(logging.ERROR):
        await report_error(i, RuntimeError("secret"))
    assert i.calls == [("send", "Something went wrong. Try again later.", True)]
    assert "secret" not in i.calls[0][1]


async def test_http_exception_on_send_is_swallowed():
    await report_error(FakeInteraction(fail=True), BotError("nope"))
    await report_error(FakeInteraction(done=True, fail=True), BotError("nope"))
