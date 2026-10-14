import asyncio
import time
from collections.abc import Awaitable, Callable, Sequence
from typing import Any

import aiohttp

from src.cf.models import Contest, Problem, RatingChange, Standing, Submission, User
from src.errors import CodeforcesError, HandleNotFound

BASE_URL = "https://codeforces.com/api/"
STATUS_PAGES = 5  # pages of submissions a `since` read may take


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
        self.requests = 0  # calls sent to Codeforces since start
        self.rate_limited = 0  # of those, answered "limit exceeded"

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
                self.requests += 1
                payload = await self._get(method, params)
            if payload.get("status") == "OK":
                return payload["result"]
            comment = payload.get("comment", "")
            lowered = comment.lower()
            if "limit exceeded" in lowered:
                self.rate_limited += 1
                if attempt + 1 < self._retries:
                    await self._sleep(self._retry_delay)
                continue
            if "not found" in lowered and lowered.startswith("handle"):
                raise HandleNotFound()
            raise CodeforcesError(f"Codeforces error: {comment or 'unknown'}")
        raise CodeforcesError("Codeforces rate limit hit. Try again shortly.")

    async def user_info(self, handle: str) -> User:
        result = await self._call("user.info", {"handles": handle})
        if len(result) != 1:  # ";" in the handle makes user.info treat it as several handles
            raise HandleNotFound()
        (data,) = result
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

    async def user_status(
        self, handle: str, count: int | None = None, since: int | None = None
    ) -> list[Submission]:
        """The newest `count` submissions (all when None). With `since`, keeps reading older
        pages of `count` until one reaches back before that time (at most STATUS_PAGES pages),
        so a busy player's early submissions of a contest are not cut off."""
        out: list[Submission] = []
        for page in range(STATUS_PAGES if since is not None and count else 1):
            params: dict[str, Any] = {"handle": handle, "from": 1 + page * (count or 0)}
            if count is not None:
                params["count"] = count
            data = await self._call("user.status", params)
            got = [
                Submission(
                    id=s["id"],
                    created=s["creationTimeSeconds"],
                    problem_key=f"{s['problem'].get('contestId', '')}{s['problem']['index']}",
                    verdict=s.get("verdict"),
                )
                for s in data
            ]
            out += got
            if len(got) < (count or 0) or any(s.created < (since or 0) for s in got):
                break
        return out

    async def standings(
        self, contest_id: int, handles: Sequence[str], chunk: int = 100
    ) -> list[Standing]:
        """Official standing rows of the given handles; handles Codeforces does not know are skipped."""
        rows: list[Standing] = []
        for i in range(0, len(handles), chunk):
            rows += await self._standings(contest_id, list(handles[i : i + chunk]))
        return rows

    async def _standings(self, contest_id: int, handles: list[str]) -> list[Standing]:
        params = {
            "contestId": contest_id,
            "handles": ";".join(handles),
            "from": 1,
            "count": len(handles),
            "showUnofficial": "false",
        }
        try:
            data = await self._call("contest.standings", params)
        except HandleNotFound:
            # One renamed or deleted handle fails the whole call: split to find and drop it.
            if len(handles) == 1:
                return []
            mid = len(handles) // 2
            return await self._standings(contest_id, handles[:mid]) + await self._standings(
                contest_id, handles[mid:]
            )
        return [
            Standing(
                rank=r["rank"],
                handles=tuple(m["handle"] for m in r["party"]["members"]),
                points=r["points"],
                penalty=r["penalty"],
            )
            for r in data["rows"]
        ]

    async def rating_history(self, handle: str) -> list[RatingChange]:
        data = await self._call("user.rating", {"handle": handle})
        return [RatingChange(c["ratingUpdateTimeSeconds"], c["newRating"]) for c in data]
