import logging

from discord.ext import tasks

from src.cf.client import CodeforcesClient
from src.cf.models import Contest
from src.db.store import Store
from src.logic import refresh_minutes

log = logging.getLogger(__name__)


class ContestCache:
    def __init__(self, cf: CodeforcesClient, store: Store):
        self._cf = cf
        self._store = store
        self.contests: list[Contest] = []
        self._loop = tasks.loop(minutes=30)(self._tick)

    async def load(self) -> None:
        self.contests = await self._store.load_contests()

    async def refresh(self) -> None:
        contests = await self._cf.contests()
        await self._store.replace_contests(contests)
        self.contests = contests

    async def _tick(self) -> None:
        try:
            await self.refresh()
        except Exception:
            log.exception("Contest cache refresh failed; retrying in 5 minutes")
            self._loop.change_interval(minutes=5)
        else:
            self._loop.change_interval(minutes=refresh_minutes(self.contests))

    async def start(self) -> None:
        await self.load()
        self._loop.start()

    def stop(self) -> None:
        self._loop.cancel()
