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
| `/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags] [contest_id]` | Random problem, hides ones you solved; `contest_id` limits it to one contest; with no rating arguments and no contest it aims at your match rating |
| `/match solo [rating] [tags] [exclude_tags]` | Practice alone; solving raises your match rating |
| `/match duel [opponent] [rating] [tags] [exclude_tags]` | 1v1 duel; name an opponent or let anyone join |
| `/match custom players [rating] [tags] [exclude_tags]` | Match for 2-10 players; ranked by solve time |
| `/match history [member]` | Your last 100 finished matches and your last 50 finished mashups, newest first, with your place; with a member, also your record and matches against them (private) |
| `/mashup contest [min_rating] [max_rating] [count] [hour] [minute] [penalty] [players]` | Timed contest of several problems (default 5 problems, 800-1500, 2h, no penalty, up to 20 players) with a live scoreboard |
| `/mashup team-contest [teams] [min_rating] [max_rating] [count] [hour] [minute] [penalty]` | The same contest for teams (`teams` 2-10, default 2; the host starts in team 1). Join opens a form asking for a team number |
| `/stats [sort]` | Top 10 by match rating (`sort: streak` ranks by daily streak instead), plus your own rank; a fire emoji and number marks a daily streak (private) |
| `/plot rating [member] [handle]` | Graph of a member's match rating in this server (private; default: you) |
| `/plot codeforces [member] [handle]` | Graph of a Codeforces rating history, any handle works (private; default: you) |
| `/contests upcoming\|running\|recent` | Paginated contest lists |
| `/daily [rating]` | Your daily problem, rated your match rating floored to 100 (or the `rating` you pick; picking a different rating than today's unsolved daily swaps in a new problem); solve it on Codeforces, run `/daily` again to claim double rating. New problem every 00:00 UTC; shows your daily streak |
| `/stalk [handle] [member]` | Problems someone tried but never solved (private; default: you) |
| `/ranklist contest_id` | Official standings of this server's linked members in a contest (private) |
| `/channel set\|show\|clear` | The server's default bot channel (needs Manage Server). `set` takes the channel, an optional role to ping and the minutes before a contest to remind (default 10,60). The bot posts contest reminders there, and after each contest ends a recap of how your linked members placed (top 10) |

A match is a public message. `tags` and `exclude_tags` work like in `/gimme` and limit the problem the match picks (the lobby shows them, and a rematch keeps them). The lobby has Join, Start and Leave buttons; once the host presses Start, the problem appears with Check and Give Up buttons. A finished duel shows a Rematch button: either player can press it to open a new duel lobby against the same opponent, who then presses Join (the old rating argument is reused). Everyone starts at a match rating of 800. With no `rating` argument, `/gimme` and `/match` pick a problem at your match rating (rounded to 100; the players' average for duel and custom). After a match, ratings change. Solo only counts solving, so giving up or running out of time never lowers your rating: you gain +1 when the problem is 400 or more below your rating, +10 when equal and +33 when 400 or more above (linear in between). Duel and custom compare every pair of players by solve order, and the winner gains a fixed step by how far the loser is rated from them: +16 when the loser is 400 or more below, +33 from 200 below, +49 within 200 of each other, +67 from 200 above, +95 from 400 above. The loser loses the same, and a tie moves nothing. Custom matches average these results over the other players.

The daily bonus doubles one rating change per UTC day: either the `/daily` problem, or your first solved solo match of the day (which then counts as that day's daily; an unsolved solo match does not use it up). Each claimed day adds one to your daily streak. Missing one day keeps the streak; missing two days in a row resets it to zero.

A mashup contest opens a lobby with Join, Start and Leave buttons (the lobby lasts 10 minutes; if the host leaves it is cancelled). When the host presses Start, the bot picks `count` problems spread evenly (easy to hard) across the rating range that nobody in the lobby has solved, and nobody can join any more. Players solve on Codeforces; every minute the bot reads submissions made between the start and the end (and on Check) and updates the scoreboard, which ranks by problems solved, then total time: minutes from the start for each solve plus `penalty` minutes for every wrong attempt before it (compilation errors are free; default penalty 0). The message turns into the final standings when time runs out, and the contest changes match ratings the way a custom match does: every pair of players is a duel decided by the standings (equal standings tie), and a player's change is the average over the others. In a team contest each team counts as one player rated at its members' average, and every member gets the team's change. A contest with a single player or team does not change ratings. A user can be in only one unfinished match or contest per server at a time. In a team contest a problem counts for a team once, from the teammate who solved it first (their time and their wrong attempts), and the standings rank teams by problems solved, then total time. To change team, Leave and Join again.

## Development

```
uv run pytest
uv run ruff check . && uv run ruff format --check .
```
