import sqlite3

import pytest

from src.cf.models import Contest, Problem
from src.db.store import Store
from src.errors import HandleTaken


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
