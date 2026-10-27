//! Following a channel from its link (`whatsapp.com/channel/<code>`).
//!
//! The link is previewed first through the newsletter API's invite lookup,
//! which answers with the same `Event::InvitePreview` a group invite does, so
//! the Join group dialog shows it. Following joins the channel by its id and
//! files the chat under the channel's name, so it opens at once instead of as
//! a bare id; its picture comes from the channel's metadata as for any other
//! channel.

use super::*;

impl Worker {
    /// Looks up the channel behind link `code`, without following it.
    pub(super) fn preview_channel(&mut self, code: String) {
        let Some(client) = self.client.clone() else {
            self.emit(Event::InvitePreview {
                code,
                result: Err(
                    crate::i18n::gettext_noop("ZapFast is not connected to WhatsApp").to_owned(),
                ),
            });
            return;
        };
        let events = self.events.clone();
        let waker = self.waker.clone();
        tokio::spawn(async move {
            let result = client
                .newsletter()
                .get_metadata_by_invite(&code)
                .await
                .map(|channel| crate::model::InviteInfo {
                    id: channel.jid.to_string(),
                    subject: channel.name,
                    description: channel.description.filter(|text| !text.trim().is_empty()),
                    members: usize::try_from(channel.subscriber_count).unwrap_or(usize::MAX),
                    approval: false,
                })
                .map_err(|error| channel_error(&error.to_string()));
            let _ = events.send(Event::InvitePreview { code, result });
            waker.wake();
        });
    }

    /// Follows channel `id`, previewed from link `code`.
    pub(super) fn follow_channel(&mut self, code: String, id: ChatId) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&id)) else {
            self.emit(Event::InviteJoined {
                code,
                result: Err(
                    crate::i18n::gettext_noop("ZapFast is not connected to WhatsApp").to_owned(),
                ),
            });
            return;
        };
        let commands = self.commands.clone();
        let waker = self.waker.clone();
        tokio::spawn(async move {
            let result = client
                .newsletter()
                .join(&jid)
                .await
                .map(|channel| (channel.jid.to_string(), channel.name))
                .map_err(|error| channel_error(&error.to_string()));
            let _ = commands.send(Command::ChannelFollowed { code, result });
            waker.wake();
        });
    }

    /// Files a channel WhatsApp now has us following, and tells the window.
    pub(super) fn channel_followed(
        &mut self,
        code: String,
        result: Result<(ChatId, String), String>,
    ) {
        let result = result.map(|(chat, name)| {
            log::info!("followed a channel");
            if self.archive.ensure_chat(&chat, &name).is_err() {
                log::warn!("could not file a followed channel");
            }
            if !name.trim().is_empty() {
                let _ = self.archive.rename_chat(&chat, &name);
            }
            // Following again after leaving lifts the mark leaving left.
            let _ = self.archive.set_left(&chat, false);
            let _ = self.archive.touch_activity(&chat, crate::util::now());
            self.emit_chat(&chat);
            (chat, false)
        });
        if let Err(error) = &result {
            log::warn!("could not follow a channel: {error}");
        }
        self.emit(Event::InviteJoined { code, result });
    }
}

/// What to tell the person when a channel link cannot be read or followed.
fn channel_error(error: &str) -> String {
    let error = error.to_ascii_lowercase();
    if error.contains("not found") || error.contains("404") || error.contains("item-not-found") {
        "This channel link is invalid or the channel no longer exists.".to_owned()
    } else {
        "WhatsApp could not open this channel. Try again later.".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_channel_says_so() {
        assert_eq!(
            channel_error("newsletter not found for invite: abc"),
            "This channel link is invalid or the channel no longer exists."
        );
        assert_eq!(
            channel_error("iq timed out"),
            "WhatsApp could not open this channel. Try again later."
        );
    }
}
