# LightningCCP

Slash-command Discord bot for competitive programmers, modeled on [TLE](https://github.com/cheran-senthil/TLE).

## Setup

1. Create a bot at https://discord.com/developers/applications. Invite it with the `bot` and `applications.commands` scopes. No privileged intents needed.
2. `cp .env.example .env` and set `BOT_TOKEN`.
3. `uv sync`
4. `uv run python -m src`

## Commands

| Command | What it does |
|---|---|
| `/handle link <handle>` | Link a Codeforces handle; prove it by submitting a compile error |
| `/handle set\|show\|whois\|unlink` | Admin link, profile, reverse lookup, unlink |
| `/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags]` | Random problem, hides ones you solved |
| `/match solo [rating]` | Practice alone with points |
| `/match duel [opponent] [rating]` | 1v1 duel; name an opponent or let anyone join |
| `/match custom players [rating]` | Match for 2-10 players; ranked by solve time |
| `/contests upcoming\|running\|recent` | Paginated contest lists |
| `/remind set\|show\|clear` | Contest reminders (needs Manage Server) |

A match is a public message. The lobby has Join, Start and Leave buttons; once the host presses Start, the problem appears with Check and Give Up buttons. Points are placement based and zero-sum for duels and custom matches.

## Development

```
uv run pytest
uv run ruff check . && uv run ruff format --check .
```
