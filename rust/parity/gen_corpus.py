"""Writes rust/parity/corpus/<domain>.jsonl: deterministic (seeded) cases {"id","fn","args"} aimed at the
trap edges of the Python-to-Rust port (see SKILLS/porting-to-rust/TRAPS.md).
Run: .venv/bin/python -I rust/parity/gen_corpus.py
Add a domain: write `def gen_<name>(c, rng)` that calls `c.add(fn, *args)`, then list it in DOMAINS.
The function itself must exist in both runners (run_py.py FNS, src/bin/parity.rs `call`)."""

import json
import random
import string
import sys
from pathlib import Path

SEED = 20261109
OUT = Path(__file__).parent / "corpus"


class Corpus:
    def __init__(self, domain):
        self.domain, self.rows = domain, []

    def add(self, fn, *args):
        self.rows.append({"id": f"{self.domain}-{len(self.rows)}", "fn": fn, "args": list(args)})


# ---------------------------------------------------------------- strings
PIECES = [
    "",
    " ",
    "  ",
    "a",
    "Z",
    "0",
    "_",
    "__",
    "*",
    "**",
    "~",
    "~~",
    "|",
    "||",
    "`",
    "``",
    "```",
    "\\",
    "\\\\",
    "[",
    "]",
    "[a](b)",
    "[x]",
    "(y)",
    "> ",
    ">>> ",
    ">",
    "# ",
    "## ",
    "### ",
    "#### ",
    "- ",
    "-",
    " - ",
    "\n",
    "\n\n",
    "\r\n",
    "\t",
    "<https://example.com/a_b>",
    "<a:b>",
    "<mailto:x>",
    "https://a.b/c_d_e.",
    "https://a.b/c_(d)",
    "http://x.y/z*",
    "steam://abc_d",
    "https://",
    "http://a b",
    "https://x.y/z?q=1&r=_",
    "<https://x y>",
    "<x:/y>",
    "<x:/y z>",
    'https://a.b/"c"',
    "https://a.b/c,",
    "Codeforces Round",
    "Div. 2",
    "тест",
    "Привет",
    "שלום",
    "עברית",
    "مرحبا",
    "العربية",
    "日本語",
    "한국어",
    "ภาษาไทย",
    "देवनागरी",
    "😀",
    "👨\u200d👩\u200d👧\u200d👦",
    "🇮🇳",
    "👍🏽",
    "\u0301",
    "e\u0301",
    "a\u0308\u0323",
    "\u0e31",
    "\u200b",
    "\u200c",
    "\u200d",
    "\ufeff",
    "\u2060",
    "\u202e",
    "\u202a",
    "\u200f",
    "\x1c",
    "\x1d",
    "\x1e",
    "\x1f",
    "\x85",
    "\xa0",
    "\u2028",
    "\u2029",
    "\u3000",
    "\u1680",
    "\u180e",
    "\x00",
    "\x0b",
    "\x0c",
    "ß",
    "ẞ",
    "ς",
    "Σ",
    "ΑΣ",
    "İ",
    "ı",
    "ǅ",
    "ǆ",
    "ﬁ",
    "ﬃ",
    "ŉ",
    "ΐ",
    "ᾳ",
    "Å",
    "Å",
    "K",
    "µ",
    "ꙋ",
    "Ⅷ",
    "ⓐ",
    "Ａ",
    "ａ１",
    "٣",
    "١٢٣",
    "①",
    "²",
    "½",
    "\U0001d7ce",
    "\U0001f600",
    "\U0010ffff",
    "\U00010400",
    "\U00010428",
]
CODEPOINT_RANGES = [
    (0, 0x7F),
    (0x80, 0x24F),
    (0x300, 0x36F),
    (0x370, 0x5FF),
    (0x600, 0x6FF),
    (0xE00, 0xE7F),
    (0x2000, 0x206F),
    (0x3040, 0x30FF),
    (0x4E00, 0x4F00),
    (0xFF00, 0xFFEF),
    (0x10000, 0x1F9FF),
    (0xE0000, 0xE007F),
]


def rand_char(rng):
    lo, hi = rng.choice(CODEPOINT_RANGES)
    cp = rng.randint(lo, hi)
    return "?" if 0xD800 <= cp <= 0xDFFF else chr(cp)


def rand_str(rng, max_pieces=8):
    parts = []
    for _ in range(rng.randint(0, max_pieces)):
        parts.append(rng.choice(PIECES) if rng.random() < 0.8 else rand_char(rng))
    return "".join(parts)


def long_str(rng, n):
    out, size = [], 0
    while size < n:
        p = rng.choice(PIECES) or "x"
        out.append(p)
        size += len(p)
    return "".join(out)[:n]


def gen_text(c, rng):
    for p in PIECES:
        c.add("esc", p)
        c.add("casefold", p)
    for _ in range(900):
        s = rand_str(rng)
        c.add("esc", s)
    for _ in range(600):
        c.add("casefold", rand_str(rng))
    for n in (100, 101, 1999, 2000, 5000, 20000):
        for _ in range(3):
            c.add("esc", long_str(rng, n))
        c.add("casefold", long_str(rng, n))
    for ch in range(0x20, 0x250):  # every Latin letter and symbol once
        c.add("casefold", chr(ch))
    for ch in range(0x370, 0x600):
        c.add("casefold", chr(ch))
        c.add("casefold", "Σ" + chr(ch) + "Σ")
    for _ in range(40):
        c.add(
            "esc",
            "x"
            + rng.choice(["https://a.b/", "http://", "<https://", "[t](", "> ", ">>> ", "# ", "- "])
            + rand_str(rng, 3),
        )


# ---------------------------------------------------------------- ints and floats
def gen_ints(c, rng):
    digit_sets = [
        "0123456789",
        "٠١٢٣٤٥٦٧٨٩",
        "０１２３４５６７８９",
        "०१२३४५६७८९",
        "๐๑๒๓๔๕๖๗๘๙",
        "𝟎𝟏𝟐𝟑𝟒𝟓𝟔𝟕𝟖𝟗",
    ]
    ws = [
        "",
        " ",
        "  ",
        "\t",
        "\n",
        "\xa0",
        "\u2003",
        "\u3000",
        "\x1c",
        "\x1f",
        "\x85",
        "\u2028",
        "\u200b",
        "\ufeff",
    ]
    fixed = [
        "",
        "-",
        "+",
        "+-1",
        "--1",
        "1_000",
        "1__0",
        "_1",
        "1_",
        "-_1",
        "1.0",
        "1e3",
        "0x10",
        "0b1",
        "٣_٤",
        "１２",
        "12a",
        "a12",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551616",
        "1" * 4300,
        "1" * 4301,
        "0" * 4300,
        "0" * 4301 + "1",
        "1" * 400,
        "0" * 100 + "5",
        "00",
        "-0",
        "+0",
        "²",
        "½",
        "①",
        "\u0660\u0661",
    ]
    for s in fixed:
        for fn in ("py_int", "parse_digits", "parse_minutes"):
            c.add(fn, s)
    for _ in range(1500):
        ds = rng.choice(digit_sets)
        body = "".join(rng.choice(ds) for _ in range(rng.choice([1, 1, 2, 3, 5, 12, 19, 20, 40])))
        if rng.random() < 0.15:
            body = body[: len(body) // 2] + "_" + body[len(body) // 2 :]
        if rng.random() < 0.1:
            body += rng.choice(string.ascii_letters + "٫.,")
        s = rng.choice(ws) + rng.choice(["", "", "-", "+"]) + body + rng.choice(ws)
        c.add("py_int", s)
        c.add("parse_digits", s)
    for _ in range(800):
        parts = []
        for _ in range(rng.choice([1, 1, 2, 3, 5, 6, 7])):
            v = rng.choice(
                [1, 5, 10, 60, 1440, 10080, 10081, 0, -3, 99999999999, rng.randint(-5, 10090)]
            )
            parts.append(rng.choice(ws) + str(v) + rng.choice(ws))
        raw = rng.choice([",", ", ", ",,"]).join(parts) if rng.random() < 0.9 else rand_str(rng, 3)
        c.add("parse_minutes", raw)
    for raw in [
        "",
        ",",
        "1,",
        ",1",
        "1,1,1",
        "5,4,3,2,1",
        "6,5,4,3,2,1",
        "10,60",
        "60,10",
        "٣,٤",
        "1,٢",
    ]:
        c.add("parse_minutes", raw)
    floats = [
        0,
        -0.0,
        0.0,
        1,
        -1,
        0.5,
        1.5,
        2.5,
        0.1,
        100000,
        999999,
        1e6,
        1234567,
        0.0001,
        0.00001,
        1e-5,
        123456.5,
        12345.65,
        1e16,
        1e22,
        1e100,
        1.7976931348623157e308,
        5e-324,
        2.5e-5,
        99999.95,
        999999.5,
        0.30000000000000004,
        1 / 3,
        2 / 3,
        1e5,
        100000.5,
        123456789.123,
    ]
    # floats travel as repr strings: serde_json's default float parser can be 1 ULP off, which is not what is tested
    fl = lambda x: repr(float(x))  # noqa: E731
    for x in floats:
        c.add("fmt_g", fl(x))
        c.add("fmt_g", fl(-x))
    for s in ("nan", "inf", "-inf"):
        c.add("fmt_g", s)
    for _ in range(1500):
        x = rng.choice(
            [
                rng.random(),
                rng.uniform(-1e6, 1e6),
                rng.randint(-(10**7), 10**7) / rng.choice([1, 2, 4, 8, 10, 100, 1000]),
                rng.uniform(-1, 1) * 10 ** rng.randint(-8, 25),
            ]
        )
        c.add("fmt_g", fl(x))
    for _ in range(300):  # exact halves around 6 significant digits
        base = rng.randint(100000, 999999)
        c.add("fmt_g", fl(base + 0.5))
        c.add("fmt_g", fl(base / 10 + 0.05))


# ---------------------------------------------------------------- contests
NAMES = [
    "Codeforces Round 900 (Div. 2)",
    "Educational Codeforces Round 150 (Rated for Div. 2)",
    "Kotlin Heroes: Episode 9",
    "CodeTON Round 5 (Div. 1 + Div. 2)",
    "Тест раунд",
    "İstanbul Contest",
    "ǅ Round",
    "Hello 2024",
    "Good Bye 2023: 2024 is NEAR",
    "Codeforces Round #1 (Div. 1)",
    "ß straße; a, b \\ c",
    "[Div3] a_b*c",
    "Line\nbreak\r\nname",
    "😀 emoji 👨\u200d👩\u200d👧 round",
    "e\u0301cole",
    "ΑΣ",
    "",
    "x" * 150,
    "Codeforces Global Round 24",
    "日本語コンテスト",
]
PHASES = [
    "BEFORE",
    "CODING",
    "PENDING_SYSTEM_TEST",
    "SYSTEM_TEST",
    "FINISHED",
    "FINISHED",
    "BEFORE",
    "",
    "before",
    "UNKNOWN",
]
NOW = 1_700_000_000


def rand_contest(rng, cid=None, now=NOW):
    phase = rng.choice(PHASES)
    start = rng.choice(
        [
            None,
            0,
            -86400,
            now - rng.randint(0, 3 * 86400),
            now + rng.randint(-100, 5 * 86400),
            now + rng.randint(0, 600),
            now,
        ]
    )
    if rng.random() < 0.8 and start is None:
        start = now + rng.randint(-86400, 86400 * 3)
    return {
        "id": cid if cid is not None else rng.randint(1, 3000),
        "name": rng.choice(NAMES) if rng.random() < 0.7 else rand_str(rng),
        "type": rng.choice(["CF", "ICPC", "IOI", ""]),
        "phase": phase,
        "start_time": start,
        "duration": rng.choice(
            [0, 3600, 7200, 9000, 10800, 86400, 7 * 86400, -60, rng.randint(0, 20000)]
        ),
    }


def contest_list(rng, now=NOW, n=None):
    n = rng.randint(0, 9) if n is None else n
    return [rand_contest(rng, rng.randint(1, 40), now) for _ in range(n)]


def boundary_contest(rng, now, mins, phase="BEFORE"):
    """A contest whose start sits exactly on, one second before or after a reminder threshold."""
    b = rng.choice(mins) if mins else 10
    start = now + b * 60 + rng.choice([-1, 0, 1, 0])
    return {
        "id": rng.randint(1, 30),
        "name": rng.choice(NAMES),
        "type": "CF",
        "phase": phase,
        "start_time": start,
        "duration": 7200,
    }


def gen_contests(c, rng):
    for _ in range(400):
        c.add(
            "select_contests",
            contest_list(rng),
            rng.choice(["upcoming", "running", "recent", "recent", "bogus", ""]),
            rng.choice([0, 1, 3, 50, 100]),
        )
        c.add("refresh_minutes", contest_list(rng))
    for _ in range(600):
        now = NOW + rng.choice([0, 1, -1, 86400, -86400, 86400 * 100])
        mins = [rng.choice([1, 10, 60, 120, 1440, 10080, 0, -5]) for _ in range(rng.randint(0, 5))]
        cs = [
            boundary_contest(rng, now, mins, rng.choice(["BEFORE", "BEFORE", "CODING"]))
            for _ in range(rng.randint(0, 4))
        ] + contest_list(rng, now, rng.randint(0, 3))
        sent = [[ct["id"], rng.choice(mins) if mins else 10] for ct in cs if rng.random() < 0.4]
        c.add("reminders_due", cs, mins, sent, now)
    for _ in range(400):
        now = NOW + rng.choice([0, 1, -1, 172800, -172800])
        cs = contest_list(rng, now)
        for _ in range(rng.randint(0, 2)):  # on the edge of the 48 h window and of "has started"
            cs.append(
                {
                    "id": rng.randint(1, 40),
                    "name": "edge",
                    "type": "CF",
                    "phase": "BEFORE",
                    "duration": 1,
                    "start_time": now + rng.choice([0, 1, -1, 172800, 172801, 172799]),
                }
            )
        done = [ct["id"] for ct in cs if rng.random() < 0.3]
        c.add("new_contests", cs, done, now)
        c.add("soon_contests", cs, done, now, rng.choice([172800, 0, 1, 3600, -5]))
    for _ in range(500):
        now = NOW + rng.choice([0, 1, -1, 86400])
        cs = contest_list(rng, now)
        for _ in range(rng.randint(0, 3)):  # ended exactly 0, 1 and 86400, 86401 seconds ago
            dur = rng.choice([0, 3600, 7200])
            cs.append(
                {
                    "id": rng.randint(1, 40),
                    "name": "edge",
                    "type": "CF",
                    "phase": rng.choice(["FINISHED", "FINISHED", "BEFORE"]),
                    "duration": dur,
                    "start_time": now - dur - rng.choice([0, 1, -1, 86400, 86401, 86399, -86400]),
                }
            )
        c.add("recaps_due", cs, [ct["id"] for ct in cs if rng.random() < 0.3], now)
    for _ in range(500):  # ICS: escaping, timestamp range, alarm text
        ct = rand_contest(rng)
        ct["start_time"] = rng.choice(
            [
                ct["start_time"],
                0,
                -1,
                1,
                951782400,
                1_700_000_000,
                4102444800,
                253402300799,
                -62135596800,
                -62135596801,
                10**12,
            ]
        )
        ct["duration"] = rng.choice([ct["duration"], 0, 1, 253402300799, -10, 86400 * 365])
        c.add(
            "contest_ics",
            ct,
            rng.choice([NOW, 0, -1, 253402300799, 253402300800, 10**10]),
            rng.choice([10, 0, 1, 15, 60, -5, 99999999]),
        )
    for nm in NAMES + [";", ",", "\\", ";,\\\n\r", "a\u2028b", "\x00"]:
        c.add(
            "contest_ics",
            {
                "id": 1,
                "name": nm,
                "type": "CF",
                "phase": "BEFORE",
                "start_time": NOW,
                "duration": 7200,
            },
            NOW,
            10,
        )


# ---------------------------------------------------------------- regex
REGEX_FIXED = [
    "",
    " ",
    "a",
    "abc",
    "Div\\. [123]",
    "^Educational",
    "(Div\\. 1|Div\\. 2)",
    "round \\d+",
    "(?i)global",
    "(?s).",
    "(?m)^a",
    "(?x) a b",
    "(?a)\\w",
    "(?u)\\w",
    "(?i:a)b",
    "(?-i:a)b",
    "(?s-i:a)",
    "(?i)(?-i:a)",
    "(?a-u:x)",
    "(?P<n>a)(?P=n)",
    "(?P<n>a)(?P<n>b)",
    "(?P<1>a)",
    "(?P<n",
    "(?P=n)",
    "(?P>n)",
    "(a)\\1",
    "\\1",
    "(a)\\2",
    "\\0",
    "\\01",
    "\\011",
    "\\400",
    "\\8",
    "(?=a)b",
    "(?!a)b",
    "(?<=a)b",
    "(?<!a)b",
    "(?<=a+)b",
    "(?<=a|bc)d",
    "(?<!a*)",
    "(?>a+)b",
    "a++",
    "a*+",
    "a?+",
    "a{2}+",
    "a++b",
    "(?#comment)a",
    "(?#",
    "(a)(?(1)b|c)",
    "(?(1)b)",
    "(?(2)b|c)",
    "(?(x)a)",
    "(a|b|c)(?(1)d|e|f)",
    "[",
    "]",
    "[a",
    "[]",
    "[]a]",
    "[^]a]",
    "[a-z]",
    "[z-a]",
    "[a-]",
    "[-a]",
    "[a-b-c]",
    "[\\d-z]",
    "[\\w-a]",
    "[[:alpha:]]",
    "[a&&b]",
    "[a--b]",
    "[a||b]",
    "[a~~b]",
    "[\\q]",
    "\\q",
    "\\",
    "a\\",
    "\\x",
    "\\x4",
    "\\x41",
    "\\u0041",
    "\\u41",
    "\\U00000041",
    "\\U0001F600",
    "\\U00110000",
    "\\N{DIGIT ONE}",
    "\\N{bogus}",
    "\\N",
    "\\p{L}",
    "\\pL",
    "\\P{L}",
    "\\h",
    "\\R",
    "\\X",
    "\\K",
    "\\G",
    "\\z",
    "\\Z",
    "\\A",
    "\\b",
    "\\B",
    "\\d\\D\\s\\S\\w\\W",
    "\\a\\f\\n\\r\\t\\v",
    "\\e",
    "\\cA",
    "(",
    ")",
    "()",
    "(()",
    "())",
    "a)",
    "(?",
    "(?:",
    "(?:a",
    "(?'n'a)",
    "(?<n>a)",
    "(?<n>a)\\k<n>",
    "(?|a|b)",
    "(*)",
    "(?R)",
    "(?1)",
    "(?&n)",
    "(?~a)",
    "*",
    "+",
    "?",
    "a**",
    "a*?",
    "a+?",
    "a??",
    "a{",
    "a{1",
    "a{1,",
    "a{,2}",
    "a{1,2",
    "a{2,1}",
    "a{0}",
    "a{0,0}",
    "a{1}{2}",
    "{1}",
    "{",
    "}",
    "a{x}",
    "a{ 1}",
    "a{4294967295}",
    "a{4294967296}",
    "a{99999999999}",
    "a{1,4294967296}",
    "a{4294967294,4294967295}",
    "(a{65535}){65535}",
    "(?:a{1000}){1000}",
    "(a*)*",
    "(a*)+",
    "(a+)*",
    "(a|)*",
    "(|a)+",
    "()*",
    "(?:)*",
    "^*",
    "$*",
    "\\b*",
    "(?=a)*",
    "(?!a)+",
    "^+",
    "(?:^)*",
    "a|",
    "|a",
    "|",
    "||",
    "a||b",
    "(|)",
    "(a|)",
    "(?i)a(?i)",
    "a(?i)",
    "(?i)",
    "(?i",
    "(?z)",
    "(?iz)",
    "(?ii)",
    "(?-)",
    "(?-i)",
    "(?i-)",
    "(?ai)",
    "(?au)",
    "(?aL)",
    "(?L)a",
    "(?i:",
    "(?i:)",
    "(?ims:a)",
    "(?x)#comment\na",
    "(?x)[ ]",
    "(?x)a #b",
    "(?x: a b )",
    "a # b",
    "(?x) a # (",
    "É",
    "é",
    "[é-ÿ]",
    "ß",
    "ǅ",
    "[Ǆ-ǆ]",
    "ı",
    "İ",
    "K",
    "ς",
    "σ",
    "[ς]",
    "ſ",
    "[a-ſ]",
    "\\w+",
    "\\d",
    "[\\d]",
    "٣",
    "\\s",
    "\\S",
    "\\x1c",
    "[\\x1c-\\x1f]",
    " \t",
    ".",
    ".+",
    ".*",
    "^.*$",
    "$",
    "^",
    "^$",
    "a$",
    "\\Aa",
    "a\\Z",
    "(?m)a$",
    "\\n",
    "a\nb",
    "\\Q",
    "\\E",
    "\\Qa\\E",
    "[[]",
    "[]]",
    "[^]",
    "[\\]]",
    "[\\[]",
    "[a\\",
    "[\\",
    "[[a]",
    "[a[]",
    "[a-\\d]",
    "[\\d-\\d]",
    "[a-a]",
    "[\\x00-\\xff]",
    "[\\u0000-\\uffff]",
    "(?P<é>a)",
    "(?P<a-b>a)",
    "(?P<a b>a)",
    "(?P<>a)",
    "(?P<_>a)",
    "(?P<n>a)(?P=m)",
    "(?P<n>a)(?(n)b|c)",
    "(?(n)b|c)",
    "a" * 100,
    "a" * 101,
    "é" * 100,
    "é" * 101,
    "😀" * 100,
    "😀" * 101,
    "(" * 100,
    "(" * 50 + ")" * 50,
    "[" * 100,
    "\\" * 100,
    "(?:" * 60 + "a" + ")" * 60,
    "  a  ",
    "\xa0a\xa0",
    "\x1ca\x1f",
    "\u3000a",
    "\u200ba\u200b",
    "\ufeffa",
    " " * 101,
    " " * 100 + "a",
    "a" + " " * 100,
]
TOKENS = [
    "a",
    "b",
    ".",
    "*",
    "+",
    "?",
    "|",
    "(",
    ")",
    "[",
    "]",
    "{2}",
    "{1,3}",
    "\\d",
    "\\w",
    "\\s",
    "^",
    "$",
    "(?:",
    "(?=",
    "(?!",
    "(?<=",
    "(?<!",
    "\\1",
    "\\2",
    "-",
    "\\b",
    "(?i)",
    "\\Z",
    "{",
    "}",
    ",",
    "\\",
    "(?P<n>",
    "(?P=n)",
    "(?#",
    "(?>",
    "++",
    "*?",
    "[^",
    "[a-z]",
    "x",
    "é",
    "(?x)",
    "(?a)",
    "{0}",
    "{,}",
]
SAFE_TOKENS = [
    "a",
    "b",
    "c",
    ".",
    "^",
    "$",
    "\\d",
    "\\w",
    "\\s",
    "\\b",
    "[a-c]",
    "[^a]",
    "|",
    "x",
    "é",
    "ß",
    "ǅ",
    "ς",
    "K",
    "İ",
    "(?i)",
    "Div",
    "Round",
    "\\.",
    " ",
    "1",
    "2",
    "(?:a|b)",
    "(?=D)",
    "(?!R)",
    "(?<=d)",
    "\\d+",
    "[A-Z]",
    "e\u0301",
    "😀",
    "(a)\\1",
    "\\n",
    "_",
    "\\*",
    "\\[",
    "\\]",
]


def gen_regex(c, rng):
    for p in REGEX_FIXED:
        c.add("check_regex", p)
    for _ in range(1500):
        c.add("check_regex", "".join(rng.choice(TOKENS) for _ in range(rng.randint(1, 8))))
    for _ in range(300):
        c.add("check_regex", rand_str(rng, 6))
    base = {"phase": "FINISHED", "type": "CF", "start_time": 1, "duration": 1}
    cs = [
        dict(base, id=i + 1, name=n) for i, n in enumerate(NAMES[:-1])
    ]  # all but the 150-char name: it makes slow matches pointless
    for p in REGEX_FIXED:
        if len(p) < 200 and p not in (
            "(a*)*",
            "(a*)+",
            "(a+)*",
            "(?:a{1000}){1000}",
            "(a{65535}){65535}",
        ):
            c.add("not_excluded", cs, p, [])
    for _ in range(1000):
        pat = "".join(rng.choice(SAFE_TOKENS) for _ in range(rng.randint(1, 5)))
        c.add("not_excluded", cs, pat, [i + 1 for i in range(len(cs)) if rng.random() < 0.15])
    for nm in (
        "straSSe",
        "STRASSE",
        "ǅ",
        "ǆ",
        "Ǆ",
        "k",
        "K",
        "i̇",
        "İ",
        "ς",
        "σ",
        "Σ",
        "é",
        "É",
        "e\u0301",
        "Å",
        "ꙋ",
        "ᲈ",
    ):
        for pat in (
            "straße",
            "STRASSE",
            "ǆ",
            "k",
            "i",
            "σ",
            "ς",
            "Σ",
            "[σς]",
            "é",
            "\\w",
            "[a-z]",
            "(?i:k)",
            "(?-i:k)",
            "(?a)k",
            "ꙋ",
        ):
            c.add("not_excluded", [dict(base, id=1, name=nm)], pat, [])


# ---------------------------------------------------------------- Codeforces payloads
HANDLES = [
    "tourist",
    "Petr",
    "jiangly",
    "Ünïcode",
    "日本語",
    "e\u0301",
    "😀",
    "İ",
    "ǅ",
    "ẞ",
    "ΑΣ",
    "a;b",
    "",
    " ",
    "a b",
    "x" * 40,
    "A_b-c",
]
RANKS = [
    "newbie",
    "pupil",
    "specialist",
    "expert",
    "candidate master",
    "master",
    "international master",
    "grandmaster",
    "legendary grandmaster",
    "headquarters",
    None,
    "",
]
TAGS = [
    "dp",
    "math",
    "greedy",
    "implementation",
    "special problem",
    "2-sat",
    "*special",
    "Ünï",
    "😀",
]


def u(rng, h=None):
    d = {
        "handle": h if h is not None else rng.choice(HANDLES),
        "rating": rng.randint(0, 4000),
        "maxRating": rng.randint(0, 4000),
        "rank": rng.choice(RANKS),
    }
    for k in ("rating", "maxRating", "rank"):
        if rng.random() < 0.2:
            del d[k]
    if rng.random() < 0.3:
        d.update({"country": "X", "avatar": "https://x"})
    return d


def prob(rng):
    d = {
        "contestId": rng.randint(1, 2000),
        "index": rng.choice(["A", "B", "C1", "C2", "", "Ü", "😀", "A" * 5]),
        "name": rng.choice(NAMES),
        "rating": rng.choice([800, 1500, 3500, 0, None]),
        "tags": rng.sample(TAGS, rng.randint(0, 4)),
        "type": "PROGRAMMING",
        "points": 500.0,
    }
    for k in ("rating", "tags"):
        if rng.random() < 0.25:
            del d[k]
    return d


def contest_cf(rng):
    d = {
        "id": rng.randint(1, 3000),
        "name": rng.choice(NAMES),
        "type": rng.choice(["CF", "ICPC", "IOI"]),
        "phase": rng.choice(PHASES[:5]),
        "frozen": False,
        "durationSeconds": rng.choice([0, 7200, 10800, 86400]),
        "startTimeSeconds": NOW + rng.randint(-(10**6), 10**6),
        "relativeTimeSeconds": -5,
    }
    if rng.random() < 0.2:
        del d["startTimeSeconds"]
    return d


def standing_row(rng, handles=None):
    ms = [{"handle": rng.choice(handles or HANDLES)} for _ in range(rng.randint(1, 3))]
    return {
        "party": {"contestId": 1, "members": ms, "participantType": "CONTESTANT"},
        "rank": rng.randint(1, 5000),
        "points": rng.choice([0.0, 1.0, 1500.5, 3, 0, 2500, 1e3]),
        "penalty": rng.randint(0, 500),
        "successfulHackCount": 0,
    }


def sub_row(rng):
    p = {"contestId": rng.randint(1, 2000), "index": rng.choice(["A", "B", "C1"]), "name": "n"}
    r = rng.random()
    if r < 0.1:
        del p["contestId"]
    elif r < 0.15:
        p["contestId"] = None
    elif r < 0.2:
        p["contestId"] = "12"
    return {
        "id": rng.randint(1, 10**9),
        "creationTimeSeconds": NOW - rng.randint(0, 10**6),
        "problem": p,
        "verdict": rng.choice(["OK", "WRONG_ANSWER", "COMPILATION_ERROR", None, ""]),
    }


def ok(result):
    return {"status": "OK", "result": result}


BAD_VALUES = [
    None,
    "",
    "x",
    "12",
    0,
    -1,
    1,
    2**31,
    2**63 - 1,
    2**63,
    2**64,
    10**30,
    1.5,
    1500.0,
    -0.0,
    True,
    False,
    [],
    [1],
    {},
    {"a": 1},
    "tourist",
    "é",
    1e300,
]


def mutate(obj, rng):
    """Set a random node of a JSON tree to a bad value, or delete it."""
    if not isinstance(obj, (dict, list)) or not obj:
        return rng.choice(BAD_VALUES)
    obj = json.loads(json.dumps(obj))
    node = obj
    while True:
        keys = list(node) if isinstance(node, dict) else list(range(len(node)))
        k = rng.choice(keys)
        child = node[k]
        if isinstance(child, (dict, list)) and child and rng.random() < 0.6:
            node = child
            continue
        if rng.random() < 0.3:
            del node[k]
        else:
            node[k] = rng.choice(BAD_VALUES)
        return obj


FAIL_PAYLOADS = [
    {"status": "FAILED", "comment": "handles: User with handle zzz not found"},
    {"status": "FAILED", "comment": "handle: User with handle zzz not found"},
    {"status": "FAILED", "comment": "Handle: not found"},
    {"status": "FAILED", "comment": "HANDLE x NOT FOUND"},
    {"status": "FAILED", "comment": "contestId: not found"},
    {"status": "FAILED", "comment": "Call limit exceeded"},
    {"status": "FAILED", "comment": "LIMIT EXCEEDED"},
    {"status": "FAILED", "comment": "İ limit exceeded"},
    {"status": "FAILED", "comment": ""},
    {"status": "FAILED"},
    {"status": "FAILED", "comment": None},
    {"status": "FAILED", "comment": 5},
    {"status": "FAILED", "comment": ["x"]},
    {"status": "FAILED", "comment": "Ünï ẞ \u0130 handle"},
    {"status": "ok", "result": []},
    {"status": "OK"},
    {"status": None, "result": []},
    {"result": []},
    {},
    {"status": "OK", "result": None},
    {"status": "OK", "result": "x"},
    {"status": "OK", "result": 5},
    {"status": "OK", "result": {}},
    {"status": "OK", "result": []},
    {"status": "OK", "result": [[]]},
    {"status": "OK", "result": [None]},
    {"status": "OK", "result": ["x"]},
    {"status": "OK", "result": {"problems": []}},
    {"status": "OK", "result": {"rows": []}},
    {"status": "OK", "result": {"problems": None}},
    {"status": "OK", "result": {"rows": None}},
    {"status": "FAILED", "comment": "handle: x not found\n"},
    {"status": "FAILED", "comment": "  handle: x not found"},
    {"status": "FAILED", "comment": "handle: ẞ not found"},
    [],
    [1],
    "",
    "x",
    0,
    5,
    None,
    True,
    1.5,
    [{"status": "OK", "result": []}],
]


def gen_cf(c, rng):
    for p in FAIL_PAYLOADS:
        c.add("cf.user_info", "x", p)
        c.add("cf.users_info", ["a", "b", "c"], p)
        c.add("cf.problemset", p)
        c.add("cf.contests", p)
        c.add("cf.user_status", "x", None, None, p)
        c.add("cf.user_status", "x", 10, 5, p)
        c.add("cf.standings", 1, ["tourist"], p)
        c.add("cf.rating_history", "x", p)
    gens = {
        "user_info": (lambda: ["x", ok([u(rng)])], lambda r: ["x", r]),
        "users_info": (
            lambda: [rng.sample(HANDLES, 3), ok([u(rng) for _ in range(rng.randint(0, 4))])],
            None,
        ),
        "problemset": (
            lambda: [
                ok(
                    {
                        "problems": [prob(rng) for _ in range(rng.randint(0, 4))],
                        "problemStatistics": [],
                    }
                )
            ],
            None,
        ),
        "contests": (lambda: [ok([contest_cf(rng) for _ in range(rng.randint(0, 4))])], None),
        "user_status": (
            lambda: [
                "x",
                rng.choice([None, 0, 3, 10]),
                rng.choice([None, 0, NOW]),
                ok([sub_row(rng) for _ in range(rng.randint(0, 4))]),
            ],
            None,
        ),
        "standings": (
            lambda: [
                rng.randint(1, 3000),
                rng.sample(HANDLES, 3),
                ok(
                    {
                        "contest": {},
                        "problems": [],
                        "rows": [standing_row(rng) for _ in range(rng.randint(0, 4))],
                    }
                ),
            ],
            None,
        ),
        "rating_history": (
            lambda: [
                "x",
                ok(
                    [
                        {
                            "contestId": 1,
                            "ratingUpdateTimeSeconds": NOW - i,
                            "oldRating": 1,
                            "newRating": rng.randint(0, 4000),
                            "rank": 1,
                        }
                        for i in range(rng.randint(0, 4))
                    ]
                ),
            ],
            None,
        ),
    }
    for name, (make, _) in gens.items():
        fn = f"cf.{name}"
        for _ in range(60):  # valid payloads
            c.add(fn, *make())
        for _ in range(240):  # one mutation in the result, or in a handle list
            args = make()
            args[-1] = dict(args[-1], result=mutate(args[-1]["result"], rng))
            c.add(fn, *args)
    for h in HANDLES:  # standings: handle matching is lower()
        row = {
            "party": {"members": [{"handle": h}, {"handle": h.upper()}]},
            "rank": 1,
            "points": 1.0,
            "penalty": 0,
        }
        for w in (h, h.lower(), h.upper(), h.casefold(), h.title()):
            c.add("cf.standings", 1, [w], ok({"rows": [row]}))
    for miss in (
        ["rank"],
        ["points"],
        ["penalty"],
        ["rank", "points", "penalty"],
    ):  # rows no wanted handle matches are never read
        bare = {
            "party": {"members": [{"handle": "zed"}]},
            **{
                k: v
                for k, v in standing_row(rng, ["zed"]).items()
                if k not in miss and k in ("rank", "points", "penalty")
            },
        }
        mine = standing_row(rng, ["tourist"])
        c.add("cf.standings", 1, ["tourist"], ok({"rows": [bare, mine]}))
        c.add(
            "cf.standings", 1, ["zed"], ok({"rows": [bare]})
        )  # matched: Python raises KeyError, Rust the generic error
    for _ in range(10):  # user.info result shapes with len() == 1
        c.add(
            "cf.user_info",
            "x",
            ok(
                rng.choice(
                    [
                        {"handle": "a"},
                        "a",
                        "😀",
                        "é",
                        [u(rng)],
                        [u(rng), u(rng)],
                        [],
                        "ab",
                        {"a": 1, "b": 2},
                    ]
                )
            ),
        )


# ---------------------------------------------------------------- logic
def subs_list(rng, n=None):
    return [
        {
            "id": i,
            "created": NOW + rng.randint(-100, 400),
            "problem_key": rng.choice(["1A", "1B", "2A", "", "12", "A"]),
            "verdict": rng.choice(["OK", "COMPILATION_ERROR", "WRONG_ANSWER", None, "", "ok"]),
        }
        for i in range(rng.randint(0, 8) if n is None else n)
    ]


INTS = [
    0,
    1,
    -1,
    2,
    -2,
    3,
    7,
    -7,
    10,
    11,
    12,
    13,
    20,
    21,
    22,
    23,
    99,
    100,
    101,
    111,
    112,
    113,
    119,
    120,
    121,
    3599,
    3600,
    3601,
    -3599,
    -3600,
    -3601,
    86399,
    86400,
    86401,
    -86399,
    -86400,
    -86401,
    10**9,
    -(10**9),
    2**31,
    2**53,
    -(2**53),
    2**62,
    -(2**62),
]


def gen_logic(c, rng):
    for n in INTS + [rng.randint(-(10**6), 10**6) for _ in range(300)]:
        for fn in ("format_hm", "ordinal", "floor_rating", "utc_day", "week_start", "duel_gain"):
            c.add(fn, n)
        c.add("round_rating", n)
    c.add("round_rating", None)
    for d in range(-8, 9):  # week_start around every weekday, before and after the epoch
        for off in (0, 1, 86399, 86400 - 1):
            c.add("week_start", d * 86400 + off)
    for _ in range(300):
        c.add("week_start", rng.randint(-(10**10), 10**10))
        c.add("round_rating", rng.randint(700, 3600))
        c.add("floor_rating", rng.randint(700, 3600))
    for g in list(range(-450, 451, 25)) + [
        -401,
        -400,
        -399,
        -201,
        -200,
        -199,
        199,
        200,
        201,
        399,
        400,
        401,
    ]:
        c.add("duel_gain", g)
    for _ in range(1500):
        r, pr = rng.randint(-100, 4500), rng.randint(-100, 4500)
        c.add("solo_delta", r, pr, rng.random() < 0.9)
    for r in range(800, 3600, 100):
        for pr in range(800, 3600, 100):
            c.add("solo_delta", r, pr, True)
    for _ in range(300):
        c.add("solo_delta", rng.randint(-(10**6), 10**6), rng.randint(-(10**6), 10**6), True)
        c.add("solo_delta", rng.randint(-(10**6), 10**6), rng.randint(-(10**6), 10**6), False)
    for _ in range(
        800
    ):  # match_deltas: ties, singletons, rating gaps at the table edges, repeated and missing players
        n = rng.randint(1, 7)
        uids = rng.sample(range(1, 40), n)
        ratings = [
            [
                uid,
                rng.choice(
                    [800, 1000, 1199, 1200, 1400, 1600, 2000, 2400, 3500, rng.randint(0, 4000)]
                ),
            ]
            for uid in uids
        ]
        groups, left = [], uids[:]
        rng.shuffle(left)
        while left:
            k = rng.randint(1, len(left))
            groups.append(left[:k])
            left = left[k:]
        if rng.random() < 0.05:
            groups.append([rng.randint(1, 80)])
        if rng.random() < 0.03 and uids:
            groups.append([uids[0]])
        c.add(
            "match_deltas",
            groups,
            ratings,
            repr(rng.choice([1.0, 1.5, 1.5, 0.5, 2.0, 0.0, -1.0, 0.3333333])),
        )
    for _ in range(300):
        entries = [
            [rng.randint(1, 30), rng.choice([None, None, NOW + rng.randint(0, 5)])]
            for _ in range(rng.randint(0, 8))
        ]
        c.add("rank_groups", entries)
        c.add(
            "match_rating",
            rng.choice([None, None, 1200, 0, -5]),
            [rng.randint(-50, 4000) for _ in range(rng.randint(0, 6))],
        )
        c.add(
            "placement",
            [rng.choice([0, 0, 5, -5, 33]) for _ in range(rng.randint(0, 7))],
            rng.choice([0, 5, -5, 33, 100]),
        )
        c.add(
            "rating_series",
            [[rng.randint(0, 10**9), rng.randint(-60, 60)] for _ in range(rng.randint(0, 8))],
        )
    for _ in range(500):
        today = rng.randint(0, 20000)
        days = sorted({today - rng.randint(-1, 12) for _ in range(rng.randint(0, 8))}, reverse=True)
        if rng.random() < 0.1:
            days = days[::-1]
        c.add("daily_streak", days, today)
    for _ in range(500):
        t = rng.choice(
            [
                "",
                "dp",
                "DP, math",
                " dp ,, math ",
                "dp,dp",
                "Ünï,ÜNÏ",
                "İ",
                "ǅ,ǆ",
                "ẞ,ß",
                "ΑΣ",
                "a\u3000b, c",
                "x\x1cy,\x1fz\x1f",
                "\u200bz",
                "dp,\xa0math\xa0",
                "A,a,B",
                ",",
            ]
        )
        c.add("parse_tags", rng.choice([t, None, rand_str(rng)]))
        c.add("normalize_tags", rng.choice([t, None, rand_str(rng, 5) + "," + rand_str(rng, 3)]))
    for _ in range(500):
        alltags = rng.sample(TAGS + ["a" * 90, "dp2", "greedy2", "Math"], rng.randint(0, 11))
        cur = rng.choice(
            [
                "",
                "d",
                "dp,",
                "dp, ma",
                "math,greedy,",
                " , ,i",
                "Ünï",
                "x" * 95,
                "a" * 95 + ",",
                "dp,dp,m",
                ",",
                "ü",
                "İ",
                "MA",
                "*",
            ]
        )
        c.add("tag_suggestions", cur, alltags, rng.choice([0, 1, 3, 25]))
    for _ in range(500):
        ss = subs_list(rng)
        c.add(
            "has_compile_error",
            ss,
            rng.choice(["1A", "1B", ""]),
            NOW + rng.randint(-5, 5),
            rng.choice([300, 0, 1, -1, 10**6]),
        )
        c.add("solved_keys", ss)
        c.add("unsolved_keys", ss)
    for _ in range(
        600
    ):  # weekly board: ties on rating and plays, handles in Unicode order, events around the window edges
        cutoff = rng.choice([7 * 86400, 14 * 86400, NOW])
        events = [
            [
                rng.randint(1, 6),
                rng.choice(["b", "a", "É", "e\u0301", "😀", "A", "ß", "B"]),
                cutoff
                - rng.choice(
                    [0, 1, 7 * 86400, 7 * 86400 + 1, 7 * 86400 - 1, rng.randint(-86400, 20 * 86400)]
                ),
                rng.choice([0, 10, -10, 33, 16, -16]),
            ]
            for _ in range(rng.randint(0, 10))
        ]
        c.add("weekly_board", events, cutoff)


# ---------------------------------------------------------------- embeds
RANKS_CF = [None, "newbie", "pupil", "grandmaster", "legendary grandmaster", "", "İ*rank*"]
RATINGS = [None, 0, 800, 1500, 3500, 4000, -5, 2**31]
COUNTS = [
    0,
    1,
    2,
    9,
    10,
    11,
    14,
    15,
    16,
    19,
    20,
    21,
    25,
    30,
    31,
    45,
]  # around the page sizes 10 and 15


def e_str(rng):
    return rng.choice(
        [
            rng.choice(HANDLES),
            rng.choice(NAMES),
            rand_str(rng, 5),
            "a_b",
            "*x*",
            "[l](u)",
            "`c`",
            "<@1>",
            "https://x.y/a_b",
        ]
    )


def e_problem(rng):
    return {
        "contest_id": rng.choice([1, 1700, 2**31, 0, -1]),
        "index": rng.choice(["A", "B2", "", "ß", "😀", "A_b"]),
        "name": e_str(rng),
        "rating": rng.choice(RATINGS),
        "tags": rng.sample(TAGS, rng.randint(0, 4)) + rng.choice([[], [""], ["a|b"], ["*"]]),
    }


def e_contest(rng):
    return {
        "id": rng.choice([1, 2000, 2**31]),
        "name": e_str(rng),
        "type": rng.choice(["CF", "ICPC"]),
        "phase": "BEFORE",
        "start_time": rng.choice([None, 0, 1, NOW, NOW + 86400 * 400, 2**40]),
        "duration": rng.choice([0, 59, 60, 3599, 3600, 7200, 7260, 86400, 10**7]),
    }


def e_entries(rng, n):
    pts = [rng.choice([0, 1, 1.5, 2.5, 1000, 1e3, 1500.25, 3e5, 0.1]) for _ in range(n)]
    pts.sort(reverse=True)
    out, rank = [], 1
    for k, p in enumerate(pts):  # ties share a rank
        if k and p != pts[k - 1]:
            rank = k + 1
        out.append(
            [
                rank,
                rng.choice([1, 2, 10**18, 2**63 - 1]),
                e_str(rng),
                p,
                rng.choice([0, 0, 1, 50, 10**6]),
            ]
        )
    return out


def gen_embeds(c, rng):
    for _ in range(150):
        c.add(
            "embed.problem", e_problem(rng), rng.choice([None, "", "footer", "Ünï *f*", e_str(rng)])
        )
    for _ in range(300):
        issued = rng.choice([None, NOW, 0])
        solved = rng.choice([None, None, NOW + 5])
        row = {
            "day": rng.choice([0, 19000, 20000, -1]),
            "problem_key": rng.choice([None, "1A", "", "1700B"]),
            "rating": rng.choice(RATINGS),
            "issued_at": issued,
            "solved_at": solved,
        }
        c.add(
            "embed.daily",
            row,
            rng.choice([None, e_problem(rng)]),
            rng.choice([None, 0, 12, -14, 100]),
            rng.choice([0, 1, 2, 365]),
        )
    for _ in range(150):
        u = {
            "handle": rng.choice(HANDLES),
            "rating": rng.choice(RATINGS),
            "max_rating": rng.choice(RATINGS),
            "rank": rng.choice(RANKS_CF),
        }
        c.add("embed.profile", u, e_str(rng))
    for _ in range(200):
        c.add(
            "embed.member_profile",
            e_str(rng),
            rng.choice([0, 1500, -20, 4000]),
            rng.choice([None, 0, 1, 7]),
            rng.choice([0, 1, 30]),
            rng.choice([0, 1, 12]),
            rng.choice([0, 3]),
            [rng.choice([-30, 0, 15, 0, 7]) for _ in range(rng.randint(0, 6))],
            rng.choice([None, 0, 1800]),
            rng.choice([0, 1, 9]),
            rng.choice(["unrated", "1500 (specialist)", e_str(rng)]),
            rng.choice([0, 1, 4]),
        )
    for _ in range(300):
        n = rng.choice(COUNTS)
        cs = [e_contest(rng) for _ in range(n)]
        c.add(
            "embed.contest_pages",
            cs,
            rng.choice(["Upcoming contests", "", e_str(rng)]),
            rng.choice([10, 10, 1, 3, 15, 0]),
        )
    for _ in range(150):
        c.add("embed.new_contest", e_contest(rng), rng.choice(["New contest", "", "Soon *x*"]))
    for _ in range(200):
        rows = [
            [
                k + 1,
                rng.choice([1, 2**63 - 1]),
                e_str(rng),
                rng.choice([0, 1500, 2**40]),
                rng.choice([0, 10, -10, 100]),
                rng.choice([None, 0, 3, -3]),
            ]
            for k in range(rng.choice(COUNTS))
        ]
        c.add("embed.weekly", rows)
    for _ in range(300):
        rows = [
            [
                rng.choice([1, 2, 3, 99, 10**18]) + k,
                e_str(rng),
                rng.choice([0, 1500, -10]),
                rng.choice([0, 1, 5, 100]),
            ]
            for k in range(rng.choice(COUNTS))
        ]
        streaks = [[r[0], rng.choice([0, 1, 5, 30])] for r in rows if rng.random() < 0.5]
        c.add(
            "embed.stats_pages",
            rows,
            rng.choice([1, 2, 99, 10**18, 0]),
            streaks,
            rng.random() < 0.4,
            rng.choice(["all", "matches", "contests", "x", ""]),
        )
    for _ in range(300):
        c.add(
            "embed.ranklist",
            e_entries(rng, rng.choice(COUNTS)),
            rng.choice(["Ranklist", "", e_str(rng)]),
            rng.choice([10, 10, 1, 4, 0]),
        )
    for _ in range(300):
        es = [
            [rng.choice([1, 2**63 - 1]), e_str(rng), rng.choice(RATINGS), rng.choice(RANKS_CF)]
            for _ in range(rng.choice(COUNTS))
        ]
        c.add("embed.handle_list", es, rng.choice([10, 10, 1, 7, 0]))
    for _ in range(200):
        c.add("embed.recap", e_contest(rng), e_entries(rng, rng.choice(COUNTS)))
    for _ in range(300):
        items = [
            [rng.choice(["1A", "1700B", "", "x_y"]), rng.choice([None, e_problem(rng)])]
            for _ in range(rng.choice(COUNTS))
        ]
        c.add("embed.stalk", rng.choice(HANDLES), items, rng.choice([15, 15, 1, 4, 0]))


# ---------------------------------------------------------------- embeds, games: match, history, head to head, mashup and bingo messages
PKEYS = [("1", "A"), ("1", "B"), ("1700", "C"), ("2", "A"), ("5", "D"), ("7", "E"), ("9", "F")]


def g_problem(rng, k):
    return {
        "contest_id": int(PKEYS[k][0]),
        "index": PKEYS[k][1],
        "name": e_str(rng),
        "rating": rng.choice(RATINGS),
        "tags": rng.sample(TAGS, rng.randint(0, 3)),
    }


def g_match(rng, mid=1, mode=None, status=None):
    status = status or rng.choice(["lobby", "active", "finished", "cancelled"])
    mode = mode or rng.choice(["solo", "duel", "custom", "custom", "weird"])
    started = None if status in ("lobby", "cancelled") else NOW
    return {
        "id": mid, "guild_id": 1, "channel_id": 2, "message_id": rng.choice([None, 3]), "host_id": rng.choice([1, 2]), "mode": mode,
        "max_players": rng.choice([1, 2, 3, 10]), "invited_id": rng.choice([None, 2]), "rating_arg": rng.choice([None, 1500]),
        "rating": rng.choice([None, 800, 1500, 3500]), "problem_key": rng.choice([None, "1A", "1700C", "zz", ""]), "status": status,
        "created_at": NOW - 100, "started_at": started, "expires_at": rng.choice([None, NOW + 600, NOW - 5]),
        "finished_at": rng.choice([NOW + 50, NOW + 50, None]) if status == "finished" else rng.choice([None, NOW]),
        "tags": rng.choice(["", "dp", "dp,math"]), "exclude_tags": rng.choice(["", "greedy"]),
    }  # fmt: skip


def g_players(rng, mid=1, n=None, uids=(1, 2, 3, 4, 5)):
    out = []
    for uid in rng.sample(list(uids), rng.randint(1, len(uids)) if n is None else n):
        out.append({
            "match_id": mid, "guild_id": 1, "user_id": uid, "handle": e_str(rng), "joined_at": NOW - 90,
            "solved_at": rng.choice([None, None, NOW + 10, NOW + 20, NOW + 10]), "gave_up": rng.random() < 0.2,
            "delta": rng.choice([None, 0, 16, -16, 33, -33, 7]), "active": rng.random() < 0.8,
        })  # fmt: skip
    return out


def g_mashup(rng, mode=None, status=None, teams=None):
    status = status or rng.choice(["lobby", "active", "finished", "cancelled"])
    mode = mode or rng.choice(["contest", "contest", "classic", "replace"])
    started = None if status in ("lobby", "cancelled") else NOW
    return {
        "id": 1, "guild_id": 1, "channel_id": 2, "message_id": rng.choice([None, 3]), "host_id": rng.choice([1, 2, 99]), "min_rating": rng.choice([800, 1200]),
        "max_rating": rng.choice([1600, 3500]), "count": rng.choice([1, 3, 4, 9]), "duration_seconds": rng.choice([3600, 7200, 5400, 59]),
        "penalty_minutes": rng.choice([0, 10]), "status": status, "created_at": NOW - 100, "started_at": started,
        "ends_at": None if started is None else NOW + rng.choice([-50, 0, 3600, 7200]), "teams": rng.choice([0, 2, 3, 10] if teams is None else [teams]),
        "max_players": rng.choice([2, 4, 20]), "rated": rng.choice([0, 1]), "team_size": rng.choice([0, 1, 2, 5]), "tags": rng.choice(["", "dp"]),
        "exclude_tags": rng.choice(["", "greedy"]), "contest_id": rng.choice([None, None, 1700]), "mode": mode, "size": rng.choice([2, 3]) if mode != "contest" else 0,
        "win": rng.choice(["line", "continuous"]), "thread_id": rng.choice([None, 5]), "log_board_id": rng.choice([None, 6]),
    }  # fmt: skip


def g_view(rng):
    m = g_mashup(rng)
    n = rng.randint(1, 5)
    players = [
        {"user_id": 1 + k, "handle": e_str(rng), "joined_at": NOW - 50, "team": rng.choice([1, 2, 3]) if m["team_size"] != 1 else k + 1,
         "delta": rng.choice([None, 0, 16, -16, 33]), "active": rng.random() < 0.8}
        for k in range(n)
    ]  # fmt: skip
    # PARITY.md 2026-11-10 (d): a team-None player (hand-edited DB) is an error in Rust
    if rng.random() < 0.05:
        rng.choice(players)["team"] = None
    cells, problems = [], []
    nprob = m["count"] if m["mode"] == "contest" else m["size"] ** 2
    nprob = min(nprob, len(PKEYS))
    ratings = sorted(rng.choice([800, 1000, 1500, 2000]) for _ in range(nprob))
    for k in range(nprob):
        key = PKEYS[k][0] + PKEYS[k][1]
        if rng.random() < 0.03:
            key = "zz"
        problems.append([key, ratings[k]])
        if m["mode"] != "contest":
            cells.append(
                {
                    "idx": k,
                    "key": key,
                    "rating": ratings[k],
                    "cell": k,
                    "since": NOW if rng.random() < 0.5 else NOW + 5,
                }
            )
            if m["mode"] == "replace" and rng.random() < 0.4:
                extra = {
                    "idx": nprob + k,
                    "key": "9F",
                    "rating": 1500,
                    "cell": k,
                    "since": NOW + 30,
                }
                cells.append(extra)
    if m["mode"] == "contest" and rng.random() < 0.2:
        problems = []
    solves = []
    for p in players:
        mine = [
            [k, [NOW + rng.randint(1, 3000), rng.choice([0, 0, 1, 4])]]
            for k in dict.fromkeys(
                k for k, _ in problems
            )  # unique keys: a Python mapping collapses repeats
            if rng.random() < 0.4
        ]
        if mine or rng.random() < 0.3:
            solves.append([p["user_id"], mine])
    rng.shuffle(solves)
    return {
        "mashup": m,
        "problems": problems,
        "players": players,
        "solves": solves,
        "note": rng.choice([None, None, "A *note*", ""]),
        "cells": cells,
    }


def gen_game_embeds(c, rng):
    for _ in range(500):
        m = g_match(rng)
        ps = g_players(rng, m["id"])
        ratings = rng.choice(
            [
                None,
                [[p["user_id"], rng.choice([800, 1500, 2100])] for p in ps if rng.random() < 0.9],
            ]
        )
        c.add(
            "embed.match",
            m,
            ps,
            rng.choice([None, g_problem(rng, rng.randint(0, 6))]),
            ratings,
            rng.choice([None, "note *x*", ""]),
        )
    for _ in range(300):
        recs = []
        for mid in range(1, rng.choice([0, 1, 3, 11, 12, 25]) + 1):
            m = g_match(rng, mid, status="finished")
            recs.append(
                [
                    m,
                    g_players(rng, mid, uids=(1, 2, 3, 4, 5, 6))
                    if rng.random() < 0.95
                    else g_players(rng, mid, uids=(7,)),
                ]
            )
        mashups = []
        for _ in range(rng.choice([0, 0, 1, 4])):
            mm = g_mashup(rng, status="finished")
            mashups.append({"mashup": mm, "rank": rng.choice([1, 2, 5]), "entrants": rng.choice([2, 5]), "solved": rng.choice([0, 0, 1, 3]),
                            "team": rng.choice([None, 1, 2]), "delta": rng.choice([None, -9, 0, 12])})  # fmt: skip
        by_key = [g_problem(rng, k) for k in rng.sample(range(len(PKEYS)), rng.randint(0, 4))]
        c.add(
            "embed.history_pages",
            rng.choice(["History", "", e_str(rng)]),
            recs,
            rng.choice([1, 2, 3]),
            by_key,
            mashups,
        )
        c.add(
            "embed.head_to_head",
            rng.choice(["Versus", ""]),
            recs,
            rng.choice([1, 2]),
            rng.choice([2, 3, 6]),
            by_key,
        )
    for _ in range(200):
        pages = [
            [rng.choice(["Title", "", e_str(rng)]), rng.choice(["body", "", e_str(rng)])]
            for _ in range(rng.choice([0, 1, 2, 7]))
        ]
        c.add("embed.number_pages", pages)
    for ts in [
        0,
        1,
        59,
        86399,
        NOW,
        2**31,
        253402300799,
        -1,
        -62135596800,
    ]:
        c.add("embed.timestamp", "stamp", ts)
    for _ in range(100):
        c.add("embed.timestamp", e_str(rng), rng.randint(-62135596800, 253402300799))
    for _ in range(1200):
        v = g_view(rng)
        c.add("mashup.start_message", v)
        c.add("mashup.give_up_message", v, rng.choice([1, 2, 3, 99]))
        c.add("mashup.final_message", v)
        c.add("mashup.log_board_embed", v)
        keys = [p[0] for p in v["problems"]] or ["1A"]
        entry = [
            NOW + rng.choice([0, 5, 3000]),
            rng.choice([1, 2, 3, 99]),
            rng.choice(keys + ["zz"]),
            rng.choice([800, 1500]),
            rng.choice([1, 2, 3, 99]),
            rng.choice([0, 1, 4]),
        ]
        c.add(
            "mashup.log_message",
            v,
            entry,
            [g_problem(rng, k) for k in rng.sample(range(len(PKEYS)), rng.randint(0, 5))],
        )
        c.add(
            "mashup.embed",
            v,
            [g_problem(rng, k) for k in rng.sample(range(len(PKEYS)), rng.randint(0, 6))],
        )


# ---------------------------------------------------------------- store scenarios (base tables: handles, pending, reminders, recaps, weekly, new-contest and contest marks)
S_HANDLES = [
    "tourist",
    "Tourist",
    "TOURIST",
    "Petr",
    "ẞ",
    "İ",
    "e\u0301",
    "é",
    "日本語",
    "😀",
    "a b",
    "x" * 40,
    "",
    "A_b-c",
]


def store_op(rng):
    g, u, k = (
        rng.choice([1, 2, 2**40]),
        rng.choice([1, 2, 3, 2**62]),
        rng.choice([1, 2, 3, 1700, 2**31]),
    )
    return rng.choice(
        [
            ["link_handle", g, u, rng.choice(S_HANDLES), rng.choice([None, 0, 1500, 3500])],
            ["get_handle", g, u],
            ["linked_handles", g],
            ["user_for_handle", g, rng.choice(S_HANDLES)],
            ["unlink", g, u],
            [
                "save_pending",
                g,
                u,
                rng.choice(S_HANDLES),
                rng.choice(["1A", "", "1700B"]),
                rng.choice([0, 1700000000, -1]),
            ],
            ["get_pending", g, u],
            ["delete_pending", g, u],
            [
                "set_reminder",
                g,
                rng.choice([None, 5, 2**40]),
                rng.choice([None, 7]),
                rng.choice([[], [60], [60, 10], [10, 60, 5], [0], [-5], [2**40]]),
                rng.choice(["", "div\\. 2", "(", "İ", "a|b"]),
            ],
            ["get_reminder", g],
            ["all_reminders"],
            ["clear_reminder", g],
            ["set_recap", g, u],
            ["get_recap", g],
            ["all_recaps"],
            ["clear_recap", g],
            ["set_weekly", g, u, rng.choice([0, 100, 200])],
            ["get_weekly", g],
            ["all_weeklies"],
            ["clear_weekly", g],
            ["mark_weekly", g, rng.choice([0, 50, 100, 150, 200, 300])],
            ["set_newcontest", g, u, rng.sample([1, 2, 3, 1700], rng.randint(0, 3))],
            ["get_newcontest", g],
            ["all_newcontests"],
            ["clear_newcontest", g],
            ["mark_posted", g, k],
            ["posted", g],
            ["mark_soon", g, k],
            ["soon_posted", g],
            ["add_contest_remind", g, k, u],
            ["contest_reminders", g],
            ["mark_sent", g, k, rng.choice([5, 10, 60])],
            ["sent_for_guild", g],
            ["mark_recapped", g, k],
            ["recapped", g],
        ]
    )


def gen_store(c, rng):
    for _ in range(800):
        c.add("store.run", [store_op(rng) for _ in range(rng.randint(1, 30))])


# ---------------------------------------------------------------- store scenarios, games: matches, daily and mashups
# Few users, guilds and ids so that scenarios collide: one_active_match, one_active_mashup, AlreadyInMatch, MatchClosed, MatchFull.
KEYS = ["1A", "1B", "2A", "1700C", "", "x_y"]
G_HANDLES = ["tourist", "Petr", "ẞ", "a_b", "😀", ""]


def game_scenario(rng):
    nm = [
        0
    ]  # matches and mashups created so far (ids are AUTOINCREMENT from 1 on a fresh database)
    nz = [0]
    now = lambda: rng.choice([1000, 2000, 5000, 86400 + 5, 86400 * 2, 10**9])  # noqa: E731
    g = lambda: rng.choice([1, 1, 1, 2])  # noqa: E731
    u = lambda: rng.choice([1, 2, 3, 4])  # noqa: E731
    mid = lambda: rng.randint(1, nm[0] + 1)  # noqa: E731
    zid = lambda: rng.randint(1, nz[0] + 1)  # noqa: E731
    players = lambda: rng.choice([[1], [1, 2], [2, 1], [1, 2, 3], [2], [3, 4], []])  # noqa: E731

    def deltas():
        return [
            [x, rng.choice([-30, -1, 0, 7, 16, 33])]
            for x in rng.sample([1, 2, 3, 4], rng.randint(0, 4))
        ]  # noqa: E731

    day = lambda: rng.choice([0, 1, 2, 19000, 19001, 19002])  # noqa: E731

    def create_match():
        nm[0] += 1
        return [
            "create_match",
            {
                "guild_id": g(),
                "channel_id": 10,
                "host_id": u(),
                "host_handle": rng.choice(G_HANDLES),
                "mode": rng.choice(["solo", "duel", "custom"]),
                "max_players": rng.choice([1, 2, 3]),
                "invited_id": rng.choice([None, 2, 3]),
                "rating_arg": rng.choice([None, 1500, 800]),
                "now": now(),
                "tags": rng.choice(["", "dp", "dp,math"]),
                "exclude_tags": rng.choice(["", "greedy"]),
            },
        ]

    def create_mashup():
        nz[0] += 1
        return [
            "create_mashup",
            {
                "guild_id": g(),
                "channel_id": 11,
                "host_id": u(),
                "host_handle": rng.choice(G_HANDLES),
                "min_rating": rng.choice([800, 1200]),
                "max_rating": rng.choice([1600, 3500]),
                "count": rng.choice([1, 3, 4, 9]),
                "duration_seconds": rng.choice([3600, 7200]),
                "penalty_minutes": rng.choice([0, 10]),
                "now": now(),
                "teams": rng.choice([0, 2, 3]),
                "host_team": rng.choice([None, 1, 2]),
                "max_players": rng.choice([2, 4, 20]),
                "rated": rng.random() < 0.8,
                "team_size": rng.choice([0, 1, 2]),
                "tags": rng.choice(["", "dp"]),
                "exclude_tags": rng.choice(["", "greedy"]),
                "contest_id": rng.choice([None, 1700]),
                "mode": rng.choice(["contest", "contest", "classic", "replace"]),
                "size": rng.choice([0, 2, 3]),
                "win": rng.choice(["line", "continuous"]),
            },
        ]

    ops = [
        (6, create_match),
        (6, create_mashup),
        (5, lambda: ["add_player", mid(), g(), u(), rng.choice(G_HANDLES), now()]),
        (
            3,
            lambda: [
                "add_mashup_player",
                zid(),
                g(),
                u(),
                rng.choice(G_HANDLES),
                now(),
                rng.choice([2, 4, 20]),
                rng.choice([None, 1, 2, 3]),
                rng.choice([0, 1, 2]),
            ],
        ),
        (
            2,
            lambda: [
                "move_mashup_player",
                zid(),
                u(),
                rng.choice([1, 2, 3]),
                rng.choice([0, 1, 2]),
            ],
        ),
        (2, lambda: ["remove_player", mid(), u()]),
        (1, lambda: ["remove_mashup_player", zid(), u()]),
        (1, lambda: ["cancel_match", mid(), now()]),
        (1, lambda: ["cancel_mashup", zid()]),
        (
            4,
            lambda: [
                "start_match",
                mid(),
                rng.choice([800, 1500]),
                rng.choice(KEYS),
                now(),
                now(),
                players(),
            ],
        ),
        (
            4,
            lambda: [
                "start_mashup",
                zid(),
                [
                    [rng.choice(KEYS), rng.choice([800, 1200, 1500])]
                    for _ in range(rng.randint(0, 4))
                ],
                now(),
                now(),
                players(),
                rng.choice([[], ["1A"], ["1A", "2A"]]),
            ],
        ),
        (2, lambda: ["mark_gave_up", mid(), u()]),
        (2, lambda: ["mark_mashup_gave_up", zid(), u()]),
        (
            4,
            lambda: [
                "record_solves",
                mid(),
                [[x, now()] for x in rng.sample([1, 2, 3, 4], rng.randint(0, 3))],
            ],
        ),
        (
            4,
            lambda: [
                "record_mashup_solves",
                zid(),
                u(),
                [
                    [
                        k,
                        [now(), rng.choice([0, 1, 3])],
                    ]  # unique keys: Python's mapping would collapse repeats
                    for k in rng.sample(KEYS, rng.randint(0, 3))
                ],
            ],
        ),
        (5, lambda: ["finish_match", mid(), now(), deltas(), rng.choice([None, None, day()])]),
        (
            4,
            lambda: [
                "finish_mashup",
                zid(),
                rng.choice([None, deltas()]),
                rng.choice([None, now()]),
            ],
        ),
        (
            2,
            lambda: [
                "replace_cell",
                zid(),
                rng.choice([0, 1, 2, 3]),
                rng.choice([0, 1, 2, 3]),
                rng.choice(KEYS),
                rng.choice([800, 1500]),
                now(),
            ],
        ),
        (2, lambda: ["set_message", mid(), 10, rng.choice([100, 2**40])]),
        (1, lambda: ["set_mashup_message", zid(), 11, 200]),
        (1, lambda: ["set_mashup_thread", zid(), 300]),
        (1, lambda: ["set_log_board", zid(), 400]),
        (2, lambda: ["mark_logged", zid(), rng.choice(KEYS + ["#final"])]),
        (
            3,
            lambda: [
                "issue_daily",
                g(),
                u(),
                day(),
                rng.choice(KEYS),
                rng.choice([800, 1500]),
                now(),
            ],
        ),
        (
            3,
            lambda: [
                "record_daily_solve",
                {
                    "guild_id": g(),
                    "channel_id": 10,
                    "user_id": u(),
                    "handle": rng.choice(G_HANDLES),
                    "day": day(),
                    "solved_at": now(),
                    "delta": rng.choice([-10, 0, 20, 40]),
                    "now": now(),
                },
            ],
        ),
        (1, lambda: ["get_daily", g(), u(), day()]),
        (1, lambda: ["daily_days", g(), u()]),
        (1, lambda: ["daily_days_by_user", g()]),
        (2, lambda: ["get_match", mid()]),
        (2, lambda: ["get_players", mid()]),
        (1, lambda: ["active_match_for", g(), u()]),
        (1, lambda: ["matches_due", now(), now()]),
        (2, lambda: ["finished_matches", g(), u(), rng.choice([0, 1, 100])]),
        (1, lambda: ["rating", g(), u()]),
        (2, lambda: ["stats", g(), rng.choice(["all", "matches", "contests", "x"])]),
        (2, lambda: ["rating_changes", g(), u()]),
        (2, lambda: ["get_mashup", zid()]),
        (1, lambda: ["active_mashup_for", g(), u()]),
        (
            1,
            lambda: [
                "mashups_in",
                *rng.sample(["lobby", "active", "finished", "cancelled"], rng.randint(0, 3)),
            ],
        ),
        (1, lambda: ["finished_mashups", g(), u(), rng.choice([0, 1, 50])]),
        (1, lambda: ["mashup_problems", zid()]),
        (2, lambda: ["mashup_cells", zid()]),
        (2, lambda: ["mashup_players", zid()]),
        (1, lambda: ["mashup_excluded", zid()]),
        (2, lambda: ["mashup_solves", zid()]),
    ]
    weights = [w for w, _ in ops]

    def rand_ops(n):
        return [rng.choices(ops, weights)[0][1]() for _ in range(n)]

    def match_flow():
        # create, join, start with the right players, solve, finish: the path the random ops rarely complete.
        gg, base = g(), rng.choice([1000, 86400 * 3 + 5, 10**9])
        host, others = u(), rng.sample([1, 2, 3, 4], rng.randint(0, 2))
        ps = [host] + [x for x in others if x != host]
        out = [create_match()]
        out[0][1].update(guild_id=gg, host_id=host, now=base)
        m = nm[0]
        out += [["add_player", m, gg, x, rng.choice(G_HANDLES), base + 1] for x in ps[1:]]
        shown = ps if rng.random() < 0.8 else rng.choice([ps[::-1], ps[:1], ps + [9]])
        out.append(
            [
                "start_match",
                m,
                rng.choice([800, 1500]),
                rng.choice(KEYS),
                base + 10,
                base + 600,
                shown,
            ]
        )
        out += [
            [
                "record_solves",
                m,
                [[x, base + rng.randint(11, 500)] for x in ps if rng.random() < 0.6],
            ]
        ]
        if rng.random() < 0.3:
            out.append(["mark_gave_up", m, rng.choice(ps)])
        out.append(
            [
                "finish_match",
                m,
                base + 700,
                [[x, rng.choice([-30, 0, 16, 33])] for x in ps],
                rng.choice([None, base // 86400]),
            ]
        )
        if rng.random() < 0.3:
            out.append(["finish_match", m, base + 800, [], None])
        return out + [
            ["get_match", m],
            ["get_players", m],
            ["finished_matches", gg, host, 100],
            ["rating", gg, host],
            ["stats", gg, "all"],
            ["rating_changes", gg, host],
            ["daily_days", gg, host],
            ["daily_days_by_user", gg],
            ["active_match_for", gg, host],
        ]

    def daily_flow():
        gg, uu, base = g(), u(), rng.choice([1000, 86400 * 3 + 5, 10**9])
        d = base // 86400
        out = [["issue_daily", gg, uu, d, rng.choice(KEYS), rng.choice([800, 1500]), base]]
        if rng.random() < 0.3:
            out.append(
                ["issue_daily", gg, uu, d, rng.choice(KEYS), rng.choice([800, 1500]), base + 5]
            )
        out.append(
            [
                "record_daily_solve",
                {
                    "guild_id": gg,
                    "channel_id": 10,
                    "user_id": uu,
                    "handle": rng.choice(G_HANDLES),
                    "day": d,
                    "solved_at": base + 50,
                    "delta": rng.choice([-10, 0, 20, 40]),
                    "now": base + 60,
                },
            ]
        )
        if rng.random() < 0.5:  # a solo match claiming the day, or a second claim
            out += [
                create_match(),
                ["start_match", nm[0], 1500, "1A", base + 70, base + 600, [uu]],
                ["record_solves", nm[0], [[uu, base + 90]]],
                ["finish_match", nm[0], base + 100, [[uu, 20]], d],
            ]
            out[-4][1].update(guild_id=gg, host_id=uu, mode="solo", now=base + 65)
        return out + [
            ["get_daily", gg, uu, d],
            ["daily_days", gg, uu],
            ["rating", gg, uu],
            ["rating_changes", gg, uu],
            ["stats", gg, "matches"],
        ]

    def mashup_flow():
        gg, base = g(), rng.choice([1000, 86400 * 3 + 5, 10**9])
        host, others = u(), rng.sample([1, 2, 3, 4], rng.randint(0, 3))
        ps = [host] + [x for x in others if x != host]
        out = [create_mashup()]
        out[0][1].update(guild_id=gg, host_id=host, now=base, max_players=rng.choice([2, 4, 20]))
        z = nz[0]
        out += [
            [
                "add_mashup_player",
                z,
                gg,
                x,
                rng.choice(G_HANDLES),
                base + 1,
                rng.choice([2, 4, 20]),
                rng.choice([None, 1, 2, 3]),
                rng.choice([0, 1, 2]),
            ]
            for x in ps[1:]
        ]
        if rng.random() < 0.3:
            out.append(
                [
                    "move_mashup_player",
                    z,
                    rng.choice(ps),
                    rng.choice([1, 2, 3]),
                    rng.choice([0, 1, 2]),
                ]
            )
        shown = ps if rng.random() < 0.8 else ps[:1] + [9]
        probs = [[k, rng.choice([800, 1200, 1500])] for k in rng.sample(KEYS, rng.randint(1, 4))]
        out.append(
            [
                "start_mashup",
                z,
                probs,
                base + 10,
                base + 7200,
                shown,
                rng.choice([[], ["1A"], ["1A", "2A"], ["1A", "1A"]]),
            ]
        )
        for x in ps:
            out.append(
                [
                    "record_mashup_solves",
                    z,
                    x,
                    [
                        [k, [base + rng.randint(11, 7000), rng.choice([0, 1, 3])]]
                        for k, _ in probs
                        if rng.random() < 0.5
                    ],
                ]
            )
        out += [
            [
                "replace_cell",
                z,
                rng.choice([0, 1]),
                len(probs) - 1,
                rng.choice(KEYS),
                1500,
                base + 100,
            ],
            ["mark_logged", z, probs[0][0]],
            ["mark_logged", z, probs[0][0]],
        ]
        if rng.random() < 0.3:
            out.append(["mark_mashup_gave_up", z, rng.choice(ps)])
        out.append(
            [
                "finish_mashup",
                z,
                rng.choice([None, [[x, rng.choice([-30, 0, 16, 33])] for x in ps]]),
                rng.choice([None, base + 100]),
            ]
        )
        return out + [
            ["get_mashup", z],
            ["mashup_players", z],
            ["mashup_problems", z],
            ["mashup_cells", z],
            ["mashup_solves", z],
            ["mashup_excluded", z],
            ["finished_mashups", gg, host, 50],
            ["stats", gg, "all"],
            ["rating_changes", gg, host],
            ["rating", gg, host],
            ["daily_days_by_user", gg],
        ]

    out = []
    for _ in range(rng.randint(1, 4)):
        out += rng.choice(
            [match_flow, daily_flow, mashup_flow, lambda: rand_ops(rng.randint(2, 12))]
        )()
    return out


def gen_games(c, rng):
    for _ in range(1500):
        c.add("store.run", game_scenario(rng))


DOMAINS = {
    "store": gen_store,
    "games": gen_games,
    "game_embeds": gen_game_embeds,
    "embeds": gen_embeds,
    "text": gen_text,
    "ints": gen_ints,
    "contests": gen_contests,
    "regex": gen_regex,
    "cf": gen_cf,
    "logic": gen_logic,
}


def main():
    OUT.mkdir(exist_ok=True)
    for old in OUT.glob("*.jsonl"):
        old.unlink()
    total = 0
    for domain, gen in DOMAINS.items():
        c = Corpus(domain)
        gen(c, random.Random(f"{SEED}-{domain}"))
        (OUT / f"{domain}.jsonl").write_text(
            "".join(json.dumps(r, ensure_ascii=True) + "\n" for r in c.rows), encoding="utf-8"
        )
        counts = {}
        for r in c.rows:
            counts[r["fn"]] = counts.get(r["fn"], 0) + 1
        print(
            f"{domain}: {len(c.rows)}  " + " ".join(f"{k}={v}" for k, v in sorted(counts.items()))
        )
        total += len(c.rows)
    print(f"total {total}")


if __name__ == "__main__":
    sys.exit(main())
