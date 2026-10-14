# LightningCCP

Slash-command Discord bot for competitive programmers, modeled on [TLE](https://github.com/cheran-senthil/TLE).

## Setup

1. Create a bot at https://discord.com/developers/applications. Invite it with the `bot` and `applications.commands` scopes. No privileged intents needed.
2. `cp .env.example .env` and set `BOT_TOKEN`.
3. `uv sync`
4. `uv run python -m src`

### Docker

```
cp .env.example .env   # set BOT_TOKEN
docker compose up -d --build
docker compose logs -f
```

The SQLite database lives in the `bot-data` named volume, so it survives rebuilds. Update with `git pull && docker compose up -d --build`. `docker compose down` keeps the data; add `-v` to delete it.

## Commands

| Command | What it does |
|---|---|
| `/handle link <handle>` | Link a Codeforces handle; prove it by submitting a compile error |
| `/handle show [member] [handle]` | Profile of a member (default: you) or of whoever linked a handle |
| `/handle unlink` | Remove your link |
| `/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags]` | Random problem, hides ones you solved; with no rating arguments it aims at your match rating |
| `/match solo [rating]` | Practice alone; solving raises your match rating |
| `/match duel [opponent] [rating]` | 1v1 duel; name an opponent or let anyone join |
| `/match custom players [rating]` | Match for 2-10 players; ranked by solve time |
| `/match history [member]` | Your last 100 finished matches; with a member, also your record and matches against them (private) |
| `/stats` | Top 10 by match rating, plus your own rank (private) |
| `/plot rating [member] [handle]` | Graph of a member's match rating in this server (private; default: you) |
| `/plot codeforces [member] [handle]` | Graph of a Codeforces rating history, any handle works (private; default: you) |
| `/contests upcoming\|running\|recent` | Paginated contest lists |
| `/stalk [handle] [member]` | Problems someone tried but never solved (private; default: you) |
| `/ranklist contest_id` | Official standings of this server's linked members in a contest (private) |
| `/remind set\|show\|clear` | Contest reminders (needs Manage Server) |

A match is a public message. The lobby has Join, Start and Leave buttons; once the host presses Start, the problem appears with Check and Give Up buttons. Everyone starts at a match rating of 800. With no `rating` argument, `/gimme` and `/match` pick a problem at your match rating (rounded to 100; the players' average for duel and custom). After a match, ratings change. Solo only counts solving, so giving up or running out of time never lowers your rating: you gain +1 when the problem is 400 or more below your rating, +10 when equal and +33 when 400 or more above (linear in between). Duel and custom compare every pair of players by solve order, and the winner gains a fixed step by how far the loser is rated from them: +16 when the loser is 400 or more below, +33 from 200 below, +49 within 200 of each other, +67 from 200 above, +95 from 400 above. The loser loses the same, and a tie moves nothing. Custom matches average these results over the other players.

## Development

```
uv run pytest
uv run ruff check . && uv run ruff format --check .
```
