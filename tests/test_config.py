from pathlib import Path

import pytest

from src.config import load_config


def test_load_config_defaults():
    cfg = load_config({"BOT_TOKEN": "tok"})
    assert cfg.token == "tok"
    assert cfg.dev_guild_id is None
    assert cfg.db_path == Path("data/bot.db")


def test_load_config_dev_guild_and_path():
    cfg = load_config({"BOT_TOKEN": "t", "DEV_GUILD_ID": "123", "DB_PATH": "x/y.db"})
    assert cfg.dev_guild_id == 123
    assert cfg.db_path == Path("x/y.db")


def test_load_config_blank_dev_guild_is_none():
    assert load_config({"BOT_TOKEN": "t", "DEV_GUILD_ID": ""}).dev_guild_id is None


def test_load_config_requires_token():
    with pytest.raises(RuntimeError, match="BOT_TOKEN"):
        load_config({})
