from collections.abc import Sequence

import discord

from src.cf.models import Contest, Problem, User
from src.logic import chunked


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
