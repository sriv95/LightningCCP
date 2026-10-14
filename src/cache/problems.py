import logging

from discord.ext import tasks

from src.cf.client import CodeforcesClient
from src.cf.models import Problem
from src.db.store import Store
from src.errors import CacheNotReady, CodeforcesError

log = logging.getLogger(__name__)


class ProblemCache:
    def __init__(self, cf: CodeforcesClient, store: Store):
        self._cf = cf
        self._store = store
        self.problems: list[Problem] = []
        self.tags: frozenset[str] = frozenset()
        self._loop = tasks.loop(hours=6)(self._tick)

    def _set(self, problems: list[Problem]) -> None:
        self.problems = problems
        self.tags = frozenset(t for p in problems for t in p.tags)

    def require(self) -> list[Problem]:
        if not self.problems:
            raise CacheNotReady()
        return self.problems

    async def load(self) -> None:
        self._set(await self._store.load_problems())

    async def refresh(self) -> None:
        problems = await self._cf.problemset()
        if not problems:  # keep the old data; the tick retries in 5 minutes
            raise CodeforcesError("Codeforces returned no problems")
        await self._store.replace_problems(problems)
        self._set(problems)

    async def _tick(self) -> None:
        try:
            await self.refresh()
        except Exception:
            log.exception("Problem cache refresh failed; retrying in 5 minutes")
            self._loop.change_interval(minutes=5)
        else:
            self._loop.change_interval(hours=6)

    async def start(self) -> None:
        await self.load()
        self._loop.start()  # first tick runs immediately

    def stop(self) -> None:
        self._loop.cancel()
