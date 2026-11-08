"""Regenerates command_tree.json: Python's global command tree (every cog loaded, no login).

Run from the repo root: PYTHONPATH=. .venv/bin/python rust/tests/fixtures/dump_command_tree.py > rust/tests/fixtures/command_tree.json
The "Task exception" noise on stderr is the cog loops starting without a client; ignore it.
"""

import asyncio
import json
import sys

from src.bot import COGS, LightningBot
from src.config import Config


async def main() -> None:
    bot = LightningBot(Config(token="x", db_path=":memory:"))
    for extension in COGS:
        await bot.load_extension(extension)
    commands = [c.to_dict(bot.tree) for c in bot.tree.get_commands()]
    json.dump(commands, sys.stdout, indent=1, sort_keys=True)
    sys.stdout.write("\n")


asyncio.run(main())
