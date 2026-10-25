//! Snoozed mail coming back (#527).

use super::{name_labels, GraphWorker};
use crate::worker::graph::http::graph_token;
use crate::worker::graph::index::graph_load_folder;
use crate::worker::graph::ops::graph_undo_move;
use crate::worker::index_queue::IndexQueue;
use crate::worker::WorkerEvent;

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
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
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        let moved = graph_undo_move(
            account,
            account_id,
            state,
            &path,
            &dest,
            &message_ids,
            Some(serde_json::json!({ "isRead": false })),
            cache.as_ref(),
        )
        .await;
        let done = match moved {
            Ok(0) => {
                tracing::info!("snooze: {message_ids:?} no longer in {path:?}");
                true
            }
            Ok(_) => {
                if let Some(token) = graph_token(account, emit).await {
                    if let Ok(messages) = graph_load_folder(
                        &token,
                        account_id,
                        dest_folder_id,
                        &dest,
                        cache.as_ref(),
                        state,
                    )
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
