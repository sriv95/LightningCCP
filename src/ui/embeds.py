from collections.abc import Mapping, Sequence

import discord

from src.cf.models import Contest, Problem, User
from src.daily import DailyView
from src.db.store import MatchPlayer, MatchRow
from src.logic import (
    DAY_SECONDS,
    LOBBY_TTL_SECONDS,
    chunked,
    format_hm,
    ordinal,
    placement,
)
from src.mashup import MashupRecord, MashupView
from src.matches import MatchView


def problem_embed(problem: Problem, *, footer: str | None = None) -> discord.Embed:
    embed = discord.Embed(
        title=f"{problem.key}. {problem.name}", url=problem.url, colour=discord.Colour.blue()
    )
    embed.add_field(name="Rating", value=str(problem.rating) if problem.rating else "unrated")
    tags = ", ".join(problem.tags)
    embed.add_field(name="Tags", value=f"||{tags}||" if tags else "none")
    if footer:
        embed.set_footer(text=footer)
    return embed


def _days(n: int) -> str:
    return f"{n} day{'' if n == 1 else 's'}"


def daily_embed(view: DailyView) -> discord.Embed:
    row = view.row
    resets = f"<t:{(row.day + 1) * DAY_SECONDS}:R>"
    if row.solved_at is None:
        footer = "Solve it on Codeforces, then run /daily again to claim double rating."
        if view.problem is None:  # dropped from the problem cache since it was issued
            embed = discord.Embed(title=f"Daily: {row.problem_key}", colour=discord.Colour.blue())
            embed.set_footer(text=footer)
        else:
            embed = problem_embed(view.problem, footer=footer)
            embed.title = f"Daily: {embed.title}"
        embed.description = f"New daily problem {resets}."
        if view.streak:
            embed.add_field(name="Streak", value=_days(view.streak))
        return embed
    if view.delta is not None:
        text = f"Daily solved: {view.delta:+d} rating (double). Next problem {resets}."
    elif row.problem_key is None:
        text = f"Today's daily is done: your first solo win counted double. Next {resets}."
    else:
        text = f"Today's daily is already done. Next problem {resets}."
    embed = discord.Embed(title="Daily", description=text, colour=discord.Colour.green())
    embed.add_field(name="Streak", value=_days(view.streak))
    return embed


def profile_embed(user: User, member: discord.abc.User) -> discord.Embed:
    rating = f"{user.rating} ({user.rank})" if user.rating is not None else "unrated"
    peak = f"\nPeak: {user.max_rating}" if user.max_rating is not None else ""
    embed = discord.Embed(
        title=f"{user.handle} - {member.display_name}",
        url=f"https://codeforces.com/profile/{user.handle}",
        description=f"Rating: {rating}{peak}",
        colour=discord.Colour.green(),
    )
    return embed


def _contest_line(c: Contest) -> str:
    hours, minutes = divmod(c.duration // 60, 60)
    when = f"<t:{c.start_time}:f> (<t:{c.start_time}:R>)" if c.start_time else "time not set"
    return f"[{c.name}]({c.url}) - {when} - {hours}h{minutes:02d}"


def contest_pages(
    contests: Sequence[Contest], title: str, per_page: int = 10
) -> list[discord.Embed]:
    groups = chunked(contests, per_page)
    pages = []
    for i, group in enumerate(groups, start=1):
        embed = discord.Embed(
            title=title,
            description="\n".join(_contest_line(c) for c in group),
            colour=discord.Colour.orange(),
        )
        embed.set_footer(text=f"Page {i}/{len(groups)}")
        pages.append(embed)
    return pages


_MODE_LABEL = {"solo": "Solo", "duel": "Duel"}


def _mode_label(view: MatchView) -> str:
    m = view.match
    if m.mode == "custom":
        return f"Custom ({m.max_players} players max)"
    return _MODE_LABEL[m.mode]


def _player_status(p: MatchPlayer) -> str:
    if p.solved_at is not None:
        return f"solved <t:{p.solved_at}:R>"
    return "gave up" if p.gave_up else "solving"


def match_embed(view: MatchView) -> discord.Embed:
    m, players = view.match, view.players
    if m.status == "cancelled":
        return discord.Embed(
            title="Match cancelled",
            description=(
                f"{view.note or 'The match was cancelled.'}\n"
                f"Host: <@{m.host_id}> | {_mode_label(view)}"
            ),
            colour=discord.Colour.red(),
        )
    if m.status == "lobby":
        lines = "\n".join(f"<@{p.user_id}> ({p.handle})" for p in players)
        desc = f"Host: <@{m.host_id}>\nPlayers {len(players)}/{m.max_players}:\n{lines}"
        if m.invited_id is not None:
            desc += f"\nInvited: <@{m.invited_id}>"
        if m.tags:
            desc += f"\nTags: {m.tags.replace(',', ', ')}"
        if m.exclude_tags:
            desc += f"\nExcluding: {m.exclude_tags.replace(',', ', ')}"
        desc += f"\nExpires <t:{m.created_at + LOBBY_TTL_SECONDS}:R>. The host presses Start."
        return discord.Embed(
            title=f"Match lobby - {_mode_label(view)}",
            description=desc,
            colour=discord.Colour.blurple(),
        )
    if view.problem is not None:
        embed = problem_embed(view.problem)
    else:
        embed = discord.Embed(title=f"Problem {m.problem_key}", colour=discord.Colour.blue())
    embed.set_author(name=f"{_mode_label(view)} match")
    if m.status == "active":
        status = "\n".join(f"<@{p.user_id}>: {_player_status(p)}" for p in players)
        embed.add_field(name="Players", value=status, inline=False)
        embed.add_field(name="Ends", value=f"<t:{m.expires_at}:R>", inline=False)
        return embed
    ratings = view.ratings or {}
    if all(p.solved_at is None for p in players) and all(not p.delta for p in players):
        results = "Nobody solved it. No ratings changed."
    else:
        ordered = sorted(players, key=lambda p: -(p.delta or 0))
        results = "\n".join(
            f"{i}. <@{p.user_id}> {p.delta or 0:+d} (rating {ratings.get(p.user_id, 0)})"
            for i, p in enumerate(ordered, start=1)
        )
    embed.colour = discord.Colour.green()
    embed.add_field(name="Results", value=results, inline=False)
    if view.note:
        embed.set_footer(text=view.note)
    return embed


STATS_SIZE = 10


def stats_embed(
    rows: Sequence[tuple[int, str, int, int]],
    me_id: int,
    streaks: Mapping[int, int] | None = None,
    by_streak: bool = False,
) -> discord.Embed:
    """Top 10, plus the caller's own row when they are ranked lower. streaks: daily streak by
    user id, shown as a fire emoji and the day count. by_streak ranks players with a streak by
    it, longest first, keeping the rating order among equal streaks."""
    streaks = streaks or {}
    if by_streak:
        rows = sorted((r for r in rows if streaks.get(r[0])), key=lambda r: -streaks[r[0]])

    def line(rank: int, row: tuple[int, str, int, int]) -> str:
        user_id, handle, total, played = row
        text = f"{rank}. <@{user_id}> ({handle}) {total} ({played} matches)"
        if streaks.get(user_id):
            text += f" \U0001f525{streaks[user_id]}"
        return f"**{text}**" if user_id == me_id else text

    lines = [line(i, r) for i, r in enumerate(rows[:STATS_SIZE], 1)]
    mine = next(((i, r) for i, r in enumerate(rows, 1) if r[0] == me_id), None)
    footer = None
    if mine is None:
        footer = "You have no daily streak." if by_streak else "You have no finished matches yet."
    elif mine[0] > STATS_SIZE:
        lines += ["...", line(*mine)]
    embed = discord.Embed(
        title="Daily streaks" if by_streak else "Match stats",
        description="\n".join(lines)
        or ("No daily streaks yet." if by_streak else "No finished matches yet."),
        colour=discord.Colour.gold(),
    )
    if footer and lines:
        embed.set_footer(text=footer)
    return embed


def ranklist_pages(
    entries: Sequence[tuple[int, int, str, float, int]], title: str, per_page: int = 10
) -> list[discord.Embed]:
    """entries: (rank, user_id, handle, points, penalty), best first."""

    def line(e: tuple[int, int, str, float, int]) -> str:
        rank, user_id, handle, points, penalty = e
        extra = f", penalty {penalty}" if penalty else ""
        return f"{rank}. <@{user_id}> ({handle}) {points:g}{extra}"

    groups = chunked(entries, per_page)
    pages = []
    for i, group in enumerate(groups, start=1):
        embed = discord.Embed(
            title=title,
            description="\n".join(line(e) for e in group),
            colour=discord.Colour.teal(),
        )
        embed.set_footer(text=f"Page {i}/{len(groups)}")
        pages.append(embed)
    return pages


def recap_embed(
    contest: Contest, entries: Sequence[tuple[int, int, str, float, int]]
) -> discord.Embed:
    """entries: (rank, user_id, handle, points, penalty), best first; the top 10 are shown."""
    embed = ranklist_pages(entries[:10], f"Contest recap - {contest.name}")[0]
    embed.url = contest.url
    n = len(entries)
    embed.set_footer(text=f"{n} linked member{'' if n == 1 else 's'} took part")
    return embed


def stalk_pages(
    handle: str, items: Sequence[tuple[str, Problem | None]], per_page: int = 15
) -> list[discord.Embed]:
    """items: (problem key, problem if the cache knows it), newest attempt first."""

    def line(key: str, problem: Problem | None) -> str:
        if problem is None:
            return key
        rating = f" ({problem.rating})" if problem.rating else ""
        return f"[{key}. {problem.name}]({problem.url}){rating}"

    groups = chunked(items, per_page)
    pages = []
    for i, group in enumerate(groups, start=1):
        embed = discord.Embed(
            title=f"Unsolved by {handle}",
            url=f"https://codeforces.com/profile/{handle}",
            description="\n".join(line(*item) for item in group),
            colour=discord.Colour.dark_orange(),
        )
        embed.set_footer(text=f"Page {i}/{len(groups)} - {len(items)} problems")
        pages.append(embed)
    return pages


MatchRecord = tuple[MatchRow, list[MatchPlayer]]


def _history_line(
    record: MatchRecord, me_id: int, by_key: dict[str, Problem], *, versus: bool = True
) -> str:
    match, players = record
    me = next(p for p in players if p.user_id == me_id)
    problem = by_key.get(match.problem_key or "")
    name = f"[{problem.key}]({problem.url})" if problem else str(match.problem_key)
    head = (
        f"<t:{match.finished_at}:d> {_MODE_LABEL.get(match.mode, 'Custom')} {name} ({match.rating})"
    )
    if match.mode == "solo":
        return f"{head} - {'solved' if me.solved_at is not None else 'not solved'}, {me.delta:+d}"
    place = placement([p.delta or 0 for p in players], me.delta or 0)
    result = "no result" if place is None else f"{ordinal(place)} of {len(players)}"
    others = [f"<@{p.user_id}>" for p in players if p.user_id != me_id]
    vs = f" vs {', '.join(others[:3])}{f' +{len(others) - 3}' if len(others) > 3 else ''}"
    return f"{head}{vs if versus else ''} - {result}, {me.delta:+d}"


def _line_pages(title: str, lines: Sequence[str], colour: discord.Colour) -> list[discord.Embed]:
    return [
        discord.Embed(title=title, description="\n".join(group), colour=colour)
        for group in chunked(lines, 10)
    ]


def _mashup_history_line(record: MashupRecord) -> str:
    m = record.mashup
    kind = f"Team mashup (team {record.team})" if m.teams else "Mashup"
    place = f"{ordinal(record.rank)} of {record.entrants}" if record.solved else "nothing solved"
    rating = f", {record.delta:+d}" if record.delta is not None else ""
    return f"<t:{m.ends_at}:d> {kind} - {place}, {record.solved}/{m.count} solved{rating}"


def history_pages(
    title: str,
    records: Sequence[MatchRecord],
    user_id: int,
    by_key: dict[str, Problem],
    mashups: Sequence[MashupRecord] = (),
) -> list[discord.Embed]:
    """Matches and finished mashups together, newest first. Empty when there are none."""
    entries = [(r[0].finished_at, _history_line(r, user_id, by_key)) for r in records]
    entries += [(r.mashup.ends_at, _mashup_history_line(r)) for r in mashups]
    entries.sort(key=lambda e: -e[0])  # stable: equal times keep matches first
    return _line_pages(title, [line for _, line in entries], discord.Colour.purple())


def head_to_head_pages(
    title: str,
    records: Sequence[MatchRecord],
    me_id: int,
    other_id: int,
    by_key: dict[str, Problem],
) -> list[discord.Embed]:
    """Matches both players finished, with the win/loss record by rating change. Never empty."""
    shared = [r for r in records if any(p.user_id == other_id for p in r[1])]
    if not shared:
        return [discord.Embed(title=title, description="No matches against each other yet.")]
    wins = losses = ties = 0
    for _, players in shared:
        mine = next(p for p in players if p.user_id == me_id).delta or 0
        theirs = next(p for p in players if p.user_id == other_id).delta or 0
        wins, losses, ties = (
            wins + (mine > theirs),
            losses + (mine < theirs),
            ties + (mine == theirs),
        )
    summary = f"**Wins {wins}, losses {losses}, ties {ties}** ({len(shared)} matches)"
    lines = [_history_line(r, me_id, by_key, versus=False) for r in shared]
    pages = _line_pages(title, lines, discord.Colour.purple())
    pages[0].description = f"{summary}\n{pages[0].description}"
    return pages


def number_pages(pages: list[discord.Embed]) -> list[discord.Embed]:
    for i, page in enumerate(pages, start=1):
        page.set_footer(text=f"Page {i}/{len(pages)}")
    return pages


def _mashup_change(view: MashupView, rank) -> int | None:
    """The match rating change of a standings row (a player, or a team in a team contest)."""
    for p in view.players:
        if (p.team if view.mashup.teams else p.user_id) == rank.user_id:
            return p.delta
    return None


def _mashup_setup(view: MashupView) -> str:
    m = view.mashup
    penalty = f"{m.penalty_minutes} min per wrong attempt" if m.penalty_minutes else "no penalty"
    return (
        f"{m.count} problems rated {m.min_rating}-{m.max_rating}, "
        f"{format_hm(m.duration_seconds)} long, {penalty}"
        + (f", {m.teams} teams" if m.teams else "")
    )


def mashup_embed(view: MashupView, by_key: dict[str, Problem]) -> discord.Embed:
    m = view.mashup
    players = f"{len(view.players)}/{m.max_players} players"
    if m.status == "cancelled":
        return discord.Embed(
            title="Mashup cancelled",
            description=f"{view.note or 'The contest was cancelled.'}\nHost: <@{m.host_id}>",
            colour=discord.Colour.red(),
        )
    if m.status == "lobby":
        if m.teams:
            names = "\n".join(
                f"Team {t}: "
                + (", ".join(f"<@{p.user_id}> ({p.handle})" for p in view.players if p.team == t))
                for t in range(1, m.teams + 1)
                if any(p.team == t for p in view.players)
            )
        else:
            names = "\n".join(f"<@{p.user_id}> ({p.handle})" for p in view.players)
        return discord.Embed(
            title="Team mashup lobby" if m.teams else "Mashup lobby",
            description=(
                f"Host: <@{m.host_id}>\n{_mashup_setup(view)}\n{players}:\n{names}\n"
                f"Expires <t:{m.created_at + LOBBY_TTL_SECONDS}:R>. "
                + (f"Join asks for a team number (1-{m.teams}). " if m.teams else "")
                + "The host presses Start; nobody can join after that."
            ),
            colour=discord.Colour.blurple(),
        )
    letters = {key: chr(ord("A") + i) for i, (key, _) in enumerate(view.problems)}
    finished = m.status == "finished"
    when = f"Ended <t:{m.ends_at}:f>" if finished else f"Ends <t:{m.ends_at}:R>"
    lines = [f"Host: <@{m.host_id}> | {_mashup_setup(view)} | {when}", "", "**Problems**"]
    for key, rating in view.problems:
        problem = by_key.get(key)
        name = f"[{key} {problem.name}]({problem.url})" if problem else key
        lines.append(f"{letters[key]}. {name} ({rating})")
    lines += ["", "**Standings**"]
    for r in view.standings:
        done = sorted(r.times.items(), key=lambda kt: kt[1])
        solved = ", ".join(
            f"{letters[k]} {t // 60}m" + (f" ({r.wrong[k]} wrong)" if r.wrong[k] else "")
            for k, t in done
        )
        detail = f" ({solved})" if solved else ""
        who = r.handle
        if m.teams:
            members = ", ".join(f"<@{p.user_id}>" for p in view.players if p.team == r.user_id)
            who = f"{r.handle} ({members})"
        lines.append(f"{r.rank}. {who} - {r.solved} solved, {format_hm(r.total_seconds)}{detail}")
        if finished and (change := _mashup_change(view, r)) is not None:
            lines[-1] += f", rating {change:+d}"
    return discord.Embed(
        title=(
            ("Team mashup" if m.teams else "Mashup contest")
            + (" - final standings" if finished else "")
        ),
        description="\n".join(lines),
        colour=discord.Colour.green() if finished else discord.Colour.gold(),
    )
