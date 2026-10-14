import os
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Config:
    token: str
    db_path: Path


def load_config(env: Mapping[str, str] | None = None) -> Config:
    env = os.environ if env is None else env
    token = env.get("BOT_TOKEN")
    if not token:
        raise RuntimeError("BOT_TOKEN is required (set it in .env)")
    return Config(
        token=token,
        db_path=Path(env.get("DB_PATH") or "data/bot.db"),
    )
