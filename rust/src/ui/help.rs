//! Port of src/ui/help.py. Texts are copied from Python by a script, never retyped.
use serenity::all::{Colour, CreateEmbed};

pub const HELP_TOPICS: [(&str, &str, &str); 6] = [
    ("start", "Getting started", "1. Run `/handle link <handle>`. The bot gives you a Codeforces problem; submit a compile error to it within 5 minutes, then press Verify. This proves the handle is yours.\n2. Everyone starts at a match rating of 800 in each server. Matches and contests move it.\n3. Try `/gimme` for a problem, `/daily` for double rating once a day, or `/match solo` to practice.\n4. `/handle show` shows a profile, `/handle list` ranks linked members by Codeforces rating and `/handle unlink` removes your link."),
    ("problems", "Problems", "`/gimme [rating] [min_rating] [max_rating] [tags] [exclude_tags] [contest_id]` gives a random problem you have not solved. With no rating it aims at your match rating.\n`/daily [rating]` gives one problem a day (new at 00:00 UTC). Solve it on Codeforces, then run `/daily` again to claim double rating. Your daily streak counts every day you claim the daily, solve a match or solve in a contest (only the daily and your first solo win of the day pay double). Missing one day keeps the streak; two days in a row reset it.\n`/stalk [handle] [member]` lists problems someone tried but never solved."),
    ("matches", "Matches", "`/match solo` practice alone: solving raises your rating, giving up never lowers it.\n`/match duel [opponent]` 1v1: the faster solver wins rating from the other. A finished duel has a Rematch button.\n`/match custom <players>` 2 to 10 players: the first to solve wins, everyone else loses together.\nA match opens a lobby with Join, Start and Leave buttons. After Start the problem appears with Check (look for your solve now) and Give Up. Solve on Codeforces, then press Check.\n`tags` and `exclude_tags` limit the problem. You can be in one unfinished match or contest per server. `/ff` leaves or gives up whichever one you are stuck in."),
    ("contests", "Timed contests", "`/host contest` makes a timed contest of several problems with a live scoreboard. It changes match ratings unless you set `rated` to False.\nOptions: `min_rating`, `max_rating`, `count` (up to 10), `hour` and `minute` (length), `penalty` (minutes per wrong attempt), `teams`, `players` (per team), `tags`, `exclude_tags`, `rated`.\n`/host virtual <contest_id>` replays a finished Codeforces contest with its problems and length (it also has `rated`).\nPress Join in the lobby (2 hours), the host presses Start. Solve on Codeforces; the scoreboard updates, Check reads you at once, Give Up leaves. A finished contest has a Rematch button. The host pressing Start opens a thread on the message that logs every solve; the newest thread message shows the standings, and a closing message ends it.\n`/host bingo [size] [mode] [win]` is a bingo: teams race for the squares of one shared size x size board (3 to 7, default 5), solved on Codeforces after Start. The team that solves a problem first claims its square. With `win: line` (default) the first team to hold a full row, column or diagonal wins; `win: continuous` runs to the end and ranks by lines. `mode: classic` (default) keeps a claimed square for good. `mode: replace` gives a claimed square a new problem rated 100 higher (never above `max_rating`), and another team that solves it takes the square over. It also has `min_rating`, `max_rating`, `hour`, `minute`, `teams`, `players`, `tags`, `exclude_tags` and `rated`. The message lists who solved each square; the thread opened at Start logs every claim and the newest message shows the board."),
    ("ratings", "Ratings and stats", "`/leaderboard [sort] [kind]` ranks everyone by match rating, with pages; `sort: streak` ranks by daily streak; `kind` counts matches or contests only.\n`/profile [member] [handle]` shows one member's rating, rank, games, contest wins and streak.\n`/match history [member] [kind]` lists finished games with your place; with a member it also shows your record against them.\n`/plot rating` graphs match rating, `/plot codeforces` graphs Codeforces rating. Both can compare up to 5 people."),
    ("contest-list", "Codeforces contests", "`/contests upcoming`, `/contests running` and `/contests recent` list Codeforces contests.\n`/ranklist <contest_id>` shows how this server's linked members placed.\nAdmins (Manage Server) can set channels: `/channel reminder` posts a reminder before each contest with an Add to calendar button (optional role ping, minutes, and an `exclude` regex on the name), and `/channel recap` posts how linked members placed after it ends, and `/channel leaderboard` posts the leaderboard every Sunday 00:00 UTC, and `/channel new-contest` announces each new contest (checked every 30 minutes) and again 48 hours before it starts, with an Add to calendar button (sends an .ics file) and a Notify me button (pings you in the reminder). Set each one you want."),
];

pub const OVERVIEW_INTRO: &str = "LightningCCP is a Codeforces bot: link your handle, practice, play matches and contests against other members and climb a server rating.\nNew here? Start with `/help topic:start`.\n\n";

/// Python `OVERVIEW`: the intro, then one `/help topic:<key> <title>` line per topic.
pub fn overview() -> String {
    let lines: Vec<String> = HELP_TOPICS
        .iter()
        .map(|(key, title, _)| format!("`/help topic:{key}` {title}"))
        .collect();
    format!("{OVERVIEW_INTRO}{}", lines.join("\n"))
}

/// Python `help_embed`: a known topic, else the overview. Both blue.
pub fn help_embed(topic: Option<&str>) -> CreateEmbed {
    let (title, description) = match HELP_TOPICS.iter().find(|(key, ..)| Some(*key) == topic) {
        Some((_, title, body)) => ((*title).to_string(), (*body).to_string()),
        None => ("LightningCCP help".to_string(), overview()),
    };
    CreateEmbed::new().title(title).description(description).colour(Colour::BLUE)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a(s: &str) -> u64 {
        s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
    }

    /// (topic, byte length, FNV-1a 64) of "title\ndescription\ncolour" printed by running src/ui/help.py.
    const PYTHON: [(Option<&str>, usize, u64); 8] = [
        (Some("start"), 516, 0xce5421e8013cb4c0),
        (Some("problems"), 610, 0x83ca6860b413a181),
        (Some("matches"), 651, 0x954aa428f9f51026),
        (Some("contests"), 1621, 0x77ee2e2fcc6f482),
        (Some("ratings"), 509, 0xa437110608ed490a),
        (Some("contest-list"), 772, 0x91c39dcac3706b15),
        (None, 428, 0x9688079a7e197f4d),
        (Some("nope"), 428, 0x9688079a7e197f4d),
    ];

    fn json(topic: Option<&str>) -> serde_json::Value {
        serde_json::to_value(help_embed(topic)).unwrap()
    }

    #[test]
    fn every_topic_and_the_overview_match_python_bytes() {
        for (topic, len, hash) in PYTHON {
            let v = json(topic);
            let s = format!("{}\n{}\n{}", v["title"].as_str().unwrap(), v["description"].as_str().unwrap(), v["color"]);
            assert_eq!((s.len(), fnv1a(&s)), (len, hash), "topic {topic:?}");
        }
    }

    #[test]
    fn unknown_topic_falls_back_to_the_overview() {
        assert_eq!(json(Some("nope")), json(None));
        assert_eq!(json(None)["title"], "LightningCCP help");
    }
}
