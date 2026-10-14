from types import SimpleNamespace

import discord

from src.ui.paginator import PaginatorView, send_pages


async def test_buttons_are_disabled_when_the_view_times_out():
    edits = []

    async def edit(**kw):
        edits.append(kw)

    view = PaginatorView([discord.Embed(), discord.Embed()], 1)
    view.message = SimpleNamespace(edit=edit)
    await view.on_timeout()
    assert all(item.disabled for item in view.children)
    assert edits == [{"view": view}]


async def test_timeout_without_a_message_or_after_it_vanished_is_quiet():
    view = PaginatorView([discord.Embed(), discord.Embed()], 1)
    await view.on_timeout()  # no message yet

    async def gone(**kw):
        raise discord.NotFound(SimpleNamespace(status=404, reason="x"), "unknown message")

    view.message = SimpleNamespace(edit=gone)
    await view.on_timeout()


async def test_send_pages_keeps_the_message_for_the_timeout():
    sent = []

    async def send(**kw):
        sent.append(kw)
        return SimpleNamespace(id=5)

    interaction = SimpleNamespace(user=SimpleNamespace(id=1), followup=SimpleNamespace(send=send))
    await send_pages(interaction, [discord.Embed(), discord.Embed()])
    assert sent[0]["view"].message.id == 5 and sent[0]["wait"] is True
    await send_pages(interaction, [discord.Embed()])
    assert "view" not in sent[1]  # one page: no buttons
