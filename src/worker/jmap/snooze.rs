//! Snoozed mail coming back (#527).

use super::JmapWorker;
use crate::worker::index_queue::IndexQueue;
use crate::worker::jmap::index::jmap_load_folder;
use crate::worker::jmap::ops::jmap_undo_move;
use crate::worker::jmap::wire::jmap_session;
use crate::worker::WorkerEvent;

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Snoozed mail's time has come: unread again, back where it was, and
    /// that folder listed so the app sees it arrive.
    pub(super) async fn on_unsnooze(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            emit(WorkerEvent::Unsnoozed {
                message_ids,
                done: false,
            });
            return;
        };
        let unread = serde_json::json!({ "keywords/$seen": serde_json::Value::Null });
        let moved = jmap_undo_move(
            &s,
            account_id,
            state,
            &path,
            &dest,
            &message_ids,
            unread,
            cache.as_ref(),
        )
        .await;
        let done = match moved {
            Ok(0) => {
                tracing::info!("snooze: {message_ids:?} no longer in {path:?}");
                true
            }
            Ok(_) => {
                if let Ok(messages) =
                    jmap_load_folder(&s, account_id, dest_folder_id, &dest, cache.as_ref(), state)
                        .await
                {
                    let unread = state.chip_count(dest_folder_id, &messages);
                    emit(WorkerEvent::Messages {
                        folder_id: dest_folder_id,
                        messages,
                    });
                    emit(WorkerEvent::FolderUnread {
                        folder_id: dest_folder_id,
                        unread,
                    });
                }
                true
            }
            Err(e) => {
                tracing::warn!("snooze: could not bring {message_ids:?} back from {path:?}: {e}");
                false
            }
        };
        emit(WorkerEvent::Unsnoozed { message_ids, done });
    }
}
