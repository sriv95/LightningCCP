import logging
from types import SimpleNamespace

import pytest

from src.cf.models import Contest
from src.cogs.contests import ContestsCog
from src.db.store import Reminder
from src.errors import CacheNotReady

NOW = 10_000
START = NOW + 3600


class FakeDB:
    def __init__(self, reminders):
        self.reminders = reminders
        self.sent: set[tuple[int, int, int]] = set()

    async def all_reminders(self):
        return self.reminders

    async def sent_for_guild(self, guild_id):
        return {(c, b) for g, c, b in self.sent if g == guild_id}

    async def mark_sent(self, guild_id, contest_id, before):
        key = (guild_id, contest_id, before)
        if key in self.sent:
            return False
        self.sent.add(key)
        return True


class FakeChannel:
    def __init__(self):
        self.messages: list[str] = []

    async def send(self, text, **kwargs):
        self.messages.append(text)


def make_cog(monkeypatch, *, before_mins=(60, 10), contests=None, channel=True, start=START):
    monkeypatch.setattr("src.cogs.contests.time.time", lambda: NOW)
    db = FakeDB([Reminder(1, 5, None, before_mins)])
    ch = FakeChannel()
    contests = [Contest(7, "Round", "CF", "BEFORE", start, 7200)] if contests is None else contests
    bot = SimpleNamespace(
        get_channel=lambda cid: ch if channel else None,
        services=SimpleNamespace(db=db, contests=SimpleNamespace(contests=contests)),
    )
    return ContestsCog.__new__(ContestsCog), bot, db, ch


def build(monkeypatch, **kw):
    cog, bot, db, ch = make_cog(monkeypatch, **kw)
    cog.bot = bot
    cog._warned_channels = set()
    return cog, db, ch


async def test_catch_up_sends_one_message_and_marks_all_offsets(monkeypatch):
    cog, db, ch = build(monkeypatch, start=NOW + 300)  # 5 min out: both 60 and 10 are due
    await cog._send_due_reminders()
    assert len(ch.messages) == 1
    assert db.sent == {(1, 7, 60), (1, 7, 10)}


async def test_at_most_once(monkeypatch):
    cog, db, ch = build(monkeypatch, start=NOW + 300)
    await cog._send_due_reminders()
    await cog._send_due_reminders()
    assert len(ch.messages) == 1


async def test_nothing_sent_at_or_after_start(monkeypatch):
    cog, db, ch = build(monkeypatch, start=NOW)
    await cog._send_due_reminders()
    assert ch.messages == [] and db.sent == set()


async def test_not_yet_due_offset_not_marked(monkeypatch):
    cog, db, ch = build(monkeypatch, before_mins=(120, 60, 10), start=NOW + 1800)
    await cog._send_due_reminders()
    assert len(ch.messages) == 1
    assert db.sent == {(1, 7, 120), (1, 7, 60)}  # 10 not due yet


async def test_missing_channel_does_not_raise_or_mark_and_warns_once(monkeypatch, caplog):
    cog, db, ch = build(monkeypatch, channel=False)
    with caplog.at_level(logging.WARNING):
        await cog._send_due_reminders()
        await cog._send_due_reminders()
    assert db.sent == set() and ch.messages == []
    assert len([r for r in caplog.records if "not found" in r.message]) == 1


async def test_list_raises_cache_not_ready_when_empty(monkeypatch):
    cog, db, ch = build(monkeypatch, contests=[])
    with pytest.raises(CacheNotReady):
        await cog._list(None, "upcoming")
