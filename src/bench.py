"""`python -m src --bench [--items N] [--idle]`: offline benchmark, the twin of rust/src/bench.rs.

No token, no network: a fake Codeforces session serves rust/bench/fixtures (raw JSON, parsed per call like a
response body), the Discord side drops what it is given. Same workload, same order as the Rust side; see
rust/src/bench.rs for the definition. Prints one JSON line. Env: DB_PATH (must not exist yet), BENCH_FIXTURES.
"""

import asyncio
import json
import math
import os
import random
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

from src.cache.contests import ContestCache
from src.cache.problems import ProblemCache
from src.cf.client import BASE_URL, CodeforcesClient
from src.cogs import contests as contests_mod
from src.cogs.contests import ContestsCog
from src.cogs.problems import ProblemsCog
from src.db.store import Store
from src.mashup import MashupService
from src.matches import MatchService

BASE = 1_800_000_000  # the bench clock starts here (the fixtures' contest times are relative to it)
SEED = 1
GUILD = 1
USERS = 60
MATCH_USERS = 8  # users 1..8 each hold a running solo match; users 11..50 play the mashup
MASHUP_PLAYERS = range(11, 51)
REMINDER_GUILDS = 20
TICK_SECONDS = 300  # the clock moves this much per item
EXCLUDES = ["Div\\. 3|Div\\. 4|Educational", "Kotlin|Hello|Good Bye", "", "(?i)global|codeton"]
STEPS = (
    "gimme",
    "contest_pages",
    "contest_ics",
    "reminder_tick",
    "match_check",
    "mashup_tick",
    "ranklist",
)
DEFAULT_ITEMS = 50


class FakeResponse:
    def __init__(self, payload):
        self._payload = payload

    async def json(self, content_type=None):
        return self._payload

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc_info):
        return False


class FakeSession:
    """Stands in for aiohttp.ClientSession: the fixture file bytes, parsed on every call."""

    FILES = {
        "problemset.problems": "problemset.json",
        "contest.list": "contest_list.json",
        "user.info": "user_info.json",
        "user.status": "user_status.json",
        "contest.standings": "contest_standings.json",
    }

    def __init__(self, directory: Path):
        self._files = {m: (directory / f).read_bytes() for m, f in self.FILES.items()}
        self._bodies: dict[str, bytes] = {}  # one response body per request, built on first use

    def _body(self, method: str, params: dict) -> bytes:
        if method not in ("user.info", "user.status"):
            return self._files[method]
        payload = json.loads(self._files[method])
        if method == "user.info":
            wanted = {h.lower() for h in params["handles"].split(";")}
            payload["result"] = [u for u in payload["result"] if u["handle"].lower() in wanted]
        else:
            rows, count = payload["result"][params["from"] - 1 :], params.get("count")
            payload["result"] = rows if count is None else rows[:count]
        return json.dumps(payload, separators=(",", ":")).encode()

    def get(self, url, params=None, timeout=None):
        method, params = url.removeprefix(BASE_URL), params or {}
        key = method + "?" + "&".join(f"{k}={v}" for k, v in sorted(params.items()))
        body = self._bodies.get(key)
        if body is None:
            body = self._bodies[key] = self._body(method, params)
        return FakeResponse(json.loads(body))  # parsed per call, like a response body


class Interaction:
    """Collects nothing: the output side of a slash command."""

    def __init__(self, user_id: int):
        self.guild_id = GUILD
        self.user = SimpleNamespace(id=user_id)
        noop = self._noop
        self.response = SimpleNamespace(defer=noop, send_message=noop)
        self.followup = SimpleNamespace(send=noop)

    @staticmethod
    async def _noop(*args, **kwargs):
        return None


class Channel:
    async def send(self, *args, **kwargs):
        return None


async def drop_pages(interaction, pages):
    return None


def percentile(sorted_us: list[float], p: float) -> float:
    return sorted_us[max(0, math.ceil(p * len(sorted_us)) - 1)]


def proc_start_age_ms() -> float | None:
    """Milliseconds since this process started (Linux, 10 ms resolution), else None."""
    try:
        ticks = int(Path("/proc/self/stat").read_text().rsplit(") ", 1)[1].split()[19])
        uptime = float(Path("/proc/uptime").read_text().split()[0])
        return max(0.0, (uptime - ticks / os.sysconf("SC_CLK_TCK")) * 1000)
    except (OSError, ValueError, IndexError):
        return None


def peak_rss_kb() -> int | None:
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    except OSError:
        pass
    return None


def fixtures_dir() -> Path:
    return Path(
        os.environ.get("BENCH_FIXTURES")
        or Path(__file__).resolve().parent.parent / "rust/bench/fixtures"
    )


async def run(items: int, idle: bool, t0: float) -> None:
    random.seed(SEED)
    now = BASE
    tmp = None
    if os.environ.get("DB_PATH"):
        db_path = Path(os.environ["DB_PATH"])
        if db_path.exists():
            sys.exit(f"DB_PATH {db_path} exists; the bench needs a fresh database")
    else:
        tmp = tempfile.mkdtemp(prefix="lightningccp-bench-")
        db_path = Path(tmp) / "bench.db"

    # 1. startup: store (opens and migrates), clients, caches from the fake CF
    store = Store(db_path)
    await store.connect()
    cf = CodeforcesClient(FakeSession(fixtures_dir()), interval=0.0)
    problems = ProblemCache(cf, store)
    contests = ContestCache(cf, store)
    await problems.load()
    await problems.refresh()
    await contests.load()
    await contests.refresh()
    matches = MatchService(store, cf, problems, clock=lambda: BASE)
    mashups = MashupService(store, cf, problems, clock=lambda: BASE)
    startup_ms = (time.perf_counter() - t0) * 1000
    basis = "main_entry"
    age = proc_start_age_ms()
    if age is not None:
        startup_ms, basis = age, "process_start"

    # setup (not timed): linked members, reminders, running matches and one mashup
    handles = [f"bench_user_{i:03d}" for i in range(1, USERS + 1)]
    for user_id, user in enumerate(await cf.users_info(handles), start=1):
        await store.link_handle(GUILD, user_id, user.handle, user.rating)
    for g in range(1, REMINDER_GUILDS + 1):
        await store.set_reminder(
            g, 100 + g, 200 + g if g % 2 else None, [60, 10], EXCLUDES[g % len(EXCLUDES)]
        )
    match_ids = []
    for u in range(1, MATCH_USERS + 1):
        view = await matches.create(
            guild_id=GUILD, channel_id=5, host_id=u, mode="solo", max_players=1
        )
        match_ids.append(view.match.id)
    host = MASHUP_PLAYERS[0]
    mashup = await mashups.create(
        guild_id=GUILD,
        channel_id=5,
        host_id=host,
        min_rating=800,
        max_rating=1600,
        count=5,
        duration_seconds=7200,
        teams=10,
        team_size=5,
    )
    for n, u in enumerate(MASHUP_PLAYERS):
        if u != host:
            await mashups.join(mashup.mashup.id, u, n % 10 + 1)
    await mashups.start(mashup.mashup.id, host)

    # the cogs' real bodies behind fake interactions; the cog reads the clock through this shim
    clock = SimpleNamespace(now=now)
    contests_mod.time = SimpleNamespace(time=lambda: clock.now)
    contests_mod.send_pages = drop_pages
    services = SimpleNamespace(db=store, cf=cf, problems=problems, contests=contests)
    bot = SimpleNamespace(services=services, get_channel=lambda cid: Channel())
    contests_cog = ContestsCog.__new__(ContestsCog)
    contests_cog.bot, contests_cog._warned_channels, contests_cog._recap_retry = bot, set(), {}
    problems_cog = ProblemsCog.__new__(ProblemsCog)
    problems_cog.bot = bot
    upcoming = sorted(c.id for c in contests.contests if c.phase == "BEFORE")
    finished = sorted(c.id for c in contests.contests if c.phase == "FINISHED")

    kinds = ("upcoming", "running", "recent")

    async def work(step: str, i: int, interaction: Interaction):
        match step:
            case "gimme":
                await problems_cog.gimme.callback(problems_cog, interaction)
            case "contest_pages":
                await contests_cog._list(interaction, kinds[i % 3])
            case "contest_ics":
                await contests_cog.calendar(interaction, upcoming[i % len(upcoming)])
            case "reminder_tick":
                await contests_cog._send_due_reminders()
            case "match_check":
                await matches.check(match_ids[i % MATCH_USERS], i % MATCH_USERS + 1)
            case "mashup_tick":
                await mashups.tick()
            case "ranklist":
                await contests_cog.ranklist.callback(
                    contests_cog, interaction, finished[i % len(finished)]
                )

    times: dict[str, list[float]] = {s: [] for s in STEPS}
    for i in range(items):
        clock.now = BASE + i * TICK_SECONDS
        user = i % USERS + 1
        interaction = Interaction(user)
        for step in STEPS:
            t = time.perf_counter_ns()
            await work(step, i, interaction)
            times[step].append((time.perf_counter_ns() - t) / 1000)

    summary = {}
    for step, us in times.items():
        s = sorted(us)
        summary[step] = {
            "mean_us": round(sum(us) / len(us), 1),
            "p50_us": round(percentile(s, 0.5), 1),
            "p95_us": round(percentile(s, 0.95), 1),
        }
    print(
        json.dumps(
            {
                "impl": "python",
                "startup_ms": round(startup_ms, 1),
                "startup_basis": basis,
                "items": items,
                "steps": summary,
                "peak_rss_kb": peak_rss_kb(),
            }
        ),
        flush=True,
    )
    if idle:
        await asyncio.Event().wait()  # sleeps forever, no busy loop
    await store.close()
    if tmp:
        for f in Path(tmp).iterdir():
            f.unlink()
        os.rmdir(tmp)


def main(argv: list[str]) -> None:
    t0 = time.perf_counter()
    items = int(argv[argv.index("--items") + 1]) if "--items" in argv else DEFAULT_ITEMS
    try:
        asyncio.run(run(items, "--idle" in argv, t0))
    except KeyboardInterrupt:
        pass
