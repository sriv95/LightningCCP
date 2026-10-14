import asyncio
import sqlite3

import aiosqlite
import pytest

from src.cf.models import Contest, Problem
from src.db.store import MIGRATIONS, Store
from src.errors import AlreadyInMatch, HandleTaken, MatchClosed, MatchFull


@pytest.fixture
async def store(tmp_path):
    s = Store(tmp_path / "sub" / "t.db")
    await s.connect()
    yield s
    await s.close()


async def test_migrations_idempotent_on_reconnect(tmp_path):
    path = tmp_path / "t.db"
    s = Store(path)
    await s.connect()
    await s.link_handle(1, 2, "Tourist", 3800)
    await s.close()
    s2 = Store(path)
    await s2.connect()
    assert (await s2.get_handle(1, 2)).handle == "Tourist"
    await s2.close()


async def test_link_and_get_handle(store):
    await store.link_handle(1, 10, "Tourist", 3800)
    linked = await store.get_handle(1, 10)
    assert (linked.handle, linked.rating) == ("Tourist", 3800)
    assert await store.get_handle(1, 11) is None
    assert await store.get_handle(2, 10) is None  # guild scoped


async def test_relink_same_user_updates(store):
    await store.link_handle(1, 10, "old", 1000)
    await store.link_handle(1, 10, "new", 1100)
    assert (await store.get_handle(1, 10)).handle == "new"


async def test_link_handle_case_insensitive_conflict(store):
    # Review Focus 1
    await store.link_handle(1, 10, "Tourist", 3800)
    with pytest.raises(HandleTaken):
        await store.link_handle(1, 11, "tourist", 3800)
    assert (await store.get_handle(1, 10)).handle == "Tourist"


async def test_same_handle_in_other_guild_ok(store):
    await store.link_handle(1, 10, "Tourist", None)
    await store.link_handle(2, 11, "Tourist", None)


async def test_user_for_handle_case_insensitive(store):
    await store.link_handle(1, 10, "Tourist", None)
    assert await store.user_for_handle(1, "TOURIST") == 10
    assert await store.user_for_handle(1, "nobody") is None


async def test_unlink(store):
    await store.link_handle(1, 10, "a", None)
    assert await store.unlink(1, 10) is True
    assert await store.unlink(1, 10) is False


async def test_pending_roundtrip_and_replace(store):
    await store.save_pending(1, 10, "a", "1A", 100)
    await store.save_pending(1, 10, "a", "2B", 200)
    pending = await store.get_pending(1, 10)
    assert (pending.handle, pending.problem_key, pending.issued_at) == ("a", "2B", 200)
    await store.delete_pending(1, 10)
    assert await store.get_pending(1, 10) is None


async def test_reminder_crud(store):
    assert await store.get_reminder(1) is None
    await store.set_reminder(1, 99, None, [60, 10])
    r = await store.get_reminder(1)
    assert (r.channel_id, r.role_id, r.before_mins) == (99, None, (60, 10))
    await store.set_reminder(1, 100, 5, [30])
    assert (await store.get_reminder(1)).role_id == 5
    assert len(await store.all_reminders()) == 1
    assert await store.clear_reminder(1) is True
    assert await store.clear_reminder(1) is False


async def test_mark_sent_is_once(store):
    # Review Focus 4
    assert await store.mark_sent(1, 500, 60) is True
    assert await store.mark_sent(1, 500, 60) is False
    assert await store.mark_sent(1, 500, 10) is True
    assert await store.sent_for_guild(1) == {(500, 60), (500, 10)}


async def test_clear_reminder_forgets_sent(store):
    await store.set_reminder(1, 99, None, [10])
    await store.mark_sent(1, 500, 10)
    await store.clear_reminder(1)
    assert await store.sent_for_guild(1) == set()


async def test_contest_cache_roundtrip_replaces(store):
    a = Contest(1, "A", "CF", "BEFORE", 1000, 7200)
    b = Contest(2, "B", "ICPC", "FINISHED", None, 60)
    await store.replace_contests([a, b])
    assert sorted(await store.load_contests(), key=lambda c: c.id) == [a, b]
    await store.replace_contests([a])
    assert await store.load_contests() == [a]


async def test_problem_cache_roundtrip(store):
    p = Problem(1700, "A", "Foo", 800, ("math", "dp"))
    q = Problem(1700, "B", "Bar", None, ())
    await store.replace_problems([p, q])
    assert sorted(await store.load_problems(), key=lambda x: x.key) == [p, q]


async def test_failed_contest_replace_keeps_cache(store):
    a = Contest(1, "A", "CF", "BEFORE", 1000, 7200)
    b = Contest(2, "B", "ICPC", "FINISHED", None, 60)
    await store.replace_contests([a, b])
    with pytest.raises(sqlite3.IntegrityError):
        await store.replace_contests([a, a])  # duplicate id
    await store.link_handle(1, 1, "a", None)  # unrelated write must not commit the DELETE
    assert sorted(await store.load_contests(), key=lambda c: c.id) == [a, b]


async def test_failed_problem_replace_keeps_cache(store):
    p = Problem(1700, "A", "Foo", 800, ("math", "dp"))
    q = Problem(1700, "B", "Bar", None, ())
    await store.replace_problems([p, q])
    with pytest.raises(sqlite3.IntegrityError):
        await store.replace_problems([p, p])  # duplicate (contest_id, idx)
    await store.link_handle(1, 1, "a", None)  # unrelated write must not commit the DELETE
    assert sorted(await store.load_problems(), key=lambda x: x.key) == [p, q]


async def test_migration_2_drops_challenge_and_adds_match_tables(tmp_path):
    path = tmp_path / "t.db"
    db = await aiosqlite.connect(path)
    await db.executescript(MIGRATIONS[0] + "\nPRAGMA user_version = 1;")
    await db.execute(
        "INSERT INTO challenge (guild_id, user_id, problem_key, rating, issued_at) "
        "VALUES (1, 1, '1A', 800, 1)"
    )
    await db.commit()
    await db.close()
    s = Store(path)
    await s.connect()
    names = {r["name"] for r in await s._all("SELECT name FROM sqlite_master WHERE type = 'table'")}
    assert "challenge" not in names
    assert {"matches", "match_players"} <= names
    assert (await s._one("PRAGMA user_version"))[0] == len(MIGRATIONS)
    await s.close()


async def test_failed_replace_does_not_discard_concurrent_write(store, monkeypatch):
    seed = Problem(1, "B", "ok", 800, ())
    await store.replace_problems([seed])
    dup = [Problem(1, "A", "x", 800, ()), Problem(1, "A", "y", 800, ())]
    started, release = asyncio.Event(), asyncio.Event()
    original = store._db.executemany

    async def paused_executemany(*args, **kwargs):
        started.set()
        await release.wait()
        return await original(*args, **kwargs)

    monkeypatch.setattr(store._db, "executemany", paused_executemany)
    a = asyncio.create_task(store.replace_problems(dup))
    await started.wait()
    b = asyncio.create_task(store.link_handle(1, 1, "a", None))
    await asyncio.wait({b}, timeout=0.2)  # without the lock, b commits the DELETE here
    release.set()
    results = await asyncio.gather(a, b, return_exceptions=True)
    assert isinstance(results[0], sqlite3.IntegrityError)
    assert results[1] is None
    assert (await store.get_handle(1, 1)).handle == "a"
    assert await store.load_problems() == [seed]


async def make_match(store, host=1, mode="duel", max_players=2, invited=None, guild=1, now=100):
    await store.link_handle(guild, host, f"h{host}", 1000)
    return await store.create_match(
        guild_id=guild,
        channel_id=5,
        host_id=host,
        host_handle=f"h{host}",
        mode=mode,
        max_players=max_players,
        invited_id=invited,
        rating_arg=None,
        now=now,
    )


async def test_create_match_adds_host_player(store):
    mid = await make_match(store)
    match = await store.get_match(mid)
    assert (match.status, match.mode, match.host_id, match.max_players) == ("lobby", "duel", 1, 2)
    players = await store.get_players(mid)
    assert [(p.user_id, p.handle, p.active, p.gave_up) for p in players] == [(1, "h1", True, False)]
    assert await store.active_match_for(1, 1) == mid
    assert await store.get_match(999) is None


async def test_one_active_match_per_user(store):
    # Review Focus 5
    await make_match(store, host=1)
    with pytest.raises(AlreadyInMatch):
        await make_match(store, host=1)
    other = await make_match(store, host=2)
    with pytest.raises(AlreadyInMatch):
        await store.add_player(other, 1, 1, "h1", 101)
    assert await store.active_match_for(1, 3) is None
    assert await store.active_match_for(2, 1) is None  # guild scoped


async def test_add_player_rules(store):
    mid = await make_match(store, host=1, mode="custom", max_players=3)
    await store.add_player(mid, 1, 2, "h2", 101)
    with pytest.raises(AlreadyInMatch):
        await store.add_player(mid, 1, 2, "h2", 102)
    await store.add_player(mid, 1, 3, "h3", 103)
    with pytest.raises(MatchFull):
        await store.add_player(mid, 1, 4, "h4", 104)
    assert [p.user_id for p in await store.get_players(mid)] == [1, 2, 3]
    assert await store.cancel_match(mid, 110) is True
    with pytest.raises(MatchClosed):
        await store.add_player(mid, 1, 4, "h4", 111)


async def test_remove_player_only_in_lobby(store):
    mid = await make_match(store, mode="custom", max_players=3)
    await store.add_player(mid, 1, 2, "h2", 101)
    assert await store.remove_player(mid, 2) is True
    assert await store.remove_player(mid, 2) is False
    assert await store.active_match_for(1, 2) is None
    await store.add_player(mid, 1, 2, "h2", 102)
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2]) is True
    assert await store.remove_player(mid, 2) is False  # started: cannot leave


async def test_cancel_match_frees_everyone_once(store):
    mid = await make_match(store)
    await store.add_player(mid, 1, 2, "h2", 101)
    assert await store.cancel_match(mid, 150) is True
    assert await store.cancel_match(mid, 151) is False
    assert (await store.get_match(mid)).status == "cancelled"
    assert await store.active_match_for(1, 1) is None
    assert await store.active_match_for(1, 2) is None


async def test_start_after_cancel_is_rejected(store):
    # Review Focus 2
    mid = await make_match(store)
    await store.cancel_match(mid, 150)
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [1]) is False
    assert (await store.get_match(mid)).status == "cancelled"


async def test_start_match_sets_fields_once(store):
    mid = await make_match(store)
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [1]) is True
    assert await store.start_match(mid, 1100, "1700B", 300, 7500, [1]) is False
    m = await store.get_match(mid)
    assert (m.status, m.rating, m.problem_key, m.started_at, m.expires_at) == (
        "active",
        1000,
        "1700A",
        200,
        7400,
    )


async def test_start_match_requires_the_same_player_set(store):
    mid = await make_match(store, mode="custom", max_players=3)
    await store.add_player(mid, 1, 2, "h2", 101)
    await store.add_player(mid, 1, 3, "h3", 102)
    assert await store.remove_player(mid, 3) is True  # a player left after the snapshot
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2, 3]) is False
    await store.add_player(mid, 1, 3, "h3", 103)  # a player joined after the snapshot
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2]) is False
    assert (await store.get_match(mid)).status == "lobby"
    assert await store.start_match(mid, 1000, "1700A", 200, 7400, [3, 2, 1]) is True


async def test_give_up_frees_slot_once_and_only_when_active(store):
    mid = await make_match(store)
    await store.add_player(mid, 1, 2, "h2", 101)
    assert await store.mark_gave_up(mid, 2) is False  # lobby
    await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2])
    assert await store.mark_gave_up(mid, 2) is True
    assert await store.mark_gave_up(mid, 2) is False
    p2 = [p for p in await store.get_players(mid) if p.user_id == 2][0]
    assert p2.gave_up is True and p2.active is False
    assert await store.active_match_for(1, 2) is None
    assert await store.active_match_for(1, 1) == mid


async def test_record_solves_sets_time_once(store):
    mid = await make_match(store)
    await store.add_player(mid, 1, 2, "h2", 101)
    await store.record_solves(mid, {1: 300})  # lobby: ignored
    await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2])
    await store.record_solves(mid, {1: 300, 2: 400})
    await store.record_solves(mid, {1: 999})
    assert {p.user_id: p.solved_at for p in await store.get_players(mid)} == {1: 300, 2: 400}
    assert await store.mark_gave_up(mid, 1) is False  # solved players cannot give up


async def test_finish_match_is_once(store):
    # Review Focus 1
    mid = await make_match(store)
    await store.add_player(mid, 1, 2, "h2", 101)
    await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2])
    assert await store.finish_match(mid, 500, {1: 5, 2: -5}) is True
    assert await store.finish_match(mid, 501, {1: 50, 2: -50}) is False
    assert {p.user_id: p.delta for p in await store.get_players(mid)} == {1: 5, 2: -5}
    assert (await store.get_match(mid)).status == "finished"
    assert await store.rating(1, 1) == 805
    assert await store.rating(1, 2) == 795
    assert await store.active_match_for(1, 1) is None


async def test_rating_sums_across_matches_and_guilds(store):
    for _ in range(2):
        mid = await make_match(store, mode="solo", max_players=1)
        await store.start_match(mid, 1000, "1700A", 200, 7400, [1])
        await store.finish_match(mid, 500, {1: 5})
    other = await make_match(store, guild=2, mode="solo", max_players=1)
    await store.start_match(other, 1000, "1700A", 200, 7400, [1])
    await store.finish_match(other, 500, {1: 7})
    assert await store.rating(1, 1) == 810
    assert await store.rating(2, 1) == 807
    assert await store.rating(1, 99) == 800  # everyone starts at 800


async def test_matches_due(store):
    lobby = await make_match(store, host=1, now=100)
    active = await make_match(store, host=2, now=100)
    await store.start_match(active, 1000, "1700A", 200, 7400, [2])
    assert await store.matches_due(lobby_before=99, now=7399) == []
    assert [m.id for m in await store.matches_due(lobby_before=100, now=7399)] == [lobby]
    due = await store.matches_due(lobby_before=100, now=7400)
    assert sorted(m.id for m in due) == sorted([lobby, active])


async def test_set_message(store):
    mid = await make_match(store)
    await store.set_message(mid, 77, 88)
    m = await store.get_match(mid)
    assert (m.channel_id, m.message_id) == (77, 88)


async def test_match_survives_reconnect(tmp_path):
    # Review Focus 6
    path = tmp_path / "t.db"
    s = Store(path)
    await s.connect()
    mid = await make_match(s)
    await s.add_player(mid, 1, 2, "h2", 101)
    await s.start_match(mid, 1000, "1700A", 200, 7400, [1, 2])
    await s.close()
    s2 = Store(path)
    await s2.connect()
    assert (await s2.get_match(mid)).status == "active"
    assert len(await s2.get_players(mid)) == 2
    assert await s2.active_match_for(1, 2) == mid
    await s2.close()


async def test_stats_orders_and_counts_finished_only(store):
    await store.link_handle(1, 2, "linked2", None)
    for deltas in ({1: 5, 2: -5}, {1: -2, 2: 2}):
        mid = await make_match(store, host=1)
        await store.add_player(mid, 1, 2, "h2", 101)
        await store.start_match(mid, 1000, "1700A", 200, 7400, [1, 2])
        await store.finish_match(mid, 500, deltas)
    await make_match(store, host=3)  # lobby: not counted
    assert await store.stats(1) == [(1, "h1", 803, 2), (2, "linked2", 797, 2)]
    assert await store.stats(9) == []


async def test_linked_handles_lists_one_guild(store):
    await store.link_handle(1, 2, "b", None)
    await store.link_handle(1, 1, "a", None)
    await store.link_handle(2, 3, "c", None)
    assert sorted(await store.linked_handles(1)) == [(1, "a"), (2, "b")]


async def test_migration_3_resets_old_points_to_start_rating(tmp_path):
    path = tmp_path / "t.db"
    db = await aiosqlite.connect(path)
    await db.executescript(MIGRATIONS[0] + MIGRATIONS[1] + "\nPRAGMA user_version = 2;")
    await db.execute(
        "INSERT INTO matches (id, guild_id, channel_id, host_id, mode, max_players, status, "
        "created_at) VALUES (1, 1, 1, 1, 'solo', 1, 'finished', 1)"
    )
    await db.execute(
        "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, delta, active) "
        "VALUES (1, 1, 1, 'h1', 1, 9, 0)"
    )
    await db.commit()
    await db.close()
    s = Store(path)
    await s.connect()
    assert await s.rating(1, 1) == 800
    assert await s.stats(1) == [(1, "h1", 800, 1)]  # still counted as a finished match
    await s.close()


async def test_rating_changes_lists_finished_matches_oldest_first(store):
    for finished, delta in ((900, 5), (500, -2)):
        mid = await make_match(store, mode="solo", max_players=1)
        await store.start_match(mid, 100, "1700A", 200, 7400, [1])
        await store.finish_match(mid, finished, {1: delta})
    await make_match(store, host=1)  # lobby: not counted
    assert await store.rating_changes(1, 1) == [(500, -2), (900, 5)]
    assert await store.rating_changes(1, 2) == [] and await store.rating_changes(2, 1) == []


async def test_finished_matches_newest_first_with_players(store):
    ids = []
    for finished, delta in ((500, 10), (900, -3)):
        mid = await make_match(store, mode="solo", max_players=1)
        await store.start_match(mid, 100, "1700A", 200, 7400, [1])
        await store.finish_match(mid, finished, {1: delta})
        ids.append(mid)
    duel = await make_match(store, host=2)
    await store.add_player(duel, 1, 1, "h1", 101)
    await store.start_match(duel, 100, "1700A", 200, 7400, [1, 2])
    await store.finish_match(duel, 700, {1: 49, 2: -49})
    await make_match(store, host=1)  # lobby: not listed
    records = await store.finished_matches(1, 1)
    assert [m.id for m, _ in records] == [ids[1], duel, ids[0]]
    assert sorted((p.user_id, p.delta) for p in records[1][1]) == [(1, 49), (2, -49)]
    assert len(await store.finished_matches(1, 1, limit=2)) == 2
    assert await store.finished_matches(1, 3) == [] and await store.finished_matches(2, 1) == []


async def test_migration_5_rebuilds_the_mashup_tables(tmp_path):
    path = tmp_path / "t.db"
    db = await aiosqlite.connect(path)
    await db.executescript("\n".join(MIGRATIONS[:4]) + "\nPRAGMA user_version = 4;")
    await db.execute(
        "INSERT INTO mashups (guild_id, channel_id, host_id, started_at, ends_at, status) "
        "VALUES (1, 1, 1, 1, 2, 'active')"
    )
    await db.commit()
    await db.close()
    s = Store(path)
    await s.connect()
    assert await s.mashups_in("lobby", "active", "finished", "cancelled") == []
    mid = await s.create_mashup(
        guild_id=1,
        channel_id=1,
        host_id=1,
        host_handle="h",
        min_rating=800,
        max_rating=1500,
        count=5,
        duration_seconds=7200,
        penalty_minutes=0,
        now=10,
    )
    assert (await s.get_mashup(mid)).status == "lobby"
    await s.close()
