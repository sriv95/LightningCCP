//! Port of src/cogs/help.py: `/help [topic]`, static text.
use crate::bot::{Context, DiscordOutput, Output};
use crate::ui::help::help_embed;

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum Topic {
    #[name = "start"]
    Start,
    #[name = "problems"]
    Problems,
    #[name = "matches"]
    Matches,
    #[name = "contests"]
    Contests,
    #[name = "ratings"]
    Ratings,
    #[name = "contest-list"]
    ContestList,
}

pub async fn reply<O: Output>(out: &O, topic: Option<&str>) -> anyhow::Result<()> {
    out.respond_embed_ephemeral(help_embed(topic)).await
}

/// How to use this bot
#[poise::command(slash_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "What to read about; leave empty for an overview"] topic: Option<Topic>,
) -> anyhow::Result<()> {
    reply(&DiscordOutput::from_ctx(ctx)?, topic.map(|t| poise::ChoiceParameter::name(&t))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::fake::{Call, FakeOutput};

    #[tokio::test]
    async fn help_replies_with_the_embed_for_the_topic() {
        let out = FakeOutput::new(1);
        reply(&out, Some("start")).await.unwrap();
        reply(&out, None).await.unwrap();
        assert_eq!(out.calls(), [Call::Respond(help_embed(Some("start"))), Call::Respond(help_embed(None))]);
    }

    #[test]
    fn command_surface_matches_python() {
        let c = help();
        assert_eq!(c.name, "help");
        assert_eq!(c.description.as_deref(), Some("How to use this bot"));
        let p = &c.parameters[0];
        assert_eq!((p.name.to_string(), p.description.as_deref(), p.required), ("topic".to_string(), Some("What to read about; leave empty for an overview"), false));
        let names: Vec<_> = p.choices.iter().map(|ch| ch.name.to_string()).collect();
        assert_eq!(names, ["start", "problems", "matches", "contests", "ratings", "contest-list"]);
    }
}
