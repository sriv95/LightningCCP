from src.cf.models import Contest, Problem, User
from src.cogs.contests import ContestsCog
from src.cogs.handles import HandlesCog, VerifyButton
from src.cogs.match import MatchButton, MatchCog
from src.cogs.problems import ProblemsCog
from src.ui.embeds import contest_pages, problem_embed, profile_embed, ranklist_pages, stats_embed


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
        "whois",
        "unlink",
    }
    assert HandlesCog.handle.guild_only


def test_verify_button_template_matches_custom_id():
    assert VerifyButton.__discord_ui_compiled_template__.fullmatch("verify:12345")


def test_problems_commands_registered():
    names = {c.name for c in ProblemsCog.__cog_app_commands__}
    assert names == {"gimme"}


def test_contests_groups():
    assert {c.name for c in ContestsCog.contests.commands} == {"upcoming", "running", "recent"}
    assert "ranklist" in {c.name for c in ContestsCog.__cog_app_commands__}
    assert {c.name for c in ContestsCog.remind.commands} == {"set", "show", "clear"}


def test_match_group_commands():
    assert {c.name for c in MatchCog.match.commands} == {"solo", "duel", "custom"}
    assert MatchCog.match.guild_only


def test_match_button_template_matches_all_actions():
    template = MatchButton.__discord_ui_compiled_template__
    for action in ("join", "start", "leave", "check", "giveup"):
        assert template.fullmatch(f"match:{action}:42")
    assert not template.fullmatch("match:other:42")


def test_stats_embed_shows_caller_below_top_ten():
    rows = [(i, f"h{i}", 100 - i, 1) for i in range(1, 13)]
    desc = stats_embed(rows, 12).description
    assert desc.count("\n") == 11 and "1. <@1>" in desc and "10. <@10>" in desc
    assert "11." not in desc and desc.endswith("**12. <@12> (h12) +88 (1 matches)**")
    in_top = stats_embed(rows, 2)
    assert "..." not in in_top.description and "**2. <@2>" in in_top.description
    assert stats_embed(rows, 99).footer.text == "You have no finished matches yet."
    assert stats_embed([], 1).description == "No finished matches yet."


def test_ranklist_pages_lines_and_paging():
    entries = [(i, i, f"h{i}", 100.0 - i, 0) for i in range(1, 13)]
    entries[1] = (2, 2, "h2", 90.0, 45)
    pages = ranklist_pages(entries, "T")
    assert [p.footer.text for p in pages] == ["Page 1/2", "Page 2/2"]
    assert pages[0].description.splitlines()[0] == "1. <@1> (h1) 99"
    assert pages[0].description.splitlines()[1] == "2. <@2> (h2) 90, penalty 45"
    assert ranklist_pages([], "T") == []
