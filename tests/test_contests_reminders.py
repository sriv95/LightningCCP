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

    async def all_recaps(self):
        return [(1, 5)]

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
    cog._recap_retry = {}
    return cog, db, ch


async def test_catch_up_sends_one_message_and_marks_all_offsets(monkeypatch):
    cog, db, ch = build(monkeypatch, start=NOW + 300)  # 5 min out: both 60 and 10 are due
    await cog._send_due_reminders()
    assert len(ch.messages) == 1
    assert db.sent == {(1, 7, 60), (1, 7, 10)}


async def test_exclude_regex_skips_matching_contest_names(monkeypatch):
    cog, db, ch = build(monkeypatch, start=NOW + 300)
    db.reminders = [Reminder(1, 5, None, (60, 10), "^round|educational")]
    await cog._send_due_reminders()
    assert ch.messages == [] and db.sent == set()
    db.reminders = [Reminder(1, 5, None, (60, 10), "educational")]
    await cog._send_due_reminders()
    assert len(ch.messages) == 1


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


def finished(cid=7, ended_ago=600):
    return Contest(cid, "Round", "CF", "FINISHED", NOW - ended_ago - 7200, 7200)


def build_recap(monkeypatch, entries, contests=None):
    cog, db, ch = build(monkeypatch, contests=[finished()] if contests is None else contests)
    db.recaps = set()

    async def recapped(guild_id):
        return {c for g, c in db.recaps if g == guild_id}

    async def mark_recapped(guild_id, contest_id):
        if (guild_id, contest_id) in db.recaps:
            return False
        db.recaps.add((guild_id, contest_id))
        return True

    db.recapped, db.mark_recapped = recapped, mark_recapped
    calls = []

    async def fetch(guild_id, contest_id):
        calls.append(contest_id)
        if isinstance(entries, Exception):
            raise entries
        return entries

    cog._entries = fetch
    cog.calls = calls
    ch.embeds = []

    async def send(text=None, embed=None, **kwargs):
        ch.embeds.append(embed)

    ch.send = send
    return cog, db, ch


async def test_recap_posts_linked_members_once(monkeypatch):
    cog, db, ch = build_recap(monkeypatch, [(1, 10, "a", 3.0, 0), (5, 11, "b", 1.0, 40)])
    await cog._send_recaps()
    await cog._send_recaps()
    assert len(ch.embeds) == 1 and db.recaps == {(1, 7)}
    e = ch.embeds[0]
    assert e.title == "Contest recap - Round" and e.footer.text == "2 linked members took part"
    assert e.description.splitlines()[0] == "1. <@10> (a) 3"


async def test_recap_skips_empty_and_marks_it(monkeypatch):
    cog, db, ch = build_recap(monkeypatch, [])
    await cog._send_recaps()
    await cog._send_recaps()
    assert ch.embeds == [] and db.recaps == {(1, 7)} and cog.calls == [7]


async def test_recap_failure_is_retried_with_a_growing_back_off_not_marked(monkeypatch):
    cog, db, ch = build_recap(monkeypatch, RuntimeError("cf down"))
    clock = [NOW]
    monkeypatch.setattr("src.cogs.contests.time.time", lambda: clock[0])
    await cog._send_recaps()
    await cog._send_recaps()  # the same minute: still backing off
    assert db.recaps == set() and cog.calls == [7]
    clock[0] += 60
    await cog._send_recaps()
    assert cog.calls == [7, 7]
    clock[0] += 60  # the delay is 2 minutes now
    await cog._send_recaps()
    assert cog.calls == [7, 7]
    clock[0] += 60
    await cog._send_recaps()
    assert cog.calls == [7, 7, 7]
    for _ in range(10):  # many failures: the wait stops growing at 30 minutes
        clock[0] += 1800
        await cog._send_recaps()
    assert cog._recap_retry[(1, 7)][1] == 1800


async def test_recap_back_off_ends_when_it_works(monkeypatch):
    cog, db, ch = build_recap(monkeypatch, [(1, 10, "a", 3.0, 0)])
    cog._recap_retry[(1, 7)] = (NOW - 1, 120)
    await cog._send_recaps()
    assert len(ch.embeds) == 1 and cog._recap_retry == {}


async def test_recap_ignores_old_running_and_upcoming_contests(monkeypatch):
    old = finished(8, ended_ago=2 * 86400)
    running = Contest(9, "R", "CF", "CODING", NOW - 100, 7200)
    cog, db, ch = build_recap(monkeypatch, [(1, 10, "a", 3.0, 0)], [old, running])
    await cog._send_recaps()
    assert cog.calls == [] and ch.embeds == []
