use std::future::Future;

/// Error whose message is safe to show to the user. Variants mirror the Python subclasses of `BotError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotError {
    /// Plain `BotError(message)`.
    #[error("{0}")]
    Plain(String),
    #[error("{0}")]
    HandleNotFound(String),
    #[error("{0}")]
    HandleTaken(String),
    #[error("{0}")]
    Codeforces(String),
    #[error("{0}")]
    NotLinked(String),
    #[error("Codeforces data is still loading. Try again in a minute.")]
    CacheNotReady,
    #[error("You are already in a match or contest. Finish it, give up, or leave the lobby first.")]
    AlreadyInMatch,
    #[error("{0}")]
    MatchClosed(String),
    #[error("{0}")]
    MatchFull(String),
}

impl BotError {
    pub fn handle_not_found() -> Self {
        Self::HandleNotFound("No Codeforces user matches that handle.".into())
    }

    pub fn not_linked() -> Self {
        Self::NotLinked("Link your Codeforces handle first with /handle link.".into())
    }

    pub fn match_closed() -> Self {
        Self::MatchClosed("This match is no longer open.".into())
    }

    pub fn match_full() -> Self {
        Self::MatchFull("This match is full.".into())
    }

    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

/// Stands in for discord.py's `app_commands.MissingPermissions` (poise: missing user permissions).
#[derive(Debug, thiserror::Error)]
#[error("missing permissions")]
pub struct MissingPermissions;

/// A failed Discord send, which `report_error` swallows (Python `discord.HTTPException`).
#[derive(Debug)]
pub struct SendFailed;

/// The interaction calls `report_error` needs; the serenity impl lives in the discord shell.
pub trait Interaction: Sync {
    fn is_done(&self) -> bool;
    /// True for a slash command, false for a component (button) interaction.
    fn is_application_command(&self) -> bool;
    fn send_ephemeral(&self, message: &str) -> impl Future<Output = Result<(), SendFailed>> + Send;
    fn followup_ephemeral(&self, message: &str) -> impl Future<Output = Result<(), SendFailed>> + Send;
    fn delete_original(&self) -> impl Future<Output = Result<(), SendFailed>> + Send;
}

pub fn error_message(error: &anyhow::Error) -> String {
    if let Some(e) = error.downcast_ref::<BotError>() {
        e.user_message()
    } else if error.downcast_ref::<MissingPermissions>().is_some() {
        "You need the Manage Server permission to use this command.".into()
    } else {
        tracing::error!("Unhandled interaction error: {error:?}");
        "Something went wrong. Try again later.".into()
    }
}

/// Send a user-facing ephemeral message for any command or component error.
pub async fn report_error<I: Interaction>(interaction: &I, error: &anyhow::Error) {
    let message = error_message(error);
    let sent = if interaction.is_done() {
        if interaction.is_application_command() {
            // a public defer() makes the first followup public; drop the placeholder so ours is ephemeral
            let _ = interaction.delete_original().await;
        }
        interaction.followup_ephemeral(&message).await
    } else {
        interaction.send_ephemeral(&message).await
    };
    if sent.is_err() {
        tracing::warn!("Could not deliver error message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        done: bool,
        component: bool,
        fail: bool,
        calls: Mutex<Vec<(String, String, bool)>>,
    }

    impl Fake {
        fn push(&self, name: &str, msg: &str, eph: bool) -> Result<(), SendFailed> {
            if self.fail {
                return Err(SendFailed);
            }
            self.calls.lock().unwrap().push((name.into(), msg.into(), eph));
            Ok(())
        }
        fn calls(&self) -> Vec<(String, String, bool)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Interaction for Fake {
        fn is_done(&self) -> bool {
            self.done
        }
        fn is_application_command(&self) -> bool {
            !self.component
        }
        async fn send_ephemeral(&self, m: &str) -> Result<(), SendFailed> {
            self.push("send", m, true)
        }
        async fn followup_ephemeral(&self, m: &str) -> Result<(), SendFailed> {
            self.push("followup", m, true)
        }
        async fn delete_original(&self) -> Result<(), SendFailed> {
            self.calls.lock().unwrap().push(("delete".into(), String::new(), false));
            Ok(())
        }
    }

    fn call(name: &str, msg: &str) -> (String, String, bool) {
        (name.into(), msg.into(), true)
    }

    #[tokio::test]
    async fn bot_error_before_defer_is_ephemeral_send_message() {
        let i = Fake::default();
        report_error(&i, &BotError::handle_not_found().into()).await;
        assert_eq!(i.calls(), [call("send", "No Codeforces user matches that handle.")]);
    }

    #[tokio::test]
    async fn bot_error_wrapped_in_context_is_still_found() {
        // Python: CommandInvokeError.original. anyhow context keeps the source reachable.
        let i = Fake::default();
        let err = anyhow::Error::from(BotError::Plain("nope".into())).context("command x failed");
        report_error(&i, &err).await;
        assert_eq!(i.calls(), [call("send", "nope")]);
    }

    #[tokio::test]
    async fn after_defer_on_slash_command_deletes_placeholder_then_followup() {
        let i = Fake { done: true, ..Default::default() };
        report_error(&i, &BotError::Plain("nope".into()).into()).await;
        assert_eq!(i.calls(), [("delete".into(), String::new(), false), call("followup", "nope")]);
    }

    #[tokio::test]
    async fn after_defer_on_component_keeps_original_message() {
        let i = Fake { done: true, component: true, ..Default::default() };
        report_error(&i, &BotError::Plain("nope".into()).into()).await;
        assert_eq!(i.calls(), [call("followup", "nope")]);
    }

    #[tokio::test]
    async fn missing_permissions_message() {
        let i = Fake::default();
        report_error(&i, &MissingPermissions.into()).await;
        assert_eq!(i.calls()[0].1, "You need the Manage Server permission to use this command.");
    }

    #[tokio::test]
    async fn unknown_exception_is_generic() {
        let i = Fake::default();
        report_error(&i, &anyhow::anyhow!("secret")).await;
        assert_eq!(i.calls(), [call("send", "Something went wrong. Try again later.")]);
    }

    #[tokio::test]
    async fn failed_send_is_swallowed() {
        let err: anyhow::Error = BotError::Plain("nope".into()).into();
        report_error(&Fake { fail: true, ..Default::default() }, &err).await;
        report_error(&Fake { done: true, fail: true, ..Default::default() }, &err).await;
    }

    #[test]
    fn fixed_messages_match_python() {
        assert_eq!(BotError::not_linked().to_string(), "Link your Codeforces handle first with /handle link.");
        assert_eq!(BotError::CacheNotReady.to_string(), "Codeforces data is still loading. Try again in a minute.");
        assert_eq!(
            BotError::AlreadyInMatch.to_string(),
            "You are already in a match or contest. Finish it, give up, or leave the lobby first."
        );
        assert_eq!(BotError::match_closed().to_string(), "This match is no longer open.");
        assert_eq!(BotError::match_full().to_string(), "This match is full.");
    }
}
