import contextlib
import logging

import discord
from discord import app_commands

log = logging.getLogger(__name__)


class BotError(Exception):
    """Error whose message is safe to show to the user."""

    def __init__(self, user_message: str):
        super().__init__(user_message)
        self.user_message = user_message


class HandleNotFound(BotError):
    def __init__(self, message: str = "No Codeforces user matches that handle."):
        super().__init__(message)


class HandleTaken(BotError):
    pass


class CodeforcesError(BotError):
    pass


class NotLinked(BotError):
    def __init__(self, message: str = "Link your Codeforces handle first with /handle link."):
        super().__init__(message)


class CacheNotReady(BotError):
    def __init__(self):
        super().__init__("Codeforces data is still loading. Try again in a minute.")


class ChallengeConflict(BotError):
    def __init__(self):
        super().__init__("You already have an open challenge. Use /gotgud or /nogud first.")


class NoChallenge(BotError):
    def __init__(self):
        super().__init__("You have no open challenge. Start one with /gitgud.")


async def report_error(interaction: discord.Interaction, error: Exception) -> None:
    """Send a user-facing ephemeral message for any command or component error."""
    original = getattr(error, "original", error)
    if isinstance(original, BotError):
        message = original.user_message
    elif isinstance(error, app_commands.MissingPermissions):
        message = "You need the Manage Server permission to use this command."
    else:
        log.error("Unhandled interaction error", exc_info=error)
        message = "Something went wrong. Try again later."
    try:
        if interaction.response.is_done():
            if interaction.type is discord.InteractionType.application_command:
                # a public defer() makes the first followup public; drop the placeholder so ours is ephemeral
                with contextlib.suppress(discord.HTTPException):
                    await interaction.delete_original_response()
            await interaction.followup.send(message, ephemeral=True)
        else:
            await interaction.response.send_message(message, ephemeral=True)
    except discord.HTTPException:
        log.warning("Could not deliver error message", exc_info=True)
