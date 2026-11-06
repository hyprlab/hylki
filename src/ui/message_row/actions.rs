//! What a row asks of the list: an action, its menu, and the card a drag
//! carries.

use super::row::Row;
use super::shared::RowLook;
use super::{RowAction, RowData};
use crate::models::Message;
use crate::ui::drag_card::{self, DRAG_STACK};
use crate::ui::message_list::MessageListInput;
use adw::prelude::*;
use relm4::prelude::*;

impl Row {
    pub(super) fn act(&self, action: RowAction) {
        if let Some(data) = self.data() {
            self.send(MessageListInput::RowAction {
                action,
                message: Box::new(Message::clone(&data.msg)),
            });
        }
    }

    pub(super) fn context_menu(&self, x: f64, y: f64) {
        let (Some(data), Some(shared)) = (self.data(), self.shared()) else {
            return;
        };
        let Some(view) = shared.view.upgrade() else {
            return;
        };
        let Some(point) = self
            .w
            .host
            .compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        self.send(MessageListInput::ContextMenu {
            x: point.x() as f64,
            y: point.y() as f64,
            key: (data.msg.account_id, data.msg.id),
        });
    }

    /// What a drag of this row carries under the pointer: the row's card,
    /// and when the row is one of a selection, the cards of the others
    /// stacked behind it, at most [`DRAG_STACK`] deep, with the full count.
    pub(super) fn drag_card(&self) -> Option<gtk::Widget> {
        let (data, shared) = (self.data()?, self.shared()?);
        let look = shared.look.borrow().clone();
        let me = (data.msg.account_id, data.msg.id);
        let selected = shared.selected_rows();
        let front = self.message_card(&data, &look, self.w.avatar.custom_image());
        if selected.len() < 2 || !selected.iter().any(|r| (r.msg.account_id, r.msg.id) == me) {
            return Some(front.upcast());
        }
        let behind: Vec<_> = selected
            .iter()
            .filter(|r| (r.msg.account_id, r.msg.id) != me)
            .take(DRAG_STACK - 1)
            .map(|r| self.message_card(r, &look, None))
            .collect();
        Some(drag_card::stack(front, behind, selected.len()))
    }

    /// One message as a drag card: its circle, the subject over who it is
    /// from (to, in Sent).
    fn message_card(
        &self,
        data: &RowData,
        look: &RowLook,
        image: Option<gtk::gdk::Paintable>,
    ) -> gtk::Box {
        let name = self.face_name(data, look);
        let email = self.face_email(data, look);
        drag_card::card(&data.msg.subject, &name, &email, image.as_ref())
    }

    pub(super) fn drag_payload(&self) -> Option<gtk::gdk::ContentProvider> {
        let (data, shared) = (self.data()?, self.shared()?);
        let me = (
            data.msg.account_id,
            data.msg.folder_id,
            data.msg.uid,
            data.msg.id,
        );
        let mut items = shared.selected_drag_keys();
        // Dragging a row outside the selection moves just that row.
        if !items.iter().any(|k| k.0 == me.0 && k.3 == me.3) {
            items = vec![me];
        }
        // A conversation row stands for its whole thread (#171).
        let threads = shared.thread_drag.borrow();
        let items: Vec<_> = items
            .into_iter()
            .flat_map(|k| threads.get(&(k.0, k.3)).cloned().unwrap_or_else(|| vec![k]))
            .collect();
        Some(drag_card::move_payload(&items))
    }
}
