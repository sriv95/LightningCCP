## Parity contract: LightningCCP Rust port

Status: APPROVED by user 2026-11-09. The user owns this file. A worker never decides that a
difference is acceptable: it reports it as "gap" or "proposed intended". Edit in one place, with a dated line
saying what changed and why. Source of the rules: `KNOWLEDGES/LightningCCP/DECISIONS.md`, 2026-11-09.
Python references are file:line in the `src/` of this branch (HEAD at contract time).

- End state: replacement. Standalone binary, no PyO3, original deleted at cut-over (rollback = old image tag).
  Same env vars (`BOT_TOKEN` required, `DB_PATH` default `data/bot.db`, src/config.py:15-19), same volume,
  same DB path `/app/data/bot.db` (Dockerfile:9, docker-compose.yaml:6).
- Unit: one slash command or button interaction -> the Discord response (embed text, content, ephemeral flag,
  files) plus DB rows written; one loop tick (reminders, recaps, weekly, new-contest, cache refresh, match and
  mashup ticks) with a fake clock and fake Codeforces client -> posts, DB rows, Codeforces calls with times.
- Measurement: corpus in `parity/fixtures`, script `parity/run.sh` (to be built in step 4); report
  identical/total, never "looks the same".

### Exact output (byte-equal, no normalization)
- Embed text: titles, descriptions, fields, footers, colors, message content (src/ui/embeds.py, src/ui/help.py).
- `esc` markdown escaping (src/ui/embeds.py:156): `discord.utils.escape_markdown` then `[` and `]` backslashed.
  The Rust version must reproduce escape_markdown exactly (including its code-block and URL handling), not a
  simplified escape.
- `.ics` output of `logic.contest_ics` (src/logic.py:428), and the file name `contest-<id>.ics`
  (src/cogs/contests.py:171).
- Error messages: every `BotError` user message (src/errors.py and all `raise BotError(...)` sites), including
  the ephemeral reply shape.
- Slash command names, subcommand groups, option names, option descriptions, choices, command descriptions,
  default permissions, `manage_guild` checks.
- Button custom ids: `verify:<user_id>` (src/cogs/handles.py:21), `match:<join|start|leave|check|giveup|rematch>:<id>`
  (src/cogs/match.py:47), `mashup:<same actions>:<id>` (src/cogs/mashup.py:88), `cal|notify:<contest_id>`
  (src/cogs/contests.py:83). Button labels and styles equal too.

### Data-level checks only (bytes cannot match)
- Plots (`/plot rating|codeforces`, src/ui/plots.py): compare the data series (points and order), axes and
  titles/labels, not pixels. Rust uses plotters with an embedded font.
- Random picks (`random.choice` at src/daily.py:82, src/matches.py:203, src/cogs/handles.py:85,
  src/cogs/problems.py:77; `rng` param of `pick_mashup` src/logic.py:475 and `pick_replacement` src/logic.py:565):
  the Rust code takes an injected rng; tests seed it. Check: each pick is a member of the pool that the Python
  filter allows (rating, tags, exclude_tags, unsolved by all players, `mashups.max_rating`). Not checked: same
  pick as Python.
- 2026-11-09 (user decision, DECISIONS.md 2026-11-09): seeded rng is injected everywhere Python uses the global
  `random`: daily, match, handle verify, `/gimme`, bingo (the sites above), so tests can seed every pick.

### Timing (same behavior, checked with a fake clock)
- Loop intervals: problem cache 6 h (5 min after a failed or empty refresh, src/cache/problems.py:19,45-47);
  contest cache 30 min, 5 min while a contest runs or after a failure, else `refresh_minutes`
  (src/cache/contests.py:19-38); new-contest loop 30 min (src/cogs/contests.py:406); reminder, recap and weekly
  loop 60 s (src/cogs/contests.py:447); match expiry loop 60 s (src/cogs/match.py:413); mashup loop 60 s base,
  re-set each tick from `svc.interval`, 10 s to 60 s (src/cogs/mashup.py:482-489, src/mashup.py:137,531).
- Codeforces client: 1 request per second (`interval=1.0`), retry on "limit exceeded" with 2 s delay, 3 attempts
  (src/cf/client.py:20-58).
- In-memory recap back-off: first failure 1 min, doubling, capped at 30 min (`RECAP_MAX_BACKOFF = 1800`),
  kept in memory only, cleared on success (src/cogs/contests.py:72,526-535).
- At-most-once mark order, each mark before the post it guards:
  `mark_sent` (src/cogs/contests.py:499-500: larger offsets first, the smallest offset last and its result
  decides whether to post), `mark_weekly` (src/cogs/contests.py:552), `mark_recapped` (src/cogs/contests.py:536,
  recap fetch first, mark after a successful fetch, marked without a post when no entries). Same rule for
  `mark_posted`/`mark_soon` (src/cogs/contests.py:429-436) and `mark_logged` (src/mashup.py:178).
- Mashup read budget: `MASHUP_READS_PER_SECOND` (0.5) x last interval (src/mashup.py:594).
- 2026-11-09 (user decision, DECISIONS.md 2026-11-09): Codeforces down at startup is the same as Python exactly:
  log the failed first tick, keep DB-loaded data, retry in 5 minutes, no abort (src/cache/problems.py:32-47,
  src/bot.py:57-58).
- 2026-11-09 (user decision, DECISIONS.md 2026-11-09): global `tree.sync()` runs on every startup after cogs load,
  the same as Python exactly (src/bot.py:64).

### DB (byte-compatible, Python stays a rollback option on the same file)
- Schema, migrations (24, reaching user_version 24, append-only, src/db/store.py:16) and `PRAGMA user_version` (src/db/store.py:493-499)
  byte-compatible: Rust applies the same scripts in the same order, each in one transaction with its version bump,
  and never edits an old one.
- Includes the partial unique indexes `one_active_match` (src/db/store.py:107) and `one_active_mashup`
  (src/db/store.py:184), `COLLATE NOCASE` handle uniqueness, and stored value formats (normalized tags, `''` for
  none, NULL conventions).
- Check: DB written by Rust opens and passes the Python test store suite and a Python run of the loops; DB
  written by Python opens in Rust; `sqlite_master` dump and `user_version` are identical for a fresh file.

### Reminder-exclude regex (check step before step 3)
Python uses `re.search` with `re.IGNORECASE` (src/cogs/contests.py:50,60), max 100 chars. Before stage 3 starts
porting `_not_excluded`, a worker:
1. Reads every stored pattern (`SELECT guild_id, exclude FROM reminder WHERE exclude != ''`) from a COPY of the
   live DB (never the live file).
2. Inspects each for lookaround (`(?=`, `(?!`, `(?<=`, `(?<!`) and backreferences (`\1`..`\9`, `(?P=name)`).
3. Port with `fancy-regex` (user decision 2026-11-09: it accepts lookaround and backreferences like Python `re`, so there is no accept/reject difference).
4. Records the result (patterns count, verdict) in this file with a date.
Syntax and error-text gaps beyond lookaround and backreferences: see Acceptable differences (2026-11-09, regex item).
- 2026-11-09 live DB check (read-only copy of volume `lightningccp_bot-data`, user_version 24): `reminder` has 1 row and 0 non-empty `exclude` patterns (`src/db/store.py:262`, renamed at `:272`), so no lookaround or backreference constructs to port. Verdict: `fancy-regex` (user decision 2026-11-09).

### Release profile
`opt-level = 3`, `lto = true`, `codegen-units = 1`, `panic = "unwind"`, `strip = true`.
2026-11-09 (user decision, DECISIONS.md 2026-11-09): `panic = "unwind"` instead of `"abort"`, so one panicking task
does not kill the bot, closer to Python (Python logs and continues after an exception in a loop tick).

### Acceptable differences (closed list)
Beyond the data-level rules above (plots by data, random picks by validity), only these, decided by the user
2026-11-09 (DECISIONS.md 2026-11-09):
- Discord rate-limit and retry may follow serenity's own behavior.
- Regex: the `Invalid regex: ` prefix must match exactly; the detail text after it may differ (it may come from the
  Rust crate). Scoped to the detail text only.
- Logs: same INFO level and same logged events; line format may differ.
- Shutdown and in-flight work: best effort.
- 2026-11-10 (main session decision): the mashup loop and message edits log any error and keep running, where Python's
  `tasks.loop` would stop for good on a non-HTTP error; kept because a loop that dies permanently is a bug.
- 2026-11-10 (lead decision, a): malformed Codeforces payloads (wrong-typed fields, integers past i64, an object or empty
  string where a list belongs, an array where an object belongs) are rejected by Rust with the generic error, where
  Python may accept them; Codeforces does not send these, and external input is validated strictly.
- 2026-11-10 (lead decision, b): regex-engine differences for exotic patterns: `\N{name}` lookup (needs a Unicode name
  table), case-insensitive İ/ı, `\B` on an empty string, and nested repeats over the compile-size limit (memory safety).
- 2026-11-10 (lead decision, c): embed timestamp text form: the same instant serializes as `+00:00` in Python and `.000Z`
  in serenity. Discord renders both the same, and no production embed sets a timestamp.
- 2026-11-10 (lead decision, d): a mashup player with team None (reachable only by a hand-edited DB: src/mashup.py:287 and
  :336-342 always set a team, and migration 16 at src/db/store.py:243-252 numbered old NULL teams) gives an error in Rust,
  where Python renders None as a team.
Anything else found in step 4 is a difference to report to the user.

### Unacceptable differences
Anything a user or caller sees or branches on: posted text, DB rows, exit codes, error text (except the regex
detail text, see Acceptable differences), accepted inputs
(a port must not silently reject what Python handled), ordering a user sees, command surface.

### Normalizations in the diff script
None listed yet. Each one added later needs a dated line here and the user's approval.

### Not ported, on purpose
Nothing decided yet.

### OPEN items (user decides, nothing here is accepted)
None. All 8 resolved 2026-11-09 (DECISIONS.md 2026-11-09); see the matching sections above.
