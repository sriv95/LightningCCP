import discord


class PaginatorView(discord.ui.View):
    def __init__(self, pages: list[discord.Embed], author_id: int):
        super().__init__(timeout=120)
        self.pages = pages
        self.author_id = author_id
        self.index = 0
        self.message: discord.WebhookMessage | None = None  # set by send_pages
        self._sync_buttons()

    async def on_timeout(self) -> None:
        for item in self.children:
            item.disabled = True
        if self.message is not None:
            try:
                await self.message.edit(view=self)
            except discord.HTTPException:
                pass  # the message is gone or too old to edit

    def _sync_buttons(self) -> None:
        self.prev.disabled = self.index == 0
        self.next.disabled = self.index == len(self.pages) - 1

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message(
                "Only the command user can flip pages.", ephemeral=True
            )
            return False
        return True

    async def _show(self, interaction: discord.Interaction) -> None:
        self._sync_buttons()
        await interaction.response.edit_message(embed=self.pages[self.index], view=self)

    @discord.ui.button(label="Prev", style=discord.ButtonStyle.secondary)
    async def prev(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        self.index -= 1
        await self._show(interaction)

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        self.index += 1
        await self._show(interaction)


async def send_pages(interaction: discord.Interaction, pages: list[discord.Embed]) -> None:
    """Send privately via followup (caller already deferred). Buttons only when there is more than one page."""
    if len(pages) == 1:
        await interaction.followup.send(embed=pages[0], ephemeral=True)
    else:
        view = PaginatorView(pages, interaction.user.id)
        view.message = await interaction.followup.send(
            embed=pages[0], view=view, ephemeral=True, wait=True
        )
