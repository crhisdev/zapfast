//! The worker's side of the Status page: statuses arrive as messages to
//! [`STATUS_CHAT`], are filed there with their author as the sender, and go
//! when their day is up or their author deletes them.

use super::*;
use crate::model::{STATUS_CHAT, STATUS_LIFETIME};

impl Worker {
    /// Files a status, or applies its author's deletion of one. Reactions,
    /// replies and the other things that ride on a status are not kept.
    pub(super) fn ingest_status(&mut self, message: &wa::Message, info: &MessageInfo) {
        let base = message.get_base_message();
        if let Some(protocol) = base.protocol_message.as_option() {
            use wa::message::protocol_message::Type;
            if protocol.r#type == Some(Type::REVOKE)
                && let Some(target) = protocol.key.as_option().and_then(|key| key.id.as_deref())
                && let Ok(true) = self.archive.delete_message(STATUS_CHAT, target)
            {
                self.emit_statuses();
            }
            return;
        }
        if info.timestamp.timestamp() < crate::util::now() - STATUS_LIFETIME {
            return;
        }
        let Some(mut content) = classify(message) else {
            return;
        };
        // A repeated delivery keeps the file already downloaded.
        if let Ok(Some(existing)) = self.archive.message(STATUS_CHAT, &info.id) {
            content.keep_local_paths(&existing.content);
        }
        let from_me = info.source.is_from_me;
        let sender = if from_me {
            self.me()
        } else {
            self.canonical(&info.source.sender)
        };
        if !from_me && !info.push_name.is_empty() {
            self.remember_push_name(&sender, &info.push_name);
        }
        let row = Message {
            id: info.id.to_string(),
            chat: STATUS_CHAT.to_owned(),
            sender,
            sender_name: (!from_me && !info.push_name.is_empty())
                .then(|| info.push_name.to_string()),
            from_me,
            timestamp: info.timestamp.timestamp(),
            content,
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            history_order: None,
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: thumbnail_of(base),
        };
        match self
            .archive
            .insert_message(&row, Some(&message.encode_to_vec()))
        {
            Ok(()) => self.emit_statuses(),
            Err(error) => log::warn!("could not store a status: {error}"),
        }
    }

    /// Files the statuses history carries that are still up.
    pub(super) fn file_history_statuses(&mut self, messages: Vec<ParsedMessage>) {
        let since = crate::util::now() - STATUS_LIFETIME;
        let mut filed = false;
        for message in messages {
            if message.timestamp < since {
                continue;
            }
            let sender = if message.from_me {
                self.me()
            } else if let Some(sender) = message.sender.as_deref() {
                self.canonical_str(sender)
            } else {
                continue;
            };
            let row = Message {
                id: message.id,
                chat: STATUS_CHAT.to_owned(),
                sender,
                sender_name: message.push_name.filter(|_| !message.from_me),
                from_me: message.from_me,
                timestamp: message.timestamp,
                content: message.content,
                status: Delivery::None,
                delivered_at: None,
                read_at: None,
                quoted: None,
                reactions: Vec::new(),
                history_order: None,
                edited: false,
                mentions: Vec::new(),
                forwarded: false,
                thumbnail: message.thumbnail,
            };
            // History repeats what live delivery filed, downloads included.
            if matches!(self.archive.message(STATUS_CHAT, &row.id), Ok(Some(_))) {
                continue;
            }
            match self.archive.insert_message(&row, Some(&message.raw)) {
                Ok(()) => filed = true,
                Err(error) => log::warn!("could not store a status from history: {error}"),
            }
        }
        if filed {
            self.emit_statuses();
        }
    }

    /// Hands the Status page the statuses still up, dropping the rest.
    pub(super) fn emit_statuses(&mut self) {
        let since = crate::util::now() - STATUS_LIFETIME;
        if let Err(error) = self.archive.forget_statuses(since) {
            log::warn!("could not drop expired statuses: {error}");
        }
        match self.archive.statuses(since) {
            Ok(mut statuses) => {
                for status in &mut statuses {
                    self.polish(status);
                }
                self.emit(Event::Statuses(statuses));
            }
            Err(error) => log::warn!("could not read statuses: {error}"),
        }
    }
}
