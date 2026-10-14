from collections.abc import Sequence

import discord

from src.cf.models import Contest, Problem, User
from src.db.store import MatchPlayer
from src.logic import LOBBY_TTL_SECONDS, chunked
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
    totals = view.totals or {}
    if all(p.solved_at is None for p in players) and all(not p.delta for p in players):
        results = "Nobody solved it. No points changed."
    else:
        ordered = sorted(players, key=lambda p: -(p.delta or 0))
        results = "\n".join(
            f"{i}. <@{p.user_id}> {p.delta or 0:+d} (total {totals.get(p.user_id, 0)})"
            for i, p in enumerate(ordered, start=1)
        )
    embed.colour = discord.Colour.green()
    embed.add_field(name="Results", value=results, inline=False)
    if view.note:
        embed.set_footer(text=view.note)
    return embed


STATS_SIZE = 10


def stats_embed(rows: Sequence[tuple[int, str, int, int]], me_id: int) -> discord.Embed:
    """Top 10, plus the caller's own row when they are ranked lower."""

    def line(rank: int, row: tuple[int, str, int, int]) -> str:
        user_id, handle, total, played = row
        text = f"{rank}. <@{user_id}> ({handle}) {total:+d} ({played} matches)"
        return f"**{text}**" if user_id == me_id else text

    lines = [line(i, r) for i, r in enumerate(rows[:STATS_SIZE], 1)]
    mine = next(((i, r) for i, r in enumerate(rows, 1) if r[0] == me_id), None)
    footer = None
    if mine is None:
        footer = "You have no finished matches yet."
    elif mine[0] > STATS_SIZE:
        lines += ["...", line(*mine)]
    embed = discord.Embed(
        title="Match stats",
        description="\n".join(lines) or "No finished matches yet.",
        colour=discord.Colour.gold(),
    )
    if footer and lines:
        embed.set_footer(text=footer)
    return embed
