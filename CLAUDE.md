# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

LightningCCP is a slash-command-only Discord bot for competitive programmers, modeled on [TLE](https://github.com/cheran-senthil/TLE) but rewritten from scratch (TLE is a behavior reference, not a fork). Phase 1 covers Codeforces handle linking, `/gimme`, `/daily` and `/match`, contest lists, `/ranklist`, `/stalk` and reminders. Graphs, starboard, rated virtual contests and rank roles are phase 2 and do not exist yet.

## Working agreement

Always ask the user before adding any new feature, command, option or behavior, and wait for an answer. Bug fixes and changes the user asked for explicitly do not need this. Features not yet built are phase 2 (see above); do not start them unprompted.

## Commands

Python is managed with `uv`. This is an application, not a package (`[tool.uv] package = false`), so there is no install step beyond `uv sync`.

```
uv sync                                  # install deps
uv run python -m src                     # run the bot (needs BOT_TOKEN in .env)
docker compose up -d --build            # run in Docker (needs .env; DB in the bot-data volume)
uv run pytest                            # all tests
uv run pytest tests/test_logic.py::test_name -v   # one test
uv run ruff check . && uv run ruff format --check .   # lint (both must pass)
```

Config comes from `.env` (see `.env.example`): `BOT_TOKEN` (required), `DB_PATH` (default `data/bot.db`).

## Layout rules

- All Python logic lives directly in `src/` (there is no `src/lightningccp/` package). Imports use the `src.` prefix, and `pytest` runs with `pythonpath = ["."]`. The repo root holds only config and docs.
- `docs/` is gitignored on purpose. Specs and plans live there locally; never `git add` anything under `docs/` or `.superpowers/`.
- Ruff ignores E501 (the formatter owns wrapping). Target is Python 3.12, so PEP 695 syntax such as `def f[T]()` is fine.

## Architecture

Slash commands only. The bot uses `Intents.default()` (no `message_content`, no `members`) and `help_command=None`; do not add prefix commands.

**Startup** (`LightningBot.setup_hook` in `src/bot.py`) runs in a fixed order: open the SQLite store (migrates), build the `Services` container, start the caches (load from DB, then the refresh loop's first tick fetches live), load the six cogs, sync the command tree. `close()` calls `super().close()` first so cogs and the reminder loop stop before the session and DB close.

**`Services`** (`src/services.py`) holds `db`, `cf`, `problems`, `contests`, `matches`, `mashups`, `session`. It is attached as `bot.services`; cogs reach everything through `self.bot.services`. There are no module-level globals, which is a deliberate departure from TLE.

**Data flow.** Cogs are thin. Decisions live in pure functions in `src/logic.py` (problem filter, tag parsing and autocomplete suggestions, compile-error verification window, match rating (solo linear gain, `duel_gain` staircase for duel and custom, default problem rating), contest selection, reminder-due calculation), which is where unit tests concentrate. `src/cf/client.py` is the Codeforces API client; `src/cache/*` wrap it with periodic refresh and persist to SQLite so restarts are fast; `src/db/store.py` is the only SQL. `src/matches.py` (`MatchService`, with `MatchView`) is the only place match state transitions happen, over the `matches` and `match_players` tables; `src/cogs/match.py` is the thin cog (`/match solo|duel|custom|history`, `/stats`, persistent `MatchButton` DynamicItem with custom id `match:<action>:<id>`, 60 s expiry loop).

**Mashup contests.** `src/mashup.py` (`MashupService`) and `src/cogs/mashup.py` (`/host-rated` and `/host-unrated`, each with `contest` and `team-contest`; the rated flag is `mashups.rated`, an unrated contest gets no deltas from `_rating_changes`; persistent `MashupButton` with custom id `mashup:<join|start|leave|check>:<id>`, 60 s loop). Same shape as matches: lobby (10 min), host Start (picks problems unsolved by every player, closes joining), active, finished or cancelled. Tables `mashups`, `mashup_problems`, `mashup_players`, `mashup_solves` (migration 5 rebuilt the migration 4 versions). A solve is the earliest accepted submission between `started_at` and `ends_at` in the player's last 100 submissions, plus the count of wrong attempts before it (`logic.first_solve`); `logic.pick_mashup` and `logic.mashup_standings` hold the pure decisions. A finished mashup rates players: `MashupService._rating_changes` feeds the standings groups to `match_deltas` (teams rated at their members' average) and `Store.finish_mashup(deltas)` stores them in `mashup_players.delta` in the same transaction that ends the contest; `Store.rating`, `stats` and `rating_changes` sum both `match_players.delta` and `mashup_players.delta`. `MashupService.history` gives each finished contest's place for `/match history` (a team's place in a team contest). The loop refreshes every active contest's players each minute (one Codeforces call each, none for a player who solved everything, cap 20 players, lowered per contest by the host's `players` option, stored in `mashups.max_players`), edits the message only when a solve is new, cancels expired lobbies and finishes contests past `ends_at`.

**Daily problem.** `src/daily.py` (`DailyService.claim`, behind `/daily` in `cogs/problems.py`) issues one problem per user per UTC day (`logic.utc_day`), rated the match rating floored to 100 (`logic.floor_rating`) or the optional `rating` option (a different rating than an unsolved daily swaps in a new problem via `Store.issue_daily`; a solve is claimed first); running `/daily` again checks for a solve after `issued_at`. The `daily` table has one row per (guild, user, day). A solve books double the solo gain (`DAILY_MULTIPLIER`) as a finished solo `matches` row so ratings, stats, plots and history need no change. A solo match's solve claims the day atomically inside `Store.finish_match(bonus_day=...)` (doubling its delta, leaving `problem_key` NULL if no daily was issued); `daily.solved_at IS NULL` guards both paths against double payout. The daily streak (`logic.daily_streak`, over `Store.daily_days`) counts claimed days where consecutive claims are at most 2 days apart, so one missed day is forgiven and two in a row end it. A daily is not an active match, so it never blocks other activities.

**Team mashups.** `/host-rated team-contest` and `/host-unrated team-contest` sets `mashups.teams` (0 = individual) and each `mashup_players.team`; `mashups.team_size` (the `players` option, 0 = no cap) caps each team inside `Store.add_mashup_player`. Join on a team contest opens `TeamModal` (a modal cannot follow a deferred response, so `MashupCog.on_button` sends it before deferring), which calls `MashupService.join(..., team)`. Solves are still stored per player; `MashupView.standings` folds them into teams with `logic.team_solves` (earliest `(time, wrong)` per problem) and feeds `mashup_standings` with the team number as `MashupRank.user_id`. The 60 s refresh skips every member of a team that solved everything.

**One activity per user.** A user can be in one unfinished match or mashup per server: the partial unique indexes `one_active_match` and `one_active_mashup` cover same-kind clashes, and `create_match`, `add_player`, `create_mashup` and `add_mashup_player` check the other table inside the same write transaction (`AlreadyInMatch`).

**Plots.** `src/cogs/plot.py` (`/plot rating|codeforces`) renders with `matplotlib.figure.Figure` in `src/ui/plots.py` (never pyplot) inside `asyncio.to_thread`, so drawing does not block the event loop. Match rating history is rebuilt from the stored deltas of finished matches (`Store.rating_changes`, `logic.rating_series`).

**Codeforces client.** One request per second enforced through a lock plus an injected clock and sleep (tests inject fakes); retries on "limit exceeded"; non-JSON or network failures become `CodeforcesError`; a "not found" comment starting with "handle" becomes `HandleNotFound` (other not-found errors, such as an unknown contest, stay `CodeforcesError`). `standings` splits a failing chunk of handles in halves to drop an unknown handle.

**Caches.** `ProblemCache` refreshes every 6h, `ContestCache` every 30 min (5 min while a contest is running). On a failed or empty refresh the old data is kept and the loop retries in 5 minutes. `ProblemCache.require()` raises `CacheNotReady` when empty.

**Store.** Single aiosqlite connection, migrations keyed on `PRAGMA user_version` (append new scripts to `MIGRATIONS`, never edit old ones). Handle uniqueness is per guild and case-insensitive (`COLLATE NOCASE`). All writes run through `Store._tx` (one write lock, commit or roll back). Match transitions are single guarded SQL transactions (`... WHERE status = 'lobby'` and so on), and the partial unique index `one_active_match` on `match_players` enforces one unfinished match per user per server. `start_match` only starts if the lobby's player set is unchanged, and `finish_match` returns `False` when another request already finished the match, so never score outside `finish_match`. `mark_sent` returns `True` only the first time, which is what makes reminders at-most-once.

**Errors.** Raise `BotError(user_message)` subclasses (`src/errors.py`) for anything the user should see. `report_error` is shared by `tree.on_error` and the button callbacks' `except` blocks (discord.py never calls `DynamicItem.on_error`), and sends an ephemeral reply. After a public `defer()` it deletes the original placeholder first, because the first followup would otherwise be public.

## Behavior that is easy to break

- Every command that calls Codeforces must `await interaction.response.defer()` first (Discord's 3 second limit).
- Handle linking uses a compile-error check, not OAuth: `/handle link` stores a `pending_link` row (survives restarts) and sends a persistent `VerifyButton` (`discord.ui.DynamicItem`, custom id `verify:<user_id>`, registered in `HandlesCog.cog_load`). Verification requires a `COMPILATION_ERROR` on the issued problem between `issued_at` and `issued_at + 300`.
- Reminders (`ContestsCog._send_due_reminders`, 60 s loop): record the sent rows before posting, never ping at or after contest start, and post one message per contest per tick for the smallest due offset while marking every due offset as sent.
- Contest recaps (`ContestsCog._send_recaps`, same 60 s loop, `logic.recaps_due`): a finished contest that ended within 24 h gets one embed of the linked members' official standings in the server's default bot channel (`/channel set`, stored in the `reminder` table). The standings are fetched first and a failure is retried next tick; `mark_recapped` then runs before posting, so a recap is at-most-once per server and contest, and a contest with no linked participant is marked without a post. `/channel clear` also forgets the recaps.
- `/gimme` and `/match` never recommend unrated problems; solved-problem exclusion matches by problem key (`contestId + index`) only. A match problem is unsolved by every player and follows the match's `tags`/`exclude_tags` (stored normalized by `logic.normalize_tags`, `''` for none; a rematch reuses them).
- Match button callbacks must `defer()` then edit the original message (except Rematch on a finished duel, which posts a new lobby with `MatchService.rematch` and leaves the result alone); Start fetches every player's history (about 2 Codeforces calls each). A solve's time is the Codeforces submission time, not the click time; it is compared against the bot's local clock for `started_at`, so a badly skewed host clock can reject a very fast solve.
- Discord does not support per-subcommand default permissions, so admin subcommands use `@app_commands.checks.has_permissions(manage_guild=True)`; the `/channel` group also sets `default_permissions`.

## Testing notes

Tests cover the client (injected fake session in `tests/conftest.py`), store (real aiosqlite on a temp file), logic, caches, `report_error`, and the reminder loop (fakes). `tests/test_matches.py` runs `MatchService` with a fake Codeforces client over the real store. Cog command bodies are not unit-tested; `tests/test_cogs.py` only checks command registration and embeds, and `tests/test_buttons.py` covers button error reporting, match re-post and Check feedback with fakes. Nothing has been run against live Discord, so verify user-facing flows manually in a test server before relying on them.
