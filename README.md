# LightningCCP

Slash-command Discord bot for competitive programmers, modeled on [TLE](https://github.com/cheran-senthil/TLE).

## Setup

1. Create a bot at https://discord.com/developers/applications. Invite it with the `bot` and `applications.commands` scopes. No privileged intents needed.
2. `cp .env.example .env` and set `BOT_TOKEN`. For development set `DEV_GUILD_ID` to your test server id so commands sync instantly.
3. `uv sync`
4. `uv run python -m src`

## Commands

| Command | What it does |
|---|---|
| `/handle link <handle>` | Link a Codeforces handle; prove it by submitting a compile error |
| `/handle set\|show\|whois\|unlink` | Admin link, profile, reverse lookup, unlink |
| `/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags]` | Random problem, hides ones you solved |
| `/gitgud [rating]`, `/gotgud`, `/nogud` | Challenge flow with points |
| `/contests upcoming\|running\|recent` | Paginated contest lists |
| `/remind set\|show\|clear` | Contest reminders (needs Manage Server) |

## Development

```
uv run pytest
uv run ruff check . && uv run ruff format --check .
```
