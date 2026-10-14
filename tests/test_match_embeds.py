from src.cf.models import Problem
from src.db.store import MatchPlayer, MatchRow
from src.matches import MatchView
from src.ui.embeds import head_to_head_pages, history_pages, match_embed, number_pages

PROBLEM = Problem(1700, "A", "Foo", 1000, ("dp",))


def row(**kw):
    base = dict(
        id=1,
        guild_id=1,
        channel_id=5,
        message_id=None,
        host_id=10,
        mode="duel",
        max_players=2,
        invited_id=None,
        rating_arg=None,
        rating=None,
        problem_key=None,
        status="lobby",
        created_at=1000,
        started_at=None,
        expires_at=None,
        finished_at=None,
    )
    base.update(kw)
    return MatchRow(**base)


def player(uid, **kw):
    base = dict(
        match_id=1,
        guild_id=1,
        user_id=uid,
        handle=f"h{uid}",
        joined_at=1000,
        solved_at=None,
        gave_up=False,
        delta=None,
        active=True,
    )
    base.update(kw)
    return MatchPlayer(**base)


def text(embed):
    parts = [embed.title or "", embed.description or "", embed.footer.text or ""]
    parts += [f"{f.name} {f.value}" for f in embed.fields]
    return "\n".join(parts)


def test_lobby_embed():
    view = MatchView(row(invited_id=11), [player(10)])
    out = text(match_embed(view))
    assert "lobby" in out.lower()
    assert "1/2" in out
    assert "<@10>" in out and "h10" in out
    assert "<@11>" in out  # invited
    assert f"<t:{1000 + 600}:R>" in out


def test_active_embed_shows_problem_status_and_expiry():
    view = MatchView(
        row(status="active", problem_key="1700A", rating=1000, started_at=2000, expires_at=9200),
        [player(10, solved_at=2100), player(11, gave_up=True), player(12)],
        problem=PROBLEM,
    )
    embed = match_embed(view)
    out = text(embed)
    assert "1700A" in out and embed.url == PROBLEM.url
    assert "solved" in out and "gave up" in out and "solving" in out
    assert "<t:9200:R>" in out


def test_active_embed_without_cached_problem_still_renders():
    view = MatchView(
        row(status="active", problem_key="1700A", rating=1000, expires_at=9200), [player(10)]
    )
    assert "1700A" in text(match_embed(view))


def test_finished_embed_lists_signed_deltas_and_ratings():
    view = MatchView(
        row(status="finished", problem_key="1700A", rating=1000),
        [player(10, solved_at=2100, delta=5), player(11, delta=-5)],
        problem=PROBLEM,
        ratings={10: 812, 11: 797},
    )
    out = text(match_embed(view))
    assert "+5" in out and "-5" in out
    assert "rating 812" in out and "rating 797" in out
    assert "1. <@10> +5" in out and "2. <@11> -5" in out


def test_finished_embed_nobody_solved():
    view = MatchView(
        row(status="finished", problem_key="1700A", rating=1000),
        [player(10, delta=0), player(11, delta=0)],
        problem=PROBLEM,
        ratings={10: 800, 11: 800},
        note="Time is up.",
    )
    out = text(match_embed(view))
    assert "Nobody solved" in out and "Time is up." in out


def test_cancelled_embed():
    view = MatchView(row(status="cancelled"), [], note="The host left, so the match was cancelled.")
    embed = match_embed(view)
    assert "cancelled" in embed.title.lower()
    assert "host left" in embed.description
    assert "Host: <@10> | Duel" in embed.description


def record(mode, finished, deltas, *, solved=()):
    """(match, players) as the store returns them; deltas maps user id to rating change."""
    match = row(status="finished", mode=mode, problem_key="1700A", rating=800, finished_at=finished)
    players = [
        player(uid, delta=delta, solved_at=100 if uid in solved else None)
        for uid, delta in deltas.items()
    ]
    return match, players


def test_history_pages_lines():
    records = [
        record("solo", 2000, {1: 10}, solved={1}),
        record("solo", 1900, {1: 0}),
        record("duel", 1800, {1: 49, 2: -49}, solved={1}),
        record("custom", 1700, {1: -16, 2: 49, 3: 16}, solved={2, 3}),
        record("duel", 1600, {1: 0, 2: 0}),
    ]
    pages = number_pages(history_pages("T", records, 1, {PROBLEM.key: PROBLEM}))
    lines = pages[0].description.splitlines()
    assert (
        lines[0].endswith("- solved, +10") and f"[{PROBLEM.key}]({PROBLEM.url}) (800)" in lines[0]
    )
    assert lines[1].endswith("- not solved, +0")
    assert lines[2].endswith("vs <@2> - 1st of 2, +49")
    assert lines[3].endswith("vs <@2>, <@3> - 3rd of 3, -16")
    assert lines[4].endswith("vs <@2> - no result, +0")
    assert pages[0].footer.text == "Page 1/1"
    assert history_pages("T", [], 1, {}) == []


def test_head_to_head_record_and_lines():
    records = [
        record("duel", 3000, {1: 49, 2: -49}, solved={1}),
        record("duel", 2000, {1: -16, 2: 16}, solved={2}),
        record("custom", 1500, {1: 16, 2: -16, 3: 0}, solved={1}),
        record("custom", 1400, {1: 5, 3: -5}, solved={1}),  # player 2 not in it
        record("duel", 1300, {1: 0, 2: 0}),
    ]
    (page,) = head_to_head_pages("T", records, 1, 2, {})
    lines = page.description.splitlines()
    assert lines[0] == "**Wins 2, losses 1, ties 1** (4 matches)"
    assert len(lines) == 5 and "vs" not in page.description
    empty = head_to_head_pages("T", records[3:4], 1, 2, {})
    assert empty[0].description == "No matches against each other yet."
