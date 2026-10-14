from pathlib import Path

import pytest

from src.config import load_config


def test_load_config_defaults():
    cfg = load_config({"BOT_TOKEN": "tok"})
    assert cfg.token == "tok"
    assert cfg.db_path == Path("data/bot.db")


def test_load_config_db_path():
    cfg = load_config({"BOT_TOKEN": "t", "DB_PATH": "x/y.db"})
    assert cfg.db_path == Path("x/y.db")


def test_load_config_requires_token():
    with pytest.raises(RuntimeError, match="BOT_TOKEN"):
        load_config({})
