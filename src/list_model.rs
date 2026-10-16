/**
 * @file list_model.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Constructs row display payloads off the GTK thread.
 */

use crate::models::Message;

#[derive(Debug, Clone)]
pub struct RowPayload {
    pub id: u32,
    pub account_id: u32,
    pub folder_id: u32,
    pub uid: u32,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    pub preview: String,
    pub date_display: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    pub initials: String,
}

/// Builds row payloads completely off the GTK thread.
/// Returns the payload vector directly without requiring a glib::Sender.
pub fn build_rows_off_thread(messages: &[Message]) -> Vec<RowPayload> {
    messages
        .iter()
        .map(|m| {
            let initials = {
                let parts: Vec<&str> = m.from_name.trim().split_whitespace().collect();
                if parts.len() >= 2 {
                    let first = parts[0].chars().next().unwrap_or('?');
                    let second = parts[1].chars().next().unwrap_or(' ');
                    format!("{}{}", first, second).to_uppercase()
                } else {
                    m.from_name.chars().take(2).collect::<String>().to_uppercase()
                }
            };

            RowPayload {
                id: m.id,
                account_id: m.account_id,
                folder_id: m.folder_id,
                uid: m.uid,
                from_name: m.from_name.clone(),
                from_addr: m.from_addr.clone(),
                subject: m.subject.clone(),
                preview: m.preview.clone(),
                date_display: m.date.clone(),
                unread: m.unread,
                starred: m.starred,
                has_attachment: m.has_attachment,
                initials,
            }
        })
        .collect()
}
