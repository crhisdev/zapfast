//! Statuses: what people post for a day, which WhatsApp sends as messages to
//! [`STATUS_CHAT`]. They are filed under it with their author as the sender,
//! so downloads and the viewer work on them as on any message, and no chat
//! row is made for them: they never reach the chat list.

use super::{Archive, Result, params};
use crate::model::{Message, STATUS_CHAT};

impl Archive {
    /// The statuses posted at `since` or later, oldest first.
    pub fn statuses(&self, since: i64) -> Result<Vec<Message>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM messages WHERE chat = ?1 AND timestamp >= ?2
             ORDER BY timestamp, rowid",
        )?;
        let ids: Vec<String> = statement
            .query_map(params![STATUS_CHAT, since], |row| row.get(0))?
            .collect::<Result<_>>()?;
        let mut statuses = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(message) = self.message(STATUS_CHAT, &id)? {
                statuses.push(message);
            }
        }
        Ok(statuses)
    }

    /// Drops the statuses posted before `before`, which are no longer up.
    pub fn forget_statuses(&self, before: i64) -> Result<usize> {
        self.connection.execute(
            "DELETE FROM messages WHERE chat = ?1 AND timestamp < ?2",
            params![STATUS_CHAT, before],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Content, Delivery};

    fn status(id: &str, sender: &str, timestamp: i64) -> Message {
        Message {
            id: id.to_owned(),
            chat: STATUS_CHAT.to_owned(),
            sender: sender.to_owned(),
            sender_name: None,
            from_me: false,
            timestamp,
            history_order: None,
            content: Content::Text {
                text: id.to_owned(),
                preview: None,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    #[test]
    fn statuses_list_what_is_still_up_and_never_make_a_chat() {
        let archive = Archive::in_memory().unwrap();
        let ada = "4917000000001@s.whatsapp.net";
        for (id, timestamp) in [("late", 300), ("old", 100), ("early", 200)] {
            archive
                .insert_message(&status(id, ada, timestamp), None)
                .unwrap();
        }
        let ids = |archive: &Archive, since| {
            archive
                .statuses(since)
                .unwrap()
                .into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&archive, 150), ["early", "late"]);
        assert_eq!(archive.forget_statuses(150).unwrap(), 1);
        assert_eq!(ids(&archive, 0), ["early", "late"]);
        assert!(archive.chat(STATUS_CHAT).unwrap().is_none());
    }
}
