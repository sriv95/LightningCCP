from src.bot import LightningBot
from src.config import Config


def test_bot_is_slash_only(tmp_path):
    bot = LightningBot(Config("x", None, tmp_path / "t.db"))
    assert bot.help_command is None
    assert list(bot.commands) == []
