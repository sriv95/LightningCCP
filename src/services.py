from dataclasses import dataclass

import aiohttp

from src.cache.contests import ContestCache
from src.cache.problems import ProblemCache
from src.cf.client import CodeforcesClient
from src.db.store import Store
from src.matches import MatchService


@dataclass
class Services:
    db: Store
    cf: CodeforcesClient
    problems: ProblemCache
    contests: ContestCache
    matches: MatchService
    session: aiohttp.ClientSession
