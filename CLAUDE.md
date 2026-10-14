# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

LightningCCP is a slash-command-only Discord bot for competitive programmers, modeled on [TLE](https://github.com/cheran-senthil/TLE) but rewritten from scratch (TLE is a behavior reference, not a fork). Phase 1 covers Codeforces handle linking, `/gimme` and `/gitgud`, contest lists and reminders. Graphs, duels, starboard, rated virtual contests and rank roles are phase 2 and do not exist yet.

## Commands

Python is managed with `uv`. This is an application, not a package (`[tool.uv] package = false`), so there is no install step beyond `uv sync`.

```
uv sync                                  # install deps
uv run python -m src                     # run the bot (needs BOT_TOKEN in .env)
uv run pytest                            # all tests
uv run pytest tests/test_logic.py::test_name -v   # one test
uv run ruff check . && uv run ruff format --check .   # lint (both must pass)
```

Config comes from `.env` (see `.env.example`): `BOT_TOKEN` (required), `DEV_GUILD_ID` (optional; syncs slash commands to that guild instantly, otherwise they sync globally and can take up to an hour), `DB_PATH` (default `data/bot.db`).

## Layout rules

- All Python logic lives directly in `src/` (there is no `src/lightningccp/` package). Imports use the `src.` prefix, and `pytest` runs with `pythonpath = ["."]`. The repo root holds only config and docs.
- `docs/` is gitignored on purpose. Specs and plans live there locally; never `git add` anything under `docs/` or `.superpowers/`.
- Ruff ignores E501 (the formatter owns wrapping). Target is Python 3.12, so PEP 695 syntax such as `def f[T]()` is fine.

## Architecture

Slash commands only. The bot uses `Intents.default()` (no `message_content`, no `members`) and `help_command=None`; do not add prefix commands.

**Startup** (`LightningBot.setup_hook` in `src/bot.py`) runs in a fixed order: open the SQLite store (migrates), build the `Services` container, start the caches (load from DB, then the refresh loop's first tick fetches live), load the three cogs, sync the command tree. `close()` calls `super().close()` first so cogs and the reminder loop stop before the session and DB close.

**`Services`** (`src/services.py`) holds `db`, `cf`, `problems`, `contests`, `session`. It is attached as `bot.services`; cogs reach everything through `self.bot.services`. There are no module-level globals, which is a deliberate departure from TLE.

**Data flow.** Cogs are thin. Decisions live in pure functions in `src/logic.py` (problem filter, tag parsing and autocomplete suggestions, compile-error verification window, gitgud points, contest selection, reminder-due calculation), which is where unit tests concentrate. `src/cf/client.py` is the Codeforces API client; `src/cache/*` wrap it with periodic refresh and persist to SQLite so restarts are fast; `src/db/store.py` is the only SQL.

**Codeforces client.** One request per second enforced through a lock plus an injected clock and sleep (tests inject fakes); retries on "limit exceeded"; non-JSON or network failures become `CodeforcesError`; "not found" becomes `HandleNotFound`.

**Caches.** `ProblemCache` refreshes every 6h, `ContestCache` every 30 min (5 min while a contest is running). On a failed or empty refresh the old data is kept and the loop retries in 5 minutes. `ProblemCache.require()` raises `CacheNotReady` when empty.

**Store.** Single aiosqlite connection, migrations keyed on `PRAGMA user_version` (append new scripts to `MIGRATIONS`, never edit old ones). Handle uniqueness is per guild and case-insensitive (`COLLATE NOCASE`). "One open challenge per user" is a partial unique index on `challenge`. `mark_sent` returns `True` only the first time, which is what makes reminders at-most-once. There is no write lock, so avoid awaiting other work between a write and its commit.

**Errors.** Raise `BotError(user_message)` subclasses (`src/errors.py`) for anything the user should see. `report_error` is shared by `tree.on_error` and the Verify button's `on_error`, and sends an ephemeral reply. After a public `defer()` it deletes the original placeholder first, because the first followup would otherwise be public.

## Behavior that is easy to break

- Every command that calls Codeforces must `await interaction.response.defer()` first (Discord's 3 second limit).
- Handle linking uses a compile-error check, not OAuth: `/handle link` stores a `pending_link` row (survives restarts) and sends a persistent `VerifyButton` (`discord.ui.DynamicItem`, custom id `verify:<user_id>`, registered in `HandlesCog.cog_load`). Verification requires a `COMPILATION_ERROR` on the issued problem between `issued_at` and `issued_at + 300`.
- Reminders (`ContestsCog._send_due_reminders`, 60 s loop): record the sent rows before posting, never ping at or after contest start, and post one message per contest per tick for the smallest due offset while marking every due offset as sent.
- `/gimme` and `/gitgud` never recommend unrated problems; solved-problem exclusion matches by problem key (`contestId + index`) only.
- Discord does not support per-subcommand default permissions, so admin subcommands use `@app_commands.checks.has_permissions(manage_guild=True)`; the `/remind` group also sets `default_permissions`.

## Testing notes

Tests cover the client (injected fake session in `tests/conftest.py`), store (real aiosqlite on a temp file), logic, caches, `report_error`, and the reminder loop (fakes). Cog command bodies are not unit-tested; `tests/test_cogs.py` only checks command registration and embeds. Nothing has been run against live Discord, so verify user-facing flows manually in a dev guild (`DEV_GUILD_ID`) before relying on them.
