from collections.abc import Mapping, Sequence

import discord

from src.cf.models import Contest, Problem, User
from src.daily import DailyView
from src.db.store import MatchPlayer, MatchRow
from src.logic import (
    BINGO_REPLACE_STEP,
    DAY_SECONDS,
    LOBBY_TTL_SECONDS,
    chunked,
    format_hm,
    ordinal,
    placement,
)
from src.mashup import LogEntry, MashupRecord, MashupView
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


def member_profile_embed(
    member: discord.abc.User,
    *,
    rating: int,
    rank: int | None,
    ranked: int,
    matches: int,
    contests: int,
    changes: Sequence[int],
    peak: int | None,
    streak: int,
    codeforces: str,
    contest_wins: int = 0,
) -> discord.Embed:
    """A member's server card: match rating and rank, games, how their rating moved, daily streak
    and Codeforces rating. changes: the rating change of each finished game."""
    place = f"#{rank} of {ranked}" if rank else "unranked"
    up = sum(c > 0 for c in changes)
    down = sum(c < 0 for c in changes)
    lines = [
        f"Match rating: **{rating}** ({place})",
        f"Peak: {peak}" if peak else "Peak: none yet",
        f"Games: {matches} matches, {contests} contests "
        f"(rating up {up}, down {down}, even {len(changes) - up - down})",
        f"Contest wins: {contest_wins}",
        f"Daily streak: {streak}",
        f"Codeforces: {codeforces}",
    ]
    return discord.Embed(
        title=member.display_name, description="\n".join(lines), colour=discord.Colour.blurple()
    )


def _contest_line(c: Contest) -> str:
    hours, minutes = divmod(c.duration // 60, 60)
    when = f"<t:{c.start_time}:f> (<t:{c.start_time}:R>)" if c.start_time else "time not set"
    return f"[{esc(c.name)}]({c.url}) - {when} - {hours}h{minutes:02d}"


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


def _first_solve(players: Sequence[MatchPlayer]) -> int | None:
    return min((p.solved_at for p in players if p.solved_at is not None), default=None)


def esc(text: str) -> str:
    """Text from Codeforces or the user, safe to show in a message: no markdown and no link
    breaking brackets."""
    return discord.utils.escape_markdown(text).replace("[", "\\[").replace("]", "\\]")


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
        lines = "\n".join(f"<@{p.user_id}> ({esc(p.handle)})" for p in players)
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
        if m.mode == "custom":  # the first solver wins, nobody else is placed
            first = _first_solve(players)
            results = "\n".join(
                ("Winner " if p.solved_at == first else "")
                + f"<@{p.user_id}> {p.delta or 0:+d} (rating {ratings.get(p.user_id, 0)})"
                for p in ordered
            )
        else:
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


def stats_pages(
    rows: Sequence[tuple[int, str, int, int]],
    me_id: int,
    streaks: Mapping[int, int] | None = None,
    by_streak: bool = False,
    kind: str = "all",
) -> list[discord.Embed]:
    """Pages of 10 ranked players (never empty); the caller's row is bold and the footer says
    where they are. streaks: daily streak by user id, shown as a fire emoji and the day count.
    by_streak ranks players with a streak by it, longest first, keeping the rating order among
    equal streaks."""
    streaks = streaks or {}
    noun = {"matches": "matches", "contests": "contests"}.get(kind, "games")
    if by_streak:
        rows = sorted((r for r in rows if streaks.get(r[0])), key=lambda r: -streaks[r[0]])

    def line(rank: int, row: tuple[int, str, int, int]) -> str:
        user_id, handle, total, played = row
        text = f"{rank}. <@{user_id}> ({esc(handle)}) {total} ({played} {noun})"
        if streaks.get(user_id):
            text += f" \U0001f525{streaks[user_id]}"
        return f"**{text}**" if user_id == me_id else text

    title = (
        "Daily streaks" if by_streak else "Contest stats" if kind == "contests" else "Match stats"
    )
    if not rows:
        empty = "No daily streaks yet." if by_streak else f"No finished {noun} yet."
        return [discord.Embed(title=title, description=empty, colour=discord.Colour.gold())]
    mine = next((i for i, r in enumerate(rows, 1) if r[0] == me_id), None)
    if mine:
        you = f"You are #{mine}."
    else:
        you = "You have no daily streak." if by_streak else f"You have no finished {noun} yet."
    groups = chunked(rows, STATS_SIZE)
    pages = []
    for i, group in enumerate(groups, 1):
        first = (i - 1) * STATS_SIZE + 1
        embed = discord.Embed(
            title=title,
            description="\n".join(line(first + j, r) for j, r in enumerate(group)),
            colour=discord.Colour.gold(),
        )
        embed.set_footer(text=f"Page {i}/{len(groups)} | {you}")
        pages.append(embed)
    return pages


def ranklist_pages(
    entries: Sequence[tuple[int, int, str, float, int]], title: str, per_page: int = 10
) -> list[discord.Embed]:
    """entries: (rank, user_id, handle, points, penalty), best first."""

    def line(e: tuple[int, int, str, float, int]) -> str:
        rank, user_id, handle, points, penalty = e
        extra = f", penalty {penalty}" if penalty else ""
        return f"{rank}. <@{user_id}> ({esc(handle)}) {points:g}{extra}"

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
        return f"[{key}. {esc(problem.name)}]({problem.url}){rating}"

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
    if match.mode == "custom":
        first = _first_solve(players)
        result = "no result" if first is None else "won" if me.solved_at == first else "lost"
    else:
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
    name = "Bingo" if m.mode != "contest" else "Mashup"
    kind = name if m.team_size == 1 else f"Team {name.lower()} (team {record.team})"
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
        if p.team == rank.user_id:
            return p.delta
    return None


def _mashup_setup(view: MashupView) -> str:
    m = view.mashup
    penalty = f"{m.penalty_minutes} min per wrong attempt" if m.penalty_minutes else "no penalty"
    if m.mode != "contest":
        head = f"{m.size}x{m.size} {m.mode} bingo, " + (
            "the first full line wins" if m.win == "line" else "most lines at the end wins"
        )
        if m.mode == "replace":
            head += f", a claimed square gets a problem {BINGO_REPLACE_STEP} higher to take it over"
        head += (
            f", problems rated {m.min_rating}-{m.max_rating}, {format_hm(m.duration_seconds)} long"
        )
    else:
        head = (
            f"{m.count} problems rated {m.min_rating}-{m.max_rating}, "
            f"{format_hm(m.duration_seconds)} long, {penalty}"
        )
    return (
        head
        + (f", {m.teams} teams" if m.team_size != 1 else "")
        + (f" of up to {m.team_size}" if m.team_size > 1 else "")
        + (f", virtual contest {m.contest_id}" if m.contest_id else "")
        + (f", tags {m.tags.replace(',', ', ')}" if m.tags else "")
        + (f", without {m.exclude_tags.replace(',', ', ')}" if m.exclude_tags else "")
        + ("" if m.rated else ", unrated")
    )


def _log_who(view: MashupView, team: int, user: int) -> str:
    return f"<@{user}>" if view.mashup.team_size == 1 else f"Team {team} (<@{user}>)"


def start_message(view: MashupView) -> str:
    """The thread post that tells every player the contest began."""
    m = view.mashup
    kind = "bingo" if m.mode != "contest" else "contest"
    who = " ".join(f"<@{p.user_id}>" for p in view.players)
    return (
        f"{who}\nThe {kind} has started and ends <t:{m.ends_at}:R>. "
        "Solve the problems on Codeforces; the bot reads your submissions, and Check reads you at once."
    )


def give_up_message(view: MashupView, user: int) -> str:
    team = next(p.team for p in view.players if p.user_id == user)
    return f"{_log_who(view, team, user)} gave up."


def _stamp(view: MashupView, at: int) -> str:
    """The clock time of a solve and how long after the start it came."""
    minutes = max(0, at - (view.mashup.started_at or at)) // 60
    elapsed = f"{minutes}m" if minutes < 60 else f"{minutes // 60}h{minutes % 60:02d}m"
    return f"<t:{at}:T> (+{elapsed})"


def log_message(view: MashupView, entry: LogEntry, by_key: dict[str, Problem]) -> str:
    """One solve for the thread log; a replace bingo adds the cell's new problem."""
    m = view.mashup
    problem = by_key.get(entry.key)
    name = f"[{entry.key}](<{problem.url}>)" if problem else entry.key
    who = _log_who(view, entry.team, entry.user)
    if m.mode == "contest":
        letter = chr(ord("A") + [k for k, _ in view.problems].index(entry.key))
        text = f"{_stamp(view, entry.at)} {who} solved {letter}. {name} ({entry.rating})"
        text += f", {entry.wrong} wrong before" if entry.wrong else ""
        # view.log is oldest first, so the first entry of a problem is its first solve.
        first = next(e for e in view.log if e.key == entry.key)
        return text + (" - first solve!" if first == entry else "")
    text = f"{_stamp(view, entry.at)} {who} claimed {name} ({entry.rating})"
    cell = next(c for c in view.cells if c.key == entry.key)
    new = next((c for c in view.cells if c.cell == cell.cell and c.idx > cell.idx), None)
    if new is not None:
        other = by_key.get(new.key)
        text += f". New problem there: {f'[{new.key}](<{other.url}>)' if other else new.key}"
        text += f" ({new.rating})"
    return text


def _bingo_log(view: MashupView) -> str:
    """The newest claims, newest first, that fit a field: when, problem, team and who solved."""
    out: list[str] = []
    size = 0
    for e in reversed(view.log):
        line = f"{_stamp(view, e.at)} {e.key} ({e.rating}) - {_log_who(view, e.team, e.user)}"
        if len(out) == 12 or size + len(line) + 1 > 1000:
            break
        out.append(line)
        size += len(line) + 1
    return "\n".join(out)


def _bingo_grid(view: MashupView) -> str:
    """The board as text: a classic claimed square shows [team] instead of its problem; a replace
    one shows its problem and the holder."""
    m = view.mashup
    shown = [
        key + (f"[{team}]" if team is not None else "")
        if m.mode == "replace"
        else (f"[{team}]" if team is not None else key)
        for key, _, team in view.board.squares
    ]
    width = max(len(text) for text in shown)
    return "\n".join(
        "  ".join(text.ljust(width) for text in shown[r * m.size : (r + 1) * m.size]).rstrip()
        for r in range(m.size)
    )


def log_board_embed(view: MashupView) -> discord.Embed:
    """The current board (a bingo's grid, a contest's standings) for the newest message of the
    solve log thread."""
    if view.mashup.mode == "contest":
        letters = {key: chr(ord("A") + i) for i, (key, _) in enumerate(view.problems)}
        text = "\n".join(_standings_lines(view, letters))
    else:
        text = f"```\n{_bingo_grid(view)}\n```"
        if view.mashup.status == "finished":
            text += "\n" + "\n".join(_bingo_standings(view))
    return discord.Embed(description=text, colour=discord.Colour.gold())


def final_message(view: MashupView) -> str:
    """The closing line of the solve log thread."""
    m = view.mashup
    kind = "bingo" if m.mode != "contest" else "contest"
    if m.status == "cancelled":
        return f"This {kind} was cancelled."
    if not view.log:
        return f"The {kind} is over. Nobody solved anything."
    first = [r for r in view.standings if r.rank == 1]
    names = [
        next(f"<@{p.user_id}>" for p in view.players if p.team == r.user_id)
        if m.team_size == 1
        else f"Team {r.user_id}"
        for r in first
    ]
    verdict = f"Winner: {names[0]}." if len(names) == 1 else f"Tied for first: {', '.join(names)}."
    return f"The {kind} is over. {verdict}" + (
        " Ratings changed." if m.rated and len(view.standings) > 1 else ""
    )


def _bingo_standings(view: MashupView) -> list[str]:
    m = view.mashup
    lines = []
    for r in view.standings:
        who = esc(r.handle)
        if m.team_size != 1:
            members = ", ".join(f"<@{p.user_id}>" for p in view.players if p.team == r.user_id)
            who = f"{esc(r.handle)} ({members})"
        lines.append(f"{r.rank}. [{r.user_id}] {who} - {r.solved} squares, {r.lines} lines")
        if m.status == "active" and m.team_size == 1:
            if not next(p for p in view.players if p.team == r.user_id).active:
                lines[-1] += " (gave up)"
        if m.status == "finished" and (change := _mashup_change(view, r)) is not None:
            lines[-1] += f", rating {change:+d}"
    return lines


def _bingo_embed(view: MashupView, by_key: dict[str, Problem]) -> discord.Embed:
    """The board (a classic claimed square shows [team] instead of its problem; a replace one
    shows its problem and the holder), the open problems by row and the standings."""
    m = view.mashup
    board = view.board
    finished = m.status == "finished"
    replace = m.mode == "replace"
    grid = _bingo_grid(view)
    when = f"Ended <t:{m.ends_at}:f>" if finished else f"Ends <t:{m.ends_at}:R>"
    lines = [f"Host: <@{m.host_id}> | {_mashup_setup(view)} | {when}", f"```\n{grid}\n```"]
    lines += [
        "**Standings** (the number in brackets is the team on the board)",
        *_bingo_standings(view),
    ]
    embed = discord.Embed(
        title=(
            ("Team bingo" if m.team_size != 1 else "Bingo")
            + (" - final standings" if finished else "")
        ),
        description="\n".join(lines),
        colour=discord.Colour.green() if finished else discord.Colour.gold(),
    )
    log = _bingo_log(view)
    if log:
        embed.add_field(name="Solve log", value=log, inline=False)
    if not finished:
        for r in range(m.size):  # open problems by row, linked (a field holds 1024 characters)
            row = []
            for key, rating, team in board.squares[r * m.size : (r + 1) * m.size]:
                if team is None or replace:
                    problem = by_key.get(key)
                    row.append(f"[{key}]({problem.url}) {rating}" if problem else f"{key} {rating}")
            if row:
                embed.add_field(name=f"Row {r + 1}", value=" | ".join(row), inline=False)
    return embed


def _standings_lines(view: MashupView, letters: dict[str, str]) -> list[str]:
    m = view.mashup
    finished = m.status == "finished"
    lines: list[str] = []
    for r in view.standings:
        done = sorted(r.times.items(), key=lambda kt: kt[1])
        solved = ", ".join(
            f"{letters[k]} {t // 60}m" + (f" ({r.wrong[k]} wrong)" if r.wrong[k] else "")
            for k, t in done
        )
        detail = f" ({solved})" if solved else ""
        who = esc(r.handle)
        if m.team_size != 1:
            members = ", ".join(f"<@{p.user_id}>" for p in view.players if p.team == r.user_id)
            who = f"{esc(r.handle)} ({members})"
        lines.append(f"{r.rank}. {who} - {r.solved} solved, {format_hm(r.total_seconds)}{detail}")
        if m.status == "active" and m.team_size == 1:
            me = next(p for p in view.players if p.team == r.user_id)
            if not me.active:
                lines[-1] += " (gave up)"
        if finished and (change := _mashup_change(view, r)) is not None:
            lines[-1] += f", rating {change:+d}"
    return lines


def mashup_embed(view: MashupView, by_key: dict[str, Problem]) -> discord.Embed:
    m = view.mashup
    players = f"{len(view.players)}/{m.max_players} players"
    if m.status == "cancelled":
        return discord.Embed(
            title="Bingo cancelled" if m.mode != "contest" else "Mashup cancelled",
            description=f"{view.note or 'The contest was cancelled.'}\nHost: <@{m.host_id}>",
            colour=discord.Colour.red(),
        )
    if m.status == "lobby":
        if m.team_size != 1:
            names = "\n".join(
                f"Team {t}: "
                + (
                    ", ".join(
                        f"<@{p.user_id}> ({esc(p.handle)})" for p in view.players if p.team == t
                    )
                )
                for t in range(1, m.teams + 1)
                if any(p.team == t for p in view.players)
            )
        else:
            names = "\n".join(f"<@{p.user_id}> ({esc(p.handle)})" for p in view.players)
        return discord.Embed(
            title=f"{'Team ' if m.team_size != 1 else ''}{'bingo' if m.mode != 'contest' else 'mashup'} lobby".capitalize(),
            description=(
                f"Host: <@{m.host_id}>\n{_mashup_setup(view)}\n{players}:\n{names}\n"
                f"Expires <t:{m.created_at + LOBBY_TTL_SECONDS}:R>. "
                + (f"Join asks for a team number (1-{m.teams}). " if m.team_size != 1 else "")
                + "The host presses Start; nobody can join after that."
            ),
            colour=discord.Colour.blurple(),
        )
    if m.mode != "contest":
        return _bingo_embed(view, by_key)
    letters = {key: chr(ord("A") + i) for i, (key, _) in enumerate(view.problems)}
    finished = m.status == "finished"
    when = f"Ended <t:{m.ends_at}:f>" if finished else f"Ends <t:{m.ends_at}:R>"
    lines = [f"Host: <@{m.host_id}> | {_mashup_setup(view)} | {when}", "", "**Problems**"]
    for key, rating in view.problems:
        problem = by_key.get(key)
        name = f"[{key} {esc(problem.name)}]({problem.url})" if problem else key
        lines.append(f"{letters[key]}. {name} ({rating})")
    lines += ["", "**Standings**", *_standings_lines(view, letters)]
    return discord.Embed(
        title=(
            ("Mashup contest" if m.team_size == 1 else "Team mashup")
            + (" - final standings" if finished else "")
        ),
        description="\n".join(lines),
        colour=discord.Colour.green() if finished else discord.Colour.gold(),
    )
