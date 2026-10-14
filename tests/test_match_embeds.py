from src.cf.models import Problem
from src.db.store import MatchPlayer, MatchRow
from src.matches import MatchView
from src.ui.embeds import match_embed

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


def test_finished_embed_lists_signed_deltas_and_totals():
    view = MatchView(
        row(status="finished", problem_key="1700A", rating=1000),
        [player(10, solved_at=2100, delta=5), player(11, delta=-5)],
        problem=PROBLEM,
        totals={10: 12, 11: -3},
    )
    out = text(match_embed(view))
    assert "+5" in out and "-5" in out
    assert "total 12" in out and "total -3" in out


def test_finished_embed_nobody_solved():
    view = MatchView(
        row(status="finished", problem_key="1700A", rating=1000),
        [player(10, delta=0), player(11, delta=0)],
        problem=PROBLEM,
        totals={10: 0, 11: 0},
        note="Time is up.",
    )
    out = text(match_embed(view))
    assert "Nobody solved" in out and "Time is up." in out


def test_cancelled_embed():
    view = MatchView(row(status="cancelled"), [], note="The host left, so the match was cancelled.")
    embed = match_embed(view)
    assert "cancelled" in embed.title.lower()
    assert "host left" in embed.description
