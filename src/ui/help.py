import discord

HELP_TOPICS: dict[str, tuple[str, str]] = {
    "start": (
        "Getting started",
        "1. Run `/handle link <handle>`. The bot gives you a Codeforces problem; submit a "
        "compile error to it within 5 minutes, then press Verify. This proves the handle is yours.\n"
        "2. Everyone starts at a match rating of 800 in each server. Matches and contests move it.\n"
        "3. Try `/gimme` for a problem, `/daily` for double rating once a day, or "
        "`/match solo` to practice.\n"
        "4. `/handle show` shows a profile, `/handle list` ranks linked members by Codeforces rating "
        "and `/handle unlink` removes your link.",
    ),
    "problems": (
        "Problems",
        "`/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags] [contest_id]` gives a "
        "random problem you have not solved. With no rating it aims at your match rating.\n"
        "`/daily [rating]` gives one problem a day (new at 00:00 UTC). Solve it on Codeforces, "
        "then run `/daily` again to claim double rating and raise your daily streak. "
        "Missing one day keeps the streak; two days in a row reset it.\n"
        "`/stalk [handle] [member]` lists problems someone tried but never solved.",
    ),
    "matches": (
        "Matches",
        "`/match solo` practice alone: solving raises your rating, giving up never lowers it.\n"
        "`/match duel [opponent]` 1v1: the faster solver wins rating from the other. "
        "A finished duel has a Rematch button.\n"
        "`/match custom <players>` 2 to 10 players: the first to solve wins, everyone else "
        "loses together.\n"
        "A match opens a lobby with Join, Start and Leave buttons. After Start the problem "
        "appears with Check (look for your solve now) and Give Up. Solve on Codeforces, then "
        "press Check.\n"
        "`tags` and `exclude_tags` limit the problem. You can be in one unfinished match or "
        "contest per server. `/ff` leaves or gives up whichever one you are stuck in.",
    ),
    "contests": (
        "Timed contests",
        "`/host contest` makes a timed contest of several problems with a live scoreboard. "
        "It changes match ratings unless you set `rated` to False.\n"
        "Options: `min_rating`, `max_rating`, `count` (up to 10), `hour` and `minute` (length), "
        "`penalty` (minutes per wrong attempt), `teams`, `players` (per team), `tags`, "
        "`exclude_tags`, `rated`.\n"
        "`/host virtual <contest_id>` replays a finished Codeforces contest with its problems "
        "and length (it also has `rated`).\n"
        "Press Join in the lobby (2 hours), the host presses Start. Solve on Codeforces; "
        "the scoreboard updates, Check reads you at once, Give Up leaves. A finished contest "
        "has a Rematch button. The host pressing Start opens a thread on the message that logs every solve; the "
        "newest thread message shows the standings, and a closing message ends it.\n"
        "`/host bingo [size] [mode] [win]` is a bingo: teams race for the squares of one shared "
        "size x size board (3 to 7, default 5), solved on Codeforces after Start. The team that "
        "solves a problem first claims its square. With `win: line` (default) the first team to "
        "hold a full row, column or diagonal wins; `win: continuous` runs to the end and ranks by "
        "lines. `mode: classic` (default) keeps a claimed square for good. `mode: replace` gives "
        "a claimed square a new problem rated 100 higher (never above `max_rating`), and another team that solves it takes "
        "the square over. It also has `min_rating`, `max_rating`, `hour`, `minute`, `teams`, "
        "`players`, `tags`, `exclude_tags` and `rated`. The message lists who solved each square; the thread opened at Start logs every claim and the newest message shows the board.",
    ),
    "ratings": (
        "Ratings and stats",
        "`/stats [sort] [kind]` ranks everyone by match rating, with pages; `sort: streak` ranks "
        "by daily streak; `kind` counts matches or contests only.\n"
        "`/profile [member] [handle]` shows one member's rating, rank, games, contest wins and streak.\n"
        "`/match history [member] [kind]` lists finished games with your place; with a member "
        "it also shows your record against them.\n"
        "`/plot rating` graphs match rating, `/plot codeforces` graphs Codeforces rating. "
        "Both can compare up to 5 people.",
    ),
    "contest-list": (
        "Codeforces contests",
        "`/contests upcoming`, `/contests running` and `/contests recent` list Codeforces "
        "contests.\n`/ranklist <contest_id>` shows how this server's linked members placed.\n"
        "Admins (Manage Server) can set channels: `/channel reminder` posts a reminder before "
        "each contest (optional role ping, minutes, and an `exclude` regex on the name), and "
        "`/channel recap` posts how linked members placed after it ends. Set each one you want.",
    ),
}

OVERVIEW = (
    "LightningCCP is a Codeforces bot: link your handle, practice, play matches and contests "
    "against other members and climb a server rating.\n"
    "New here? Start with `/help topic:start`.\n\n"
    + "\n".join(f"`/help topic:{key}` {title}" for key, (title, _) in HELP_TOPICS.items())
)


def help_embed(topic: str | None) -> discord.Embed:
    if topic in HELP_TOPICS:
        title, body = HELP_TOPICS[topic]
        return discord.Embed(title=title, description=body, colour=discord.Colour.blue())
    return discord.Embed(
        title="LightningCCP help", description=OVERVIEW, colour=discord.Colour.blue()
    )
