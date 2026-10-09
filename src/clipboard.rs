//! The system clipboard, as a component of the app's entity.
//!
//! It is opened once and kept for the life of the app: on X11 the clipboard
//! belongs to the process that set it, so a connection that came and went would
//! take the copied text with it as soon as it dropped.

use std::sync::Mutex;

use anyhow::anyhow;

/// The clipboard a selection is copied to.
pub(crate) struct Clipboard {
    /// Where the connection lives. Behind a mutex because arboard's handle is
    /// not `Sync`, and a component of the world has to be.
    inner: Mutex<arboard::Clipboard>,
}

impl Clipboard {
    /// Opens the clipboard, or reports why it cannot be reached — a session
    /// with no display is one of those, and the app carries on without one.
    pub(crate) fn new() -> anyhow::Result<Self> {
        let clipboard =
            arboard::Clipboard::new().map_err(|error| anyhow!("opening the clipboard: {error}"))?;

        Ok(Self {
            inner: Mutex::new(clipboard),
        })
    }

    /// Replaces what is on the clipboard with `text`.
    pub(crate) fn copy(&self, text: &str) -> anyhow::Result<()> {
        self.inner
            .lock()
            .map_err(|_| anyhow!("the clipboard is poisoned"))?
            .set_text(text)
            .map_err(|error| anyhow!("copying to the clipboard: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::Clipboard;

    /// The clipboard is the session's, so on a machine with no display there is
    /// nothing here to test — that is a skip, not a failure, because the app
    /// runs without one.
    #[test]
    fn what_is_copied_is_what_the_clipboard_hands_back() {
        let Ok(clipboard) = Clipboard::new() else {
            return;
        };

        let mut reader = arboard::Clipboard::new().expect("a second connection");
        // The clipboard belongs to the person running the tests, so whatever was
        // on it goes back on it when the test is done.
        let previous = reader.get_text().ok();

        clipboard.copy("hext selection").expect("copying");

        assert_eq!(
            reader.get_text().expect("reading it back"),
            "hext selection"
        );

        match previous {
            Some(text) => clipboard.copy(&text).expect("putting it back"),
            None => reader.clear().expect("clearing what the test put there"),
        }
    }
}
