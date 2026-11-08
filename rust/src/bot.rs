//! Port of src/bot.py: the Discord shell. Startup order, intents, global command sync, error reporting.
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use poise::serenity_prelude as serenity;
use serenity::{
    ComponentInteractionCollector, CreateActionRow, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, GatewayIntents, MessageId,
};

use crate::config::Config;
use crate::errors::{self, Interaction, MissingPermissions, SendFailed};
use crate::services::Services;
use crate::ui::paginator::{self, Buttons, DENIED, Flip, PREV_ID, Paginator, Press};

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Arc<Services>, Error>;
pub type Command = poise::Command<Arc<Services>, Error>;

/// The command modules of src/bot.py `COGS`, in load order.
pub const COGS: [&str; 7] = ["handles", "problems", "contests", "match", "plot", "mashup", "help"];

/// Python `discord.Intents.default()`: everything except members, presences and message content.
pub fn intents() -> GatewayIntents {
    GatewayIntents::non_privileged()
}

/// What a slash command writes to Discord. A fake implements it for tests.
pub trait Output: Sync {
    fn user_id(&self) -> u64;
    /// `interaction.response.send_message(embed=..., ephemeral=True)`
    fn respond_embed_ephemeral(&self, embed: CreateEmbed) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `interaction.followup.send(embed=..., ephemeral=True, wait=True)`; the new message's id.
    fn followup_embed_ephemeral(
        &self,
        embed: CreateEmbed,
        buttons: Option<Buttons>,
    ) -> impl Future<Output = anyhow::Result<u64>> + Send;
    /// Edit only the buttons of a followup message (Python `message.edit(view=view)`).
    fn edit_followup_buttons(&self, message: u64, buttons: Buttons) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Serenity implementation of `Output` and of `errors::Interaction` for one slash command.
pub struct DiscordOutput<'a> {
    ctx: &'a serenity::Context,
    interaction: &'a serenity::CommandInteraction,
    /// poise's "initial response sent" flag, shared with `Context::defer` and `Context::say`.
    done: &'a AtomicBool,
}

impl<'a> DiscordOutput<'a> {
    pub fn from_ctx(ctx: Context<'a>) -> anyhow::Result<Self> {
        let Context::Application(app) = ctx else { anyhow::bail!("slash commands only") };
        Ok(Self { ctx: ctx.serenity_context(), interaction: app.interaction, done: app.has_sent_initial_response })
    }

    async fn respond(&self, message: CreateInteractionResponseMessage) -> Result<(), serenity::Error> {
        self.interaction.create_response(self.ctx, CreateInteractionResponse::Message(message)).await?;
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Python `send_pages` plus the view's life: sends, then handles Prev/Next presses until 120 s pass
    /// without one, then disables the buttons.
    pub async fn send_pages(&self, pages: Vec<CreateEmbed>) -> anyhow::Result<()> {
        if let Some(paginator) = paginator::send_pages(self, pages).await? {
            self.drive(paginator).await;
        }
        Ok(())
    }

    async fn drive(&self, mut paginator: Paginator) {
        let Some(message) = paginator.message else { return };
        // discord.py's view timeout is idle time: each press restarts it, so wait for one press at a time
        while let Some(press) = ComponentInteractionCollector::new(self.ctx)
            .message_id(MessageId::new(message))
            .timeout(paginator::TIMEOUT)
            .next()
            .await
        {
            let flip = if press.data.custom_id == PREV_ID { Flip::Prev } else { Flip::Next };
            let response = match paginator.press(press.user.id.get(), flip) {
                Press::Denied => CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new().content(DENIED).ephemeral(true),
                ),
                Press::Show(embed, buttons) => CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new().embed(embed).components(vec![buttons.row()]),
                ),
            };
            if let Err(e) = press.create_response(self.ctx, response).await {
                tracing::warn!("Could not answer a page button: {e}");
            }
        }
        paginator.on_timeout(self).await;
    }
}

impl Output for DiscordOutput<'_> {
    fn user_id(&self) -> u64 {
        self.interaction.user.id.get()
    }

    async fn respond_embed_ephemeral(&self, embed: CreateEmbed) -> anyhow::Result<()> {
        Ok(self.respond(CreateInteractionResponseMessage::new().embed(embed).ephemeral(true)).await?)
    }

    async fn followup_embed_ephemeral(&self, embed: CreateEmbed, buttons: Option<Buttons>) -> anyhow::Result<u64> {
        let mut followup = CreateInteractionResponseFollowup::new().embed(embed).ephemeral(true);
        if let Some(b) = buttons {
            followup = followup.components(vec![b.row()]);
        }
        Ok(self.interaction.create_followup(self.ctx, followup).await?.id.get())
    }

    async fn edit_followup_buttons(&self, message: u64, buttons: Buttons) -> anyhow::Result<()> {
        let rows: Vec<CreateActionRow> = vec![buttons.row()];
        let edit = CreateInteractionResponseFollowup::new().components(rows);
        self.interaction.edit_followup(self.ctx, MessageId::new(message), edit).await?;
        Ok(())
    }
}

impl Interaction for DiscordOutput<'_> {
    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn is_application_command(&self) -> bool {
        true
    }

    async fn send_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.respond(CreateInteractionResponseMessage::new().content(message).ephemeral(true))
            .await
            .map_err(|_| SendFailed)
    }

    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        let followup = CreateInteractionResponseFollowup::new().content(message).ephemeral(true);
        self.interaction.create_followup(self.ctx, followup).await.map(drop).map_err(|_| SendFailed)
    }

    async fn delete_original(&self) -> Result<(), SendFailed> {
        self.interaction.delete_response(&self.ctx.http).await.map_err(|_| SendFailed)
    }
}

/// Every slash command, from every module in `COGS`.
pub fn commands() -> Vec<Command> {
    vec![
        crate::cogs::help::help(),
        // TODO(s12, s13, s14, s15, s18, s19): handles, problems, contests, match, mashup and plot commands.
    ]
}

/// Python `tree.on_error`: the ephemeral reply for any command error.
async fn on_error(error: poise::FrameworkError<'_, Arc<Services>, Error>) {
    let Some(ctx) = error.ctx() else {
        tracing::error!("Framework error without a command: {error}");
        return;
    };
    let Ok(out) = DiscordOutput::from_ctx(ctx) else { return };
    match &error {
        poise::FrameworkError::Command { error, .. } => errors::report_error(&out, error).await,
        poise::FrameworkError::MissingUserPermissions { .. } => errors::report_error(&out, &MissingPermissions.into()).await,
        other => errors::report_error(&out, &anyhow::anyhow!("{other}")).await,
    }
}

pub fn options() -> poise::FrameworkOptions<Arc<Services>, Error> {
    poise::FrameworkOptions {
        commands: commands(),
        on_error: |error| Box::pin(on_error(error)),
        ..Default::default()
    }
}

/// Resolves on SIGINT or SIGTERM, like discord.py's `Client.run`.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return std::future::pending().await;
    };
    tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
}

/// Python `LightningBot.setup_hook` then `run`. Dotenv, logging and config are done by `main`.
/// Order: login (application id), store, HTTP session and clients, caches start, command modules,
/// global command sync, then the gateway connects. Python runs `setup_hook` inside `login()`, so the
/// sync happens before the gateway connects and on every startup, not after the Ready event.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let (data_tx, data_rx) = tokio::sync::oneshot::channel::<Arc<Services>>();
    let framework = poise::Framework::builder()
        .options(options()) // load the command modules
        .setup(move |_ctx, _ready, _framework| Box::pin(async move { Ok(data_rx.await?) }))
        .build();
    let mut client = serenity::ClientBuilder::new(&config.token, intents()).framework(framework).await?;
    // discord.py `login()`: the application id comes from the REST API, before any gateway connection
    let http = client.http.clone();
    http.set_application_id(http.get_current_application_info().await?.id);
    let services = Arc::new(Services::connect(&config).await?);
    services.start_caches().await;
    poise::builtins::register_globally(&http, &commands()).await?;
    tracing::info!("Synced commands globally");
    let _ = data_tx.send(services.clone());
    let shards = client.shard_manager.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        shards.shutdown_all().await;
    });
    let result = client.start().await;
    services.close().await; // after the client stops, like `close()` calling `super().close()` first
    Ok(result?)
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub enum Call {
        Respond(CreateEmbed),
        Followup(CreateEmbed, Option<Buttons>),
        EditButtons(u64, Buttons),
    }

    impl PartialEq for Call {
        fn eq(&self, other: &Self) -> bool {
            let json = |c: &Call| match c {
                Call::Respond(e) => (0, serde_json::to_value(e).unwrap(), None, 0),
                Call::Followup(e, b) => (1, serde_json::to_value(e).unwrap(), *b, 0),
                Call::EditButtons(m, b) => (2, serde_json::Value::Null, Some(*b), *m),
            };
            json(self) == json(other)
        }
    }

    pub struct FakeOutput {
        user: u64,
        fail_edit: bool,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeOutput {
        pub const MESSAGE_ID: u64 = 5;

        pub fn new(user: u64) -> Self {
            Self { user, fail_edit: false, calls: Mutex::new(vec![]) }
        }

        pub fn failing_edit(user: u64) -> Self {
            Self { fail_edit: true, ..Self::new(user) }
        }

        pub fn calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    impl Output for FakeOutput {
        fn user_id(&self) -> u64 {
            self.user
        }

        async fn respond_embed_ephemeral(&self, embed: CreateEmbed) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::Respond(embed));
            Ok(())
        }

        async fn followup_embed_ephemeral(&self, embed: CreateEmbed, buttons: Option<Buttons>) -> anyhow::Result<u64> {
            self.calls.lock().unwrap().push(Call::Followup(embed, buttons));
            Ok(Self::MESSAGE_ID)
        }

        async fn edit_followup_buttons(&self, message: u64, buttons: Buttons) -> anyhow::Result<()> {
            anyhow::ensure!(!self.fail_edit, "message is gone");
            self.calls.lock().unwrap().push(Call::EditButtons(message, buttons));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Python test_bot_is_slash_only: no prefix commands, help_command None (poise has no built-in help here).
    #[test]
    fn bot_is_slash_only() {
        let opts = options();
        assert!(opts.prefix_options.prefix.is_none());
        assert!(opts.commands.iter().all(|c| c.slash_action.is_some() && c.prefix_action.is_none()));
        assert!(opts.commands.iter().any(|c| c.name == "help"));
    }

    /// Python test_match_cog_is_loaded, plus the full list and order of `COGS`.
    #[test]
    fn match_cog_is_loaded() {
        assert!(COGS.contains(&"match") && COGS.contains(&"plot"));
        assert_eq!(COGS, ["handles", "problems", "contests", "match", "plot", "mashup", "help"]);
    }

    /// 53575421 is `discord.Intents.default().value` from the repo's discord.py.
    #[test]
    fn intents_equal_discord_py_default() {
        assert_eq!(intents().bits(), 53575421);
        assert!(!intents().contains(GatewayIntents::MESSAGE_CONTENT));
        assert!(!intents().contains(GatewayIntents::GUILD_MEMBERS));
    }
}
