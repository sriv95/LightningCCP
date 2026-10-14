import logging

import aiohttp
import discord
from discord import app_commands
from discord.ext import commands

from src.cache.contests import ContestCache
from src.cache.problems import ProblemCache
from src.cf.client import CodeforcesClient
from src.config import Config
from src.db.store import Store
from src.errors import report_error
from src.matches import MatchService
from src.services import Services

log = logging.getLogger(__name__)

COGS = ("src.cogs.handles", "src.cogs.problems", "src.cogs.contests", "src.cogs.match")


class LightningBot(commands.Bot):
    services: Services

    def __init__(self, config: Config):
        super().__init__(
            command_prefix=commands.when_mentioned,
            intents=discord.Intents.default(),
            help_command=None,
        )
        self.config = config

    async def setup_hook(self) -> None:
        store = Store(self.config.db_path)
        await store.connect()
        session = aiohttp.ClientSession(headers={"User-Agent": "LightningCCP"})
        cf = CodeforcesClient(session)
        problems = ProblemCache(cf, store)
        self.services = Services(
            db=store,
            cf=cf,
            problems=problems,
            contests=ContestCache(cf, store),
            matches=MatchService(store, cf, problems),
            session=session,
        )
        await self.services.problems.start()
        await self.services.contests.start()
        for extension in COGS:
            await self.load_extension(extension)
        self.tree.on_error = self._on_tree_error
        if self.config.dev_guild_id:
            guild = discord.Object(id=self.config.dev_guild_id)
            self.tree.copy_global_to(guild=guild)
            await self.tree.sync(guild=guild)
            log.info("Synced commands to dev guild %s", self.config.dev_guild_id)
        else:
            await self.tree.sync()
            log.info("Synced commands globally")

    async def _on_tree_error(
        self, interaction: discord.Interaction, error: app_commands.AppCommandError
    ) -> None:
        await report_error(interaction, error)

    async def close(self) -> None:
        await super().close()  # unloads cogs (stops the reminder loop) before services go away
        services = getattr(self, "services", None)
        if services is None:
            return
        try:
            services.problems.stop()
            services.contests.stop()
        finally:
            try:
                await services.session.close()
            finally:
                await services.db.close()
