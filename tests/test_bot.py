from src.bot import COGS, LightningBot
from src.config import Config


def test_bot_is_slash_only(tmp_path):
    bot = LightningBot(Config("x", tmp_path / "t.db"))
    assert bot.help_command is None
    assert list(bot.commands) == []


def test_match_cog_is_loaded():
    assert "src.cogs.match" in COGS and "src.cogs.plot" in COGS
