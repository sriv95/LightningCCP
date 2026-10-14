from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class User:
    handle: str
    rating: int | None
    max_rating: int | None
    rank: str | None


@dataclass(frozen=True, slots=True)
class Problem:
    contest_id: int
    index: str
    name: str
    rating: int | None
    tags: tuple[str, ...]

    @property
    def key(self) -> str:
        return f"{self.contest_id}{self.index}"

    @property
    def url(self) -> str:
        return f"https://codeforces.com/problemset/problem/{self.contest_id}/{self.index}"


@dataclass(frozen=True, slots=True)
class Contest:
    id: int
    name: str
    type: str
    phase: str
    start_time: int | None
    duration: int

    @property
    def url(self) -> str:
        return f"https://codeforces.com/contest/{self.id}"


@dataclass(frozen=True, slots=True)
class Submission:
    id: int
    created: int
    problem_key: str
    verdict: str | None
