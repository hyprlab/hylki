//! What a card asks of the app: its menus, templates, and the addresses in
//! its header.

use super::menu::{MenuHit, TranslateEntry};
use super::types::{MessageViewInput, MessageViewOutput, TrState};
use super::MessageView;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageView {
    pub(super) fn on_card_action(
        &mut self,
        action: crate::ui::message_list::RowAction,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        if let Some(m) = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
        {
            let _ = sender.output(MessageViewOutput::CardAction {
                action,
                message: Box::new(m.clone()),
            });
        }
    }

    pub(super) fn on_card_menu_at(
        &mut self,
        key: Option<(u32, u32)>,
        x: f64,
        y: f64,
        hit: MenuHit,
        sender: &ComponentSender<Self>,
    ) {
        // Unnamed (the page did not answer): the message on screen
        // when there is one, else the conversation's first, so a
        // right-click never ends without a menu.
        let m = key
            .and_then(|(a, i)| self.thread.iter().find(|m| m.account_id == a && m.id == i))
            .or_else(|| self.thread.first());
        let Some(m) = m.cloned() else { return };
        let mut hit = hit;
        let key = (m.account_id, m.id);
        if crate::services::translate::load().enabled() && !self.encrypted(key) {
            let label = match self.translations.get(&key) {
                Some(TrState::Shown(_)) => i18n("Show Original"),
                Some(TrState::Original(_)) => i18n("Show Translation"),
                _ => i18n("Translate"),
            };
            hit.translate = Some(TranslateEntry {
                label,
                reader: sender.input_sender().clone(),
            });
        }
        if self.reader_style.active() && !self.reader_mode {
            hit.sender_style = Some((
                self.sender_style.contains(&key),
                sender.input_sender().clone(),
            ));
        }
        if self.thread.len() > 1 {
            let folded = self.folded_cards();
            hit.folds = Some((
                !folded.is_empty(),
                folded.len() < self.thread.len(),
                sender.input_sender().clone(),
            ));
        }
        let point = self.webview.root().and_then(|root| {
            let root: gtk::Widget = root.upcast();
            self.webview
                .compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
        });
        let (wx, wy) = point.map_or((x, y), |p| (p.x() as f64, p.y() as f64));
        let _ = sender.output(MessageViewOutput::CardMenu {
            message: Box::new(m),
            x: wx,
            y: wy,
            hit,
        });
    }

    pub(super) fn on_card_menu_from(
        &mut self,
        account_id: u32,
        id: u32,
        x: f64,
        y: f64,
        page_width: f64,
        sender: &ComponentSender<Self>,
    ) {
        // CSS pixels → widget pixels (the page may be zoomed); from
        // there it is the same menu the right-click path opens.
        let ratio = self.webview.width() as f64 / page_width.max(1.0);
        sender.input_sender().emit(MessageViewInput::CardMenuAt {
            key: Some((account_id, id)),
            x: x * ratio,
            y: y * ratio,
            hit: MenuHit::default(),
        });
    }

    pub(super) fn on_template_action(
        &mut self,
        account_id: u32,
        id: u32,
        edit: bool,
        sender: &ComponentSender<Self>,
    ) {
        let message = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
            .cloned();
        if let Some(message) = message {
            use crate::ui::message_list::RowAction;
            let action = if edit {
                RowAction::EditTemplate
            } else {
                RowAction::EditAsNew
            };
            let _ = sender.output(MessageViewOutput::CardAction {
                action,
                message: Box::new(message),
            });
        }
    }

    pub(super) fn on_open_header(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        if let Some(m) = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
        {
            let _ = sender.output(MessageViewOutput::OpenWindow(Box::new(m.clone())));
        }
    }
}
