import pytest

from src.cf.client import CodeforcesClient
from src.errors import CodeforcesError, HandleNotFound
from tests.conftest import FakeResponse, FakeSession, failed, ok


def make_client(*responses, **kw):
    session = FakeSession(*responses)
    sleeps: list[float] = []

    async def fake_sleep(seconds):
        sleeps.append(seconds)

    client = CodeforcesClient(session, interval=0, retry_delay=0, sleep=fake_sleep, **kw)
    return client, session, sleeps


async def test_user_info_parses():
    client, session, _ = make_client(
        ok(
            [
                {
                    "handle": "Tourist",
                    "rating": 3800,
                    "maxRating": 3979,
                    "rank": "legendary grandmaster",
                }
            ]
        )
    )
    user = await client.user_info("tourist")
    assert (user.handle, user.rating, user.max_rating, user.rank) == (
        "Tourist",
        3800,
        3979,
        "legendary grandmaster",
    )
    assert session.calls[0][0].endswith("user.info")
    assert session.calls[0][1] == {"handles": "tourist"}


async def test_user_info_unrated_user():
    client, *_ = make_client(ok([{"handle": "newbie"}]))
    user = await client.user_info("newbie")
    assert user.rating is None and user.rank is None


async def test_handle_not_found_maps_to_error():
    client, *_ = make_client(failed("handles: User with handle nobody not found"))
    with pytest.raises(HandleNotFound):
        await client.user_info("nobody")


async def test_retry_on_rate_limit_then_success():
    client, session, sleeps = make_client(failed("Call limit exceeded"), ok([{"handle": "a"}]))
    user = await client.user_info("a")
    assert user.handle == "a"
    assert len(session.calls) == 2
    assert sleeps  # backed off before retrying


async def test_rate_limit_exhausted_raises():
    client, session, _ = make_client(*(failed("Call limit exceeded") for _ in range(3)))
    with pytest.raises(CodeforcesError):
        await client.user_info("a")
    assert len(session.calls) == 3


async def test_other_failure_raises_codeforces_error():
    client, *_ = make_client(failed("Something odd"))
    with pytest.raises(CodeforcesError):
        await client.contests()


async def test_non_json_body_maps_to_codeforces_error():
    # Review Focus 2: maintenance pages are HTML, json() raises ValueError.
    client, *_ = make_client(FakeResponse(exc=ValueError("not json")))
    with pytest.raises(CodeforcesError):
        await client.problemset()


async def test_problemset_parses_and_keeps_unrated():
    client, *_ = make_client(
        ok(
            {
                "problems": [
                    {
                        "contestId": 1700,
                        "index": "A",
                        "name": "Foo",
                        "rating": 800,
                        "tags": ["math"],
                    },
                    {"contestId": 1700, "index": "B", "name": "Bar", "tags": []},
                ],
                "problemStatistics": [],
            }
        )
    )
    problems = await client.problemset()
    assert [p.key for p in problems] == ["1700A", "1700B"]
    assert problems[0].rating == 800 and problems[0].tags == ("math",)
    assert problems[1].rating is None
    assert problems[0].url == "https://codeforces.com/problemset/problem/1700/A"


async def test_contests_parses_missing_start_time():
    client, *_ = make_client(
        ok(
            [
                {
                    "id": 1,
                    "name": "R1",
                    "type": "CF",
                    "phase": "BEFORE",
                    "durationSeconds": 7200,
                    "startTimeSeconds": 1000,
                },
                {"id": 2, "name": "R2", "type": "CF", "phase": "BEFORE", "durationSeconds": 60},
            ]
        )
    )
    contests = await client.contests()
    assert contests[0].start_time == 1000 and contests[0].duration == 7200
    assert contests[1].start_time is None
    assert contests[0].url == "https://codeforces.com/contest/1"


async def test_user_status_params_and_parse():
    client, session, _ = make_client(
        ok(
            [
                {
                    "id": 9,
                    "creationTimeSeconds": 500,
                    "problem": {"contestId": 1700, "index": "A", "name": "x"},
                    "verdict": "OK",
                },
                {
                    "id": 8,
                    "creationTimeSeconds": 400,
                    "problem": {"contestId": 1700, "index": "B", "name": "y"},
                },
            ]
        ),
        ok([]),
    )
    subs = await client.user_status("a", count=10)
    assert session.calls[0][1] == {"handle": "a", "from": 1, "count": 10}
    assert subs[0].problem_key == "1700A" and subs[0].verdict == "OK"
    assert subs[1].verdict is None
    await client.user_status("a")
    assert session.calls[1][1] == {"handle": "a", "from": 1}


async def test_user_info_semicolon_handle_is_not_found():
    client, *_ = make_client(ok([{"handle": "a"}, {"handle": "b"}]))
    with pytest.raises(HandleNotFound):
        await client.user_info("a;b")


def standing_row(rank, *handles, points=1500.0, penalty=0):
    members = [{"handle": h} for h in handles]
    return {"party": {"members": members}, "rank": rank, "points": points, "penalty": penalty}


async def test_standings_parses_and_sends_filter():
    client, session, _ = make_client(ok({"rows": [standing_row(3, "a", "b", points=7.0)]}))
    rows = await client.standings(1700, ["a", "c"])
    assert [(r.rank, r.handles, r.points, r.penalty) for r in rows] == [(3, ("a", "b"), 7.0, 0)]
    assert session.calls[0][1]["handles"] == "a;c" and session.calls[0][1]["contestId"] == 1700
    assert session.calls[0][1]["showUnofficial"] == "false"


async def test_standings_chunks_handles():
    client, session, _ = make_client(
        ok({"rows": [standing_row(1, "a")]}), ok({"rows": [standing_row(2, "c")]})
    )
    rows = await client.standings(1, ["a", "b", "c"], chunk=2)
    assert [r.rank for r in rows] == [1, 2]
    assert [c[1]["handles"] for c in session.calls] == ["a;b", "c"]


async def test_standings_splits_to_drop_unknown_handle():
    not_found = "handles: User with handle b not found"
    client, session, _ = make_client(
        failed(not_found),  # a;b
        ok({"rows": [standing_row(1, "a")]}),  # a
        failed(not_found),  # b
    )
    rows = await client.standings(1, ["a", "b"])
    assert [r.handles for r in rows] == [("a",)]
    assert [c[1]["handles"] for c in session.calls] == ["a;b", "a", "b"]


async def test_unknown_contest_is_not_a_handle_error():
    client, *_ = make_client(failed("contestId: Contest with id 5 not found"))
    with pytest.raises(CodeforcesError) as info:
        await client.standings(5, ["a"])
    assert not isinstance(info.value, HandleNotFound)


async def test_rating_history_parses():
    client, session, _ = make_client(
        ok(
            [
                {"ratingUpdateTimeSeconds": 100, "newRating": 1500, "oldRating": 1400},
                {"ratingUpdateTimeSeconds": 200, "newRating": 1450, "oldRating": 1500},
            ]
        )
    )
    changes = await client.rating_history("a")
    assert [(c.time, c.rating) for c in changes] == [(100, 1500), (200, 1450)]
    assert session.calls[0][0].endswith("user.rating") and session.calls[0][1] == {"handle": "a"}
