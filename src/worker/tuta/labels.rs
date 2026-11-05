//! Tags as Tuta labels. A tag is put on a message as the label named like
//! it, as on Gmail (#374): the tag's name, else its keyword. A label is
//! made the first time a tag needs one, in the tag's color, and a tag
//! comes off as every label that stands for it, its name's and its
//! keyword's. Listings carry each message's labels by name (see
//! `header_labels`), which is how a tag set in Tuta's own app shows here.

use super::TutaWorker;
use crate::config::{Tag, TAG_COLORS};
use crate::i18n::i18n_f;
use crate::models::KeywordFinding;
use crate::worker::helper::HelperError;
use crate::worker::tuta::index::run;
use crate::worker::tuta::wire::{apply_labels, create_label, LabelRow};
use crate::worker::WorkerEvent;

/// The label a tag goes by: (name, color). The name is the tag's, else
/// its keyword; the color the tag's when it is one Tuta takes.
pub(super) fn label_for(keyword: &str, tags: &[Tag]) -> (String, String) {
    let tag = tags.iter().find(|t| t.keyword.eq_ignore_ascii_case(keyword));
    let name = tag
        .map(|t| t.name.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or(keyword)
        .to_string();
    let color = tag
        .map(|t| t.color.clone())
        .filter(|c| is_color(c))
        .unwrap_or_else(|| TAG_COLORS[0].to_string());
    (name, color)
}

/// Whether `color` is `#rrggbb`.
fn is_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// The labels that stand for a tag: the one named like it, and one named
/// like its keyword.
pub(super) fn standing_for<'a>(
    labels: impl Iterator<Item = &'a LabelRow>,
    name: &str,
    keyword: &str,
) -> Vec<&'a LabelRow> {
    labels
        .filter(|l| l.name.eq_ignore_ascii_case(name) || l.name.eq_ignore_ascii_case(keyword))
        .collect()
}

/// The keyword a label is found as (#166): the tag's whose name or keyword
/// it carries, else its own name, as Microsoft 365's categories are.
pub(super) fn keyword_of(label: &LabelRow, tags: &[Tag]) -> String {
    tags.iter()
        .find(|t| {
            t.name.trim().eq_ignore_ascii_case(&label.name)
                || t.keyword.eq_ignore_ascii_case(&label.name)
        })
        .map_or_else(|| label.name.clone(), |t| t.keyword.clone())
}

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// Put a tag on a message or take it off, as labels on the server. A
    /// tag Hylki kept on this computer before Tuta labels were reachable
    /// comes off here too.
    pub(super) async fn on_set_keyword(
        &mut self,
        path: String,
        uid: u32,
        message_id: String,
        keyword: String,
        add: bool,
    ) {
        let account_id = self.account_id;
        let (name, color) = label_for(&keyword, &crate::config::load_tags());
        let labeled = self.label(&path, uid, &name, &keyword, &color, add).await;
        match labeled {
            Ok(names) => {
                if let Some(c) = self.cache.as_ref() {
                    let names: Vec<String> = names
                        .iter()
                        .map(|n| crate::mail::mutf7::encode(n))
                        .collect();
                    c.set_gmail_label(account_id, &path, uid, &names, add);
                    if !add {
                        c.set_keyword(account_id, &path, uid, &keyword, false);
                        if !message_id.is_empty() {
                            c.set_local_tag(account_id, &message_id, &keyword, false);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[account {account_id}] could not {} a tag in {path}: {}",
                    if add { "put on" } else { "take off" },
                    e.code
                );
                (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not tag message: {e}",
                    &[("e", &e.message())],
                )));
            }
        }
        (self.emit)(WorkerEvent::TagSettled {
            path,
            uid,
            keyword,
        });
    }

    /// Put the label `name` on the message `uid` in `path`, made first in
    /// `color` if there is none, or take off every label standing for the
    /// tag. The names put on or taken off.
    async fn label(
        &mut self,
        path: &str,
        uid: u32,
        name: &str,
        keyword: &str,
        color: &str,
        add: bool,
    ) -> Result<Vec<String>, HelperError> {
        let standing: Vec<LabelRow> = standing_for(self.state.labels.values(), name, keyword)
            .into_iter()
            .cloned()
            .collect();
        if !add && standing.is_empty() {
            return Ok(Vec::new());
        }
        let id = self.resolve(path, uid).await?;
        let client = self.state.client(&self.account)?;
        let (put, taken) = if add {
            let label = match standing.first() {
                Some(label) => label.clone(),
                None => {
                    let (c, n, col) = (client.clone(), name.to_string(), color.to_string());
                    let made = run(move || create_label(&c, &n, &col)).await?;
                    let label = LabelRow {
                        id: made,
                        name: name.to_string(),
                        color: color.to_string(),
                    };
                    self.state.labels.insert(label.id.clone(), label.clone());
                    label
                }
            };
            (vec![label], Vec::new())
        } else {
            (Vec::new(), standing)
        };
        let ids = |labels: &[LabelRow]| labels.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
        let (add_ids, remove_ids) = (ids(&put), ids(&taken));
        let (_, failed) = run(move || Ok(apply_labels(&client, &[id], &add_ids, &remove_ids)))
            .await
            .unwrap_or_else(|e| (Vec::new(), Some(e)));
        if let Some(e) = failed {
            return Err(e);
        }
        Ok(put.iter().chain(&taken).map(|l| l.name.clone()).collect())
    }

    /// The account's labels, as the tags they stand for (#166). Counts are
    /// left out: only the listings know them.
    pub(super) fn on_find_keywords(&self) {
        let tags = crate::config::load_tags();
        let found = self
            .state
            .labels
            .values()
            .map(|l| KeywordFinding {
                keyword: keyword_of(l, &tags),
                name: Some(l.name.clone()),
                color: Some(l.color.clone()).filter(|c| is_color(c)),
                count: 0,
                folders: Vec::new(),
            })
            .collect();
        (self.emit)(WorkerEvent::KeywordsFound(found));
    }
}
