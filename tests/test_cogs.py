from src.cf.models import Contest, Problem, User
from src.ui.embeds import contest_pages, problem_embed, profile_embed


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
