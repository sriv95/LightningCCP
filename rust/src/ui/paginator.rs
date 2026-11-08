//! Port of src/ui/paginator.py: Prev/Next pages for the command user only, buttons dead after 120 s idle.
use std::time::Duration;

use serenity::all::{ButtonStyle, CreateActionRow, CreateButton, CreateEmbed};

use crate::bot::Output;

pub const TIMEOUT: Duration = Duration::from_secs(120);
pub const PREV_ID: &str = "page:prev";
pub const NEXT_ID: &str = "page:next";
pub const DENIED: &str = "Only the command user can flip pages.";

/// Disabled flags of the Prev and Next buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buttons {
    pub prev_disabled: bool,
    pub next_disabled: bool,
}

impl Buttons {
    /// Same labels and style as Python: "Prev" and "Next", secondary.
    pub fn row(self) -> CreateActionRow {
        let button = |id: &str, label: &str, disabled| {
            CreateButton::new(id).label(label).style(ButtonStyle::Secondary).disabled(disabled)
        };
        CreateActionRow::Buttons(vec![
            button(PREV_ID, "Prev", self.prev_disabled),
            button(NEXT_ID, "Next", self.next_disabled),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flip {
    Prev,
    Next,
}

/// What a button press leads to.
pub enum Press {
    /// Another user pressed: ephemeral `DENIED`, the page stays.
    Denied,
    /// Edit the message to this page and these buttons.
    Show(CreateEmbed, Buttons),
}

/// Python `PaginatorView`.
pub struct Paginator {
    pages: Vec<CreateEmbed>,
    author_id: u64,
    index: usize,
    /// Set by `send_pages`, like Python's `view.message`.
    pub message: Option<u64>,
}

impl Paginator {
    pub fn new(pages: Vec<CreateEmbed>, author_id: u64) -> Self {
        Self { pages, author_id, index: 0, message: None }
    }

    pub fn buttons(&self) -> Buttons {
        Buttons { prev_disabled: self.index == 0, next_disabled: self.index + 1 >= self.pages.len() }
    }

    pub fn press(&mut self, user_id: u64, flip: Flip) -> Press {
        if user_id != self.author_id {
            return Press::Denied;
        }
        // the buttons are disabled at the ends, so this only guards a stale click
        match flip {
            Flip::Prev => self.index = self.index.saturating_sub(1),
            Flip::Next => self.index = (self.index + 1).min(self.pages.len() - 1),
        }
        Press::Show(self.pages[self.index].clone(), self.buttons())
    }

    /// Disable both buttons on the sent message. Quiet when there is no message or the edit fails
    /// (the message is gone or too old to edit).
    pub async fn on_timeout<O: Output>(&self, out: &O) {
        if let Some(message) = self.message {
            let _ = out.edit_followup_buttons(message, Buttons { prev_disabled: true, next_disabled: true }).await;
        }
    }
}

/// Python `send_pages`: private followup (the caller already deferred); buttons only for more than one page.
/// Returns the paginator to drive when there are buttons.
pub async fn send_pages<O: Output>(out: &O, mut pages: Vec<CreateEmbed>) -> anyhow::Result<Option<Paginator>> {
    if pages.len() == 1 {
        out.followup_embed_ephemeral(pages.remove(0), None).await?;
        return Ok(None);
    }
    let mut paginator = Paginator::new(pages, out.user_id());
    let first = paginator.pages[0].clone();
    paginator.message = Some(out.followup_embed_ephemeral(first, Some(paginator.buttons())).await?);
    Ok(Some(paginator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::fake::{Call, FakeOutput};

    fn two_pages() -> Vec<CreateEmbed> {
        vec![CreateEmbed::new(), CreateEmbed::new()]
    }

    const ALL_OFF: Buttons = Buttons { prev_disabled: true, next_disabled: true };

    #[tokio::test]
    async fn buttons_are_disabled_when_the_view_times_out() {
        let out = FakeOutput::new(1);
        let mut p = Paginator::new(two_pages(), 1);
        p.message = Some(7);
        p.on_timeout(&out).await;
        assert_eq!(out.calls(), [Call::EditButtons(7, ALL_OFF)]);
    }

    #[tokio::test]
    async fn timeout_without_a_message_or_after_it_vanished_is_quiet() {
        let out = FakeOutput::new(1);
        let mut p = Paginator::new(two_pages(), 1);
        p.on_timeout(&out).await; // no message yet
        assert_eq!(out.calls(), []);
        let gone = FakeOutput::failing_edit(1);
        p.message = Some(7);
        p.on_timeout(&gone).await; // edit fails: no panic, no error
    }

    #[tokio::test]
    async fn send_pages_keeps_the_message_for_the_timeout() {
        let out = FakeOutput::new(1);
        let p = send_pages(&out, two_pages()).await.unwrap().unwrap();
        assert_eq!(p.message, Some(FakeOutput::MESSAGE_ID));
        let calls = out.calls();
        assert!(matches!(&calls[0], Call::Followup(_, Some(b)) if *b == Buttons { prev_disabled: true, next_disabled: false }));
        let out = FakeOutput::new(1);
        assert!(send_pages(&out, vec![CreateEmbed::new()]).await.unwrap().is_none());
        assert!(matches!(&out.calls()[0], Call::Followup(_, None))); // one page: no buttons
    }

    #[test]
    fn only_the_author_flips_and_the_buttons_follow_the_page() {
        let mut p = Paginator::new(vec![CreateEmbed::new().title("a"), CreateEmbed::new().title("b")], 1);
        assert!(matches!(p.press(2, Flip::Next), Press::Denied));
        let Press::Show(e, b) = p.press(1, Flip::Next) else { panic!("author must flip") };
        assert_eq!(serde_json::to_value(e).unwrap()["title"], "b");
        assert_eq!(b, Buttons { prev_disabled: false, next_disabled: true });
        let Press::Show(_, b) = p.press(1, Flip::Prev) else { panic!() };
        assert_eq!(b, Buttons { prev_disabled: true, next_disabled: false });
    }

    #[test]
    fn button_row_matches_python_labels_and_style() {
        let v = serde_json::to_value(Buttons { prev_disabled: true, next_disabled: false }.row()).unwrap();
        let c = &v["components"];
        assert_eq!((c[0]["label"].as_str(), c[0]["style"].as_u64(), c[0]["disabled"].as_bool()), (Some("Prev"), Some(2), Some(true)));
        assert_eq!((c[1]["label"].as_str(), c[1]["style"].as_u64(), c[1]["disabled"].as_bool()), (Some("Next"), Some(2), Some(false)));
        assert_eq!(DENIED, "Only the command user can flip pages.");
    }
}
