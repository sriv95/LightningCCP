import asyncio
import time
from collections.abc import Awaitable, Callable
from typing import Any

import aiohttp

from src.cf.models import Contest, Problem, Submission, User
from src.errors import CodeforcesError, HandleNotFound

BASE_URL = "https://codeforces.com/api/"


class CodeforcesClient:
    def __init__(
        self,
        session: Any,
        *,
        interval: float = 1.0,
        retries: int = 3,
        retry_delay: float = 2.0,
        sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
        clock: Callable[[], float] = time.monotonic,
    ):
        self._session = session
        self._interval = interval
        self._retries = retries
        self._retry_delay = retry_delay
        self._sleep = sleep
        self._clock = clock
        self._lock = asyncio.Lock()
        self._last_call = float("-inf")

    async def _get(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        try:
            async with self._session.get(
                BASE_URL + method, params=params, timeout=aiohttp.ClientTimeout(total=15)
            ) as resp:
                return await resp.json(content_type=None)
        except (aiohttp.ClientError, TimeoutError, ValueError) as exc:
            raise CodeforcesError("Codeforces is unreachable or down for maintenance.") from exc

    async def _call(self, method: str, params: dict[str, Any]) -> Any:
        for attempt in range(self._retries):
            async with self._lock:  # serialises calls so the 1 req/s spacing holds
                wait = self._interval - (self._clock() - self._last_call)
                if wait > 0:
                    await self._sleep(wait)
                self._last_call = self._clock()
                payload = await self._get(method, params)
            if payload.get("status") == "OK":
                return payload["result"]
            comment = payload.get("comment", "")
            lowered = comment.lower()
            if "limit exceeded" in lowered:
                if attempt + 1 < self._retries:
                    await self._sleep(self._retry_delay)
                continue
            if "not found" in lowered:
                raise HandleNotFound()
            raise CodeforcesError(f"Codeforces error: {comment or 'unknown'}")
        raise CodeforcesError("Codeforces rate limit hit. Try again shortly.")

    async def user_info(self, handle: str) -> User:
        (data,) = await self._call("user.info", {"handles": handle})
        return User(
            handle=data["handle"],
            rating=data.get("rating"),
            max_rating=data.get("maxRating"),
            rank=data.get("rank"),
        )

    async def problemset(self) -> list[Problem]:
        data = await self._call("problemset.problems", {})
        return [
            Problem(
                contest_id=p["contestId"],
                index=p["index"],
                name=p["name"],
                rating=p.get("rating"),
                tags=tuple(p.get("tags", ())),
            )
            for p in data["problems"]
        ]

    async def contests(self) -> list[Contest]:
        data = await self._call("contest.list", {})
        return [
            Contest(
                id=c["id"],
                name=c["name"],
                type=c["type"],
                phase=c["phase"],
                start_time=c.get("startTimeSeconds"),
                duration=c["durationSeconds"],
            )
            for c in data
        ]

    async def user_status(self, handle: str, count: int | None = None) -> list[Submission]:
        params: dict[str, Any] = {"handle": handle, "from": 1}
        if count is not None:
            params["count"] = count
        data = await self._call("user.status", params)
        return [
            Submission(
                id=s["id"],
                created=s["creationTimeSeconds"],
                problem_key=f"{s['problem'].get('contestId', '')}{s['problem']['index']}",
                verdict=s.get("verdict"),
            )
            for s in data
        ]
