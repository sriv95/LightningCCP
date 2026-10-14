from src.cf.models import Contest, Problem, User
from src.cogs.contests import ContestsCog
from src.cogs.handles import HandlesCog, VerifyButton
from src.cogs.mashup import MashupButton, MashupCog
from src.cogs.match import MatchButton, MatchCog
from src.cogs.plot import PlotCog
from src.cogs.problems import ProblemsCog
from src.ui.embeds import (
    contest_pages,
    problem_embed,
    profile_embed,
    ranklist_pages,
    stalk_pages,
    stats_embed,
)


class FakeMember:
    display_name = "Alice"
    display_avatar = None


def test_problem_embed_hides_tags_in_spoiler():
    e = problem_embed(Problem(1700, "A", "Foo", 800, ("dp", "math")), footer="hi")
    assert e.title == "1700A. Foo"
    assert e.url == "https://codeforces.com/problemset/problem/1700/A"
    values = {f.name: f.value for f in e.fields}
    assert values["Rating"] == "800"
    assert values["Tags"] == "||dp, math||"
    assert e.footer.text == "hi"


def test_problem_embed_unrated_no_tags():
    values = {f.name: f.value for f in problem_embed(Problem(1, "A", "x", None, ())).fields}
    assert values["Rating"] == "unrated" and values["Tags"] == "none"


def test_profile_embed_unrated():
    e = profile_embed(User("newbie", None, None, None), FakeMember())
    assert "newbie" in e.title
    assert "unrated" in e.description


def test_contest_pages_paginates_and_labels():
    cs = [Contest(i, f"C{i}", "CF", "BEFORE", 1000 + i, 7200) for i in range(23)]
    pages = contest_pages(cs, "Upcoming contests")
    assert len(pages) == 3
    assert pages[0].footer.text == "Page 1/3"
    assert pages[0].description.count("\n") == 9  # 10 lines
    assert "<t:1000:f>" in pages[0].description and "2h00" in pages[0].description
    assert contest_pages([], "x") == []


def test_handles_group_commands():
    assert {c.name for c in HandlesCog.handle.commands} == {
        "link",
        "show",
        "unlink",
    }
    assert HandlesCog.handle.guild_only


def test_verify_button_template_matches_custom_id():
    assert VerifyButton.__discord_ui_compiled_template__.fullmatch("verify:12345")


def test_problems_commands_registered():
    names = {c.name for c in ProblemsCog.__cog_app_commands__}
    assert names == {"gimme", "daily", "stalk"}


def test_contests_groups():
    assert {c.name for c in ContestsCog.contests.commands} == {"upcoming", "running", "recent"}
    assert "ranklist" in {c.name for c in ContestsCog.__cog_app_commands__}
    assert {c.name for c in ContestsCog.channel.commands} == {"set", "show", "clear"}


def test_match_group_commands():
    assert {c.name for c in MatchCog.match.commands} == {"solo", "duel", "custom", "history"}
    assert MatchCog.match.guild_only


def test_match_button_template_matches_all_actions():
    template = MatchButton.__discord_ui_compiled_template__
    for action in ("join", "start", "leave", "check", "giveup", "rematch"):
        assert template.fullmatch(f"match:{action}:42")
    assert not template.fullmatch("match:other:42")


def test_stats_embed_shows_caller_below_top_ten():
    rows = [(i, f"h{i}", 100 - i, 1) for i in range(1, 13)]
    desc = stats_embed(rows, 12).description
    assert desc.count("\n") == 11 and "1. <@1>" in desc and "10. <@10>" in desc
    assert "11." not in desc and desc.endswith("**12. <@12> (h12) 88 (1 matches)**")
    in_top = stats_embed(rows, 2)
    assert "..." not in in_top.description and "**2. <@2>" in in_top.description
    assert stats_embed(rows, 99).footer.text == "You have no finished matches yet."
    assert stats_embed([], 1).description == "No finished matches yet."


def test_stats_embed_shows_daily_streak_with_fire():
    rows = [(1, "a", 900, 3), (2, "b", 850, 2)]
    lines = stats_embed(rows, 9, {1: 4, 2: 0}).description.splitlines()
    assert lines[0].endswith("900 (3 matches) \U0001f5254")
    assert lines[1].endswith("850 (2 matches)")


def test_stats_embed_sorts_by_streak():
    rows = [(1, "a", 900, 3), (2, "b", 850, 2), (3, "c", 800, 1), (4, "d", 790, 1)]
    e = stats_embed(rows, 4, {2: 5, 3: 5, 4: 1}, by_streak=True)
    lines = e.description.splitlines()
    assert e.title == "Daily streaks" and len(lines) == 3  # player 1 has no streak
    assert "<@2>" in lines[0] and "<@3>" in lines[1]  # equal streaks keep the rating order
    assert lines[2].startswith("**3. <@4>") and lines[2].endswith("\U0001f5251**")
    nobody = stats_embed(rows, 1, {2: 5}, by_streak=True)
    assert nobody.footer.text == "You have no daily streak."
    assert stats_embed(rows, 1, {}, by_streak=True).description == "No daily streaks yet."


def test_ranklist_pages_lines_and_paging():
    entries = [(i, i, f"h{i}", 100.0 - i, 0) for i in range(1, 13)]
    entries[1] = (2, 2, "h2", 90.0, 45)
    pages = ranklist_pages(entries, "T")
    assert [p.footer.text for p in pages] == ["Page 1/2", "Page 2/2"]
    assert pages[0].description.splitlines()[0] == "1. <@1> (h1) 99"
    assert pages[0].description.splitlines()[1] == "2. <@2> (h2) 90, penalty 45"
    assert ranklist_pages([], "T") == []


def test_stalk_pages_lines_and_paging():
    known = Problem(1700, "B", "Foo", 1400, ())
    items = [("1700B", known), ("9999Z", None)] + [(f"1A{i}", None) for i in range(14)]
    pages = stalk_pages("tourist", items)
    assert [p.footer.text for p in pages] == ["Page 1/2 - 16 problems", "Page 2/2 - 16 problems"]
    first = pages[0].description.splitlines()
    assert first[0] == f"[1700B. Foo]({known.url}) (1400)" and first[1] == "9999Z"
    assert pages[0].title == "Unsolved by tourist"
    assert stalk_pages("x", []) == []


def test_plot_group_commands():
    assert {c.name for c in PlotCog.plot.commands} == {"rating", "codeforces"}


def test_rating_plot_renders_png():
    from src.ui.plots import CF_BANDS, rating_plot

    png = rating_plot("T", [(1_700_000_000, 1500), (1_700_100_000, 1450)], CF_BANDS)
    assert png.read(8) == b"\x89PNG\r\n\x1a\n"
    assert rating_plot("T", [(1_700_000_000, 800)]).read(4) == b"\x89PNG"  # one point works


def test_mashup_group_commands_and_button_template():
    for group in (MashupCog.host_rated, MashupCog.host_unrated):
        assert {c.name for c in group.commands} == {"contest", "team-contest", "virtual"}
        assert group.guild_only
    template = MashupButton.__discord_ui_compiled_template__
    for action in ("join", "start", "leave", "check", "rematch"):
        assert template.fullmatch(f"mashup:{action}:7")
    assert not template.fullmatch("mashup:other:7")
