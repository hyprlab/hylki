//! Free helpers for the account editor's form: filling it from an account,
//! reading an account back from it, and the rows that adapt to the choices.

use super::providers::{
    manual_index, protocol_at, protocol_index, provider_at, provider_index_for_account,
    ProviderKind,
};
use super::*;

/// Stop a row's subtitle from squeezing out the row's value.
///
/// An [`adw::ActionRow`] gives its title block whatever width the text asks
/// for, and an [`adw::ComboRow`]'s value label takes what is left — which for
/// a subtitle of any length is an ellipsis where "Disabled" should be. Capping
/// the label's width in characters bounds what it can ask for, so it wraps to
/// a second line instead of taking the room from its neighbour. `subtitle-lines`
/// does not do this: it caps how many lines are drawn, not how wide one is.
pub(super) fn wrap_subtitle(row: &impl IsA<gtk::Widget>, chars: i32) {
    fn walk(w: &gtk::Widget, chars: i32) -> bool {
        if let Some(label) = w.downcast_ref::<gtk::Label>() {
            if label.has_css_class("subtitle") {
                label.set_wrap(true);
                label.set_max_width_chars(chars);
                return true;
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, chars) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    walk(row.as_ref(), chars);
}

/// The subtitle under "Save a copy of sent mail in" (#199). Says the one thing
/// the row above it cannot: this only files mail somewhere, so the Inbox is a
/// fair answer here even though giving it the Sent *role* would cost the
/// account its inbox. What "Disabled" leaves the copy to is left to the title,
/// which already says "a copy": the originals are the Sent row's business.
/// Declared once because the row is rebuilt whenever an editor opens, and the
/// two copies drifting apart is how a hint goes stale.
pub(super) const SENT_COPY_HINT: &str =
    "Any folder, inbox included. Only applies to mail sent from this Hylki client \
     going forward. Not recursive.";

pub(super) const REPLY_COPIES_HINT: &str =
    "Where the message replied to is when the reply is sent. Replies to mail in Sent, \
     Drafts, Trash or Junk go with sent mail.";

/// Whether a list row survives the page's search text (#141): matched
/// against the row's title and, for an action row, its subtitle.
pub(super) fn row_matches(row: &gtk::ListBoxRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let title = row
        .downcast_ref::<adw::PreferencesRow>()
        .map(|r| r.title().to_lowercase())
        .unwrap_or_default();
    let subtitle = row
        .downcast_ref::<adw::ActionRow>()
        .and_then(|r| r.subtitle())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    title.contains(query) || subtitle.contains(query)
}

/// A list-item factory whose labels never ellipsize, so a `ComboRow` popup grows
/// to fit its longest option instead of truncating it.
pub(crate) fn non_ellipsizing_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::None);
            label.set_margin_start(6);
            label.set_margin_end(6);
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|o| o.string())
                .unwrap_or_default();
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        }
    });
    factory
}

pub(super) fn display_name(acc: &AccountConfig) -> String {
    if acc.name.trim().is_empty() {
        acc.email.clone()
    } else {
        acc.name.clone()
    }
}

/// The side of the square copy an avatar is stored as: sharp in a 30px
/// disc on a 2x display, small on disk.
const AVATAR_PX: i32 = 256;

/// Copy a chosen image in as an account avatar (#162): oriented by its
/// EXIF tag, scaled to cover a square, centre-cropped and saved as PNG
/// under the avatars directory with a fresh name, which is returned.
pub(super) fn import_avatar_file(src: &std::path::Path) -> Result<String, String> {
    use gtk::gdk_pixbuf::{InterpType, Pixbuf};
    let dir = crate::config::avatars_dir().ok_or("no data directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pb = Pixbuf::from_file(src).map_err(|e| e.to_string())?;
    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
    let (w, h) = (pb.width().max(1), pb.height().max(1));
    // Scale so the shorter side is AVATAR_PX (an image smaller than that
    // is left at its size), then cut the longer side down to a square.
    let side = AVATAR_PX.min(w.min(h));
    let (sw, sh) = if w <= h {
        (
            side,
            (h as f64 * side as f64 / w as f64).round().max(side as f64) as i32,
        )
    } else {
        (
            (w as f64 * side as f64 / h as f64).round().max(side as f64) as i32,
            side,
        )
    };
    let scaled = pb
        .scale_simple(sw, sh, InterpType::Bilinear)
        .ok_or("could not scale the image")?;
    let square = scaled.new_subpixbuf((sw - side) / 2, (sh - side) / 2, side, side);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("avatar-{stamp}.png");
    square
        .savev(dir.join(&name), "png", &[])
        .map_err(|e| e.to_string())?;
    Ok(name)
}

/// Build an `AccountConfig` from the current editor form values.
pub(super) fn read_account(
    widgets: &AccountsWindowWidgets,
    emoji: Option<String>,
    avatar: Option<String>,
) -> AccountConfig {
    let protocol = form_protocol(widgets);
    let default_port = match protocol {
        Protocol::Pop3 => 995,
        Protocol::Jmap => 443,
        _ => 993,
    };
    let jmap_token = protocol == Protocol::Jmap && widgets.form.jmap_token_row.is_active();
    let jmap_generic = provider_at(widgets.form.provider_row.selected()).wizard_jmap_generic();
    AccountConfig {
        name: trimmed(&widgets.form.name_row),
        email: trimmed(&widgets.form.email_row),
        protocol,
        imap_host: trimmed(&widgets.form.host_row),
        imap_port: trimmed(&widgets.form.port_row)
            .parse()
            .unwrap_or(default_port),
        smtp_host: trimmed(&widgets.form.smtp_row),
        smtp_port: trimmed(&widgets.form.smtp_port_row).parse().unwrap_or(587),
        // A token sign-in asks for no username; the address stands in.
        username: if jmap_token && trimmed(&widgets.form.user_row).is_empty() {
            trimmed(&widgets.form.email_row)
        } else {
            trimmed(&widgets.form.user_row)
        },
        password: widgets.form.pass_row.text().to_string(),
        smtp_separate: widgets.form.smtp_separate_row.is_active(),
        jmap_token,
        jmap_generic,
        tls_accept_hostname_mismatch: widgets.form.tls_mismatch_row.is_active(),
        // GNOME Online Accounts' own, carried over from the account edited.
        security: None,
        smtp_username: trimmed(&widgets.form.smtp_user_row),
        smtp_password: widgets.form.smtp_pass_row.text().to_string(),
        color: Some(crate::color::to_hex(&widgets.form.color_btn.rgba())),
        emoji,
        avatar,
        gravatar: widgets.form.gravatar_row.is_active(),
        // Filled in by SaveWithSig from the rich-text editor.
        signature: None,
        signature_html: true,
        signatures: Vec::new(),
        default_signature: None,
        // Only store a custom label; blank or same-as-email falls back to email.
        label: {
            let l = trimmed(&widgets.form.label_row);
            let email = trimmed(&widgets.form.email_row);
            if l.is_empty() || l == email {
                None
            } else {
                Some(l)
            }
        },
        // The alias list is model state (alias_edits), assigned by SaveWithSig.
        aliases: Vec::new(),
        // Defaults for a new account; preserved from the original when editing.
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: match widgets.form.push_row.selected() {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        },
        // Assigned by SaveWithSig from the Special Folders combos.
        folder_roles: Default::default(),
        // Likewise from the Hidden Folders list, and carried over from
        // the account being edited.
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        reply_copies: crate::config::ReplyCopies::ALL
            .get(widgets.form.reply_copies_row.selected() as usize)
            .copied()
            .unwrap_or_default(),
        empty_junk_days: AUTO_EMPTY_DAYS
            .get(widgets.form.empty_junk_row.selected() as usize)
            .copied()
            .unwrap_or(0),
        empty_trash_days: AUTO_EMPTY_DAYS
            .get(widgets.form.empty_trash_row.selected() as usize)
            .copied()
            .unwrap_or(0),
        clean_lists_days: CLEAN_LISTS_DAYS
            .get(widgets.form.clean_lists_row.selected() as usize)
            .copied()
            .unwrap_or(0),
        offline_mail: crate::config::OfflineMail::ALL
            .get(widgets.form.offline_mail_row.selected() as usize)
            .copied()
            .unwrap_or_default(),
        offline_attachments: widgets.form.offline_attachments_row.is_active(),
        copy_to_local: widgets.form.copy_local_row.is_active(),
        // Row 0 is Automatic; the rest follow PGP_KEY_CHOICES. Until gpg
        // has answered, the account keeps the key it had.
        pgp_key: PGP_KEY_CHOICES.with(|c| {
            let c = c.borrow();
            if !c.loaded {
                return c.chosen.clone();
            }
            let sel = widgets.form.pgp_key_row.selected() as usize;
            sel.checked_sub(1)
                .and_then(|i| c.fingerprints.get(i).cloned())
        }),
        in_unified: widgets.form.in_unified_row.is_active(),
        // Row 0 follows Settings; the rest follow FolderSort::ALL.
        folder_sort: widgets
            .form
            .folder_sort_row
            .selected()
            .checked_sub(1)
            .and_then(|i| crate::config::FolderSort::ALL.get(i as usize).copied()),
        sign_by_default: widgets.form.sign_default_row.is_active(),
        plain_subject: !widgets.form.hide_subject_row.is_active(),
        graph_directory: protocol == Protocol::Graph
            && widgets.form.graph_directory_row.is_active(),
        graph_directory_only: protocol == Protocol::Graph
            && widgets.form.directory_only_row.is_active(),
        graph_label_names: protocol == Protocol::Graph && widgets.form.graph_labels_row.is_active(),
    }
}

/// The editor's OpenPGP combo (#133): the fingerprints behind its rows, and
/// the account's key while gpg is still listing.
struct PgpKeyChoices {
    /// Row `i + 1` holds `fingerprints[i]`; row 0 is Automatic.
    fingerprints: Vec<String>,
    chosen: Option<String>,
    /// The list for the editor now open has arrived.
    loaded: bool,
    /// Bumped per fill, so an answer for an editor since left is dropped.
    generation: u64,
}

thread_local! {
    static PGP_KEY_CHOICES: std::cell::RefCell<PgpKeyChoices> = const {
        std::cell::RefCell::new(PgpKeyChoices { fingerprints: Vec::new(), chosen: None, loaded: true, generation: 0 })
    };
}

/// Fill the OpenPGP key combo with the user's own keys and select the
/// account's, or Automatic.
///
/// gpg is asked on a worker thread: a keyring lock left by a stale gpg
/// makes every call wait about ten seconds, which froze the window each
/// time an editor opened (#316).
fn fill_pgp_key_row(widgets: &AccountsWindowWidgets, chosen: Option<&str>) {
    let row = widgets.form.pgp_key_row.clone();
    row.set_model(Some(&gtk::StringList::new(&[&i18n("Automatic")])));
    row.set_list_factory(Some(&non_ellipsizing_factory()));
    row.set_selected(0);
    row.set_sensitive(false);
    let chosen = chosen.map(str::to_string);
    let generation = PGP_KEY_CHOICES.with(|c| {
        let mut c = c.borrow_mut();
        c.fingerprints.clear();
        c.chosen = chosen.clone();
        c.loaded = false;
        c.generation += 1;
        c.generation
    });
    gtk::glib::spawn_future_local(async move {
        let keys = gtk::gio::spawn_blocking(|| {
            if !crate::mail::pgp::available() {
                return Vec::new();
            }
            crate::mail::pgp::list_keys(&crate::mail::pgp::Gpg::system(), true)
                .into_iter()
                .filter(|k| k.usable() && k.can_sign)
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if PGP_KEY_CHOICES.with(|c| c.borrow().generation) != generation {
            return;
        }
        let mut labels = vec![i18n("Automatic")];
        let mut fingerprints: Vec<String> = Vec::new();
        for k in &keys {
            labels.push(format!(
                "{} ({})",
                k.primary_uid(),
                crate::mail::pgp::key_display(&k.key_id)
            ));
            fingerprints.push(k.fingerprint.clone());
        }
        // A chosen key gpg did not list (it failed, or the key went) stays
        // a row of its own, so saving the editor does not drop it.
        if let Some(f) = chosen.as_deref() {
            if !fingerprints.iter().any(|k| k.eq_ignore_ascii_case(f)) {
                labels.push(crate::mail::pgp::key_display(f));
                fingerprints.push(f.to_string());
            }
        }
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&refs)));
        let selected = chosen
            .as_deref()
            .and_then(|f| fingerprints.iter().position(|k| k.eq_ignore_ascii_case(f)))
            .map(|i| i as u32 + 1)
            .unwrap_or(0);
        row.set_selected(selected);
        row.set_sensitive(true);
        PGP_KEY_CHOICES.with(|c| {
            let mut c = c.borrow_mut();
            c.fingerprints = fingerprints;
            c.loaded = true;
        });
    });
}

/// Auto-empty choices (#140), in combo order: never, then the ages.
const AUTO_EMPTY_DAYS: &[u32] = &[0, 7, 14, 30];

/// The combo index for a stored age (an unknown value shows as "Never").
fn auto_empty_index(days: u32) -> u32 {
    AUTO_EMPTY_DAYS.iter().position(|d| *d == days).unwrap_or(0) as u32
}

/// Moving list mail to the Trash (#407): never, then the ages.
pub(super) const CLEAN_LISTS_DAYS: &[u32] = &[0, 30, 60, 90, 180];

/// The combo's labels, translated.
pub(super) fn auto_empty_labels() -> Vec<String> {
    age_labels(AUTO_EMPTY_DAYS)
}

/// "Never", then "After N days" for each age.
pub(super) fn age_labels(days: &[u32]) -> Vec<String> {
    days.iter()
        .map(|d| {
            if *d == 0 {
                i18n("Never")
            } else {
                crate::i18n::ni18n_f(
                    "After {n} day",
                    "After {n} days",
                    *d,
                    &[("n", &d.to_string())],
                )
            }
        })
        .collect()
}

/// Left-justify every wrapping label under `root`. Works around a libadwaita
/// 1.9 quirk where an `AdwPreferencesGroup` description renders fill-justified
/// (stretched word gaps) whenever its text is shorter than the label's width;
/// row titles and other single-line labels don't wrap, so they're left alone.
pub(super) fn left_justify_wrapping_labels(root: &gtk::Widget) {
    if let Some(label) = root.downcast_ref::<gtk::Label>() {
        if label.wraps() {
            label.set_justify(gtk::Justification::Left);
        }
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        left_justify_wrapping_labels(&c);
        child = c.next_sibling();
    }
}

/// Put the editor page into the navigation view if it isn't there (it is
/// left out until first needed — see the panel's init).
pub(super) fn mount_editor(widgets: &AccountsWindowWidgets) {
    if widgets.editor_page.parent().is_none() {
        widgets.nav.add(&widgets.editor_page);
    }
}

/// Show the storage row with the server's figures (#298), or hide it.
pub(super) fn show_quota(
    widgets: &AccountsWindowWidgets,
    quota: Option<crate::models::MailboxQuota>,
) {
    let Some(q) = quota else {
        widgets.form.quota_group.set_visible(false);
        return;
    };
    widgets
        .form
        .quota_row
        .set_subtitle(&format!("{} \u{b7} {}", q.used_line(), q.free_line()));
    // GTK's own offsets mark a *low* value as the warning, which is a fuel
    // gauge's sense, not a mailbox's: here the warning is a full one.
    let bar = &widgets.form.quota_bar;
    for name in [
        "low",
        "high",
        "full",
        "quota-ok",
        "quota-warn",
        "quota-full",
    ] {
        bar.remove_offset_value(Some(name));
    }
    bar.add_offset_value("quota-ok", 0.9);
    bar.add_offset_value("quota-warn", 0.98);
    bar.add_offset_value("quota-full", 1.0);
    bar.set_value(q.fraction());
    widgets.form.quota_group.set_visible(true);
}

pub(super) fn fill_editor(widgets: &AccountsWindowWidgets, acc: &AccountConfig) {
    widgets.form.name_row.set_text(&acc.name);
    widgets.form.email_row.set_text(&acc.email);
    // Reflect the account's provider in the dropdown (OAuth by endpoint, known
    // password providers by server, otherwise "IMAP/POP3 Account").
    widgets
        .form
        .provider_row
        .set_selected(provider_index_for_account(acc));
    widgets
        .form
        .protocol_row
        .set_selected(protocol_index(acc.protocol));
    widgets.form.jmap_token_row.set_active(acc.jmap_token);
    widgets.form.host_row.set_text(&acc.imap_host);
    widgets.form.port_row.set_text(&acc.imap_port.to_string());
    widgets.form.smtp_row.set_text(&acc.smtp_host);
    widgets
        .form
        .smtp_port_row
        .set_text(&acc.smtp_port.to_string());
    widgets.form.user_row.set_text(&acc.username);
    widgets.form.pass_row.set_text(&acc.password);
    widgets.form.smtp_separate_row.set_active(acc.smtp_separate);
    widgets
        .form
        .tls_mismatch_row
        .set_active(acc.tls_accept_hostname_mismatch);
    widgets.form.smtp_user_row.set_text(&acc.smtp_username);
    widgets.form.smtp_pass_row.set_text(&acc.smtp_password);
    widgets.form.push_row.set_selected(match acc.push {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    });
    widgets
        .form
        .empty_junk_row
        .set_selected(auto_empty_index(acc.empty_junk_days));
    widgets
        .form
        .empty_trash_row
        .set_selected(auto_empty_index(acc.empty_trash_days));
    widgets.form.clean_lists_row.set_selected(
        CLEAN_LISTS_DAYS
            .iter()
            .position(|d| *d == acc.clean_lists_days)
            .unwrap_or(0) as u32,
    );
    widgets
        .form
        .offline_mail_row
        .set_selected(acc.offline_mail.index());
    widgets
        .form
        .offline_attachments_row
        .set_active(acc.offline_attachments);
    widgets.form.copy_local_row.set_active(acc.copy_to_local);
    fill_pgp_key_row(widgets, acc.pgp_key.as_deref());
    widgets.form.in_unified_row.set_active(acc.in_unified);
    widgets
        .form
        .folder_sort_row
        .set_selected(acc.folder_sort.map_or(0, |s| s.index() + 1));
    widgets
        .form
        .sign_default_row
        .set_active(acc.sign_by_default);
    widgets.form.hide_subject_row.set_active(!acc.plain_subject);
    widgets
        .form
        .reply_copies_row
        .set_selected(acc.reply_copies.index());
    // Show the effective label (custom, or the email address).
    widgets
        .form
        .label_row
        .set_text(acc.label.as_deref().unwrap_or(&acc.email));

    // OAuth client detail fields. GOA accounts (goa_id set) can't be
    // re-authenticated here, so they show as password (their mechanism is kept on
    // save); natively-added OAuth accounts show their client details.
    let s = (acc.goa_id.is_none() && acc.oauth)
        .then_some(acc.oauth_settings.as_ref())
        .flatten();
    widgets
        .form
        .oauth_client_id_row
        .set_text(s.map(|s| s.client_id.as_str()).unwrap_or(""));
    widgets
        .form
        .oauth_secret_row
        .set_text(s.map(|s| s.client_secret.as_str()).unwrap_or(""));
    widgets
        .form
        .oauth_auth_url_row
        .set_text(s.map(|s| s.auth_url.as_str()).unwrap_or(""));
    widgets
        .form
        .oauth_token_url_row
        .set_text(s.map(|s| s.token_url.as_str()).unwrap_or(""));
    widgets
        .form
        .oauth_scope_row
        .set_text(s.map(|s| s.scopes.as_str()).unwrap_or(""));
    // What a Microsoft account was set up with beyond Hylki's own app (#329).
    let adv = s
        .map(crate::services::oauth::microsoft_advanced_of)
        .unwrap_or_default();
    widgets.form.ms_client_row.set_text(&adv.0);
    widgets.form.ms_tenant_row.set_text(&adv.1);
    widgets.form.ms_scopes_row.set_text(&adv.2);
    widgets.form.ms_redirect_row.set_text(&adv.3);
    widgets.form.ms_advanced.set_expanded(
        !(adv.0.is_empty() && adv.1.is_empty() && adv.2.is_empty() && adv.3.is_empty()),
    );
    widgets.form.oauth_status.set_visible(false);
    widgets.form.oauth_signin_btn.set_sensitive(true);

    // Signature is loaded into the rich-text editor by the caller.
    widgets.form.test_result.set_visible(false);
    widgets.form.test_btn.set_sensitive(true);
}

/// Grey out everything GNOME Online Accounts owns.
///
/// An account imported from GOA takes its address, servers, protocol and
/// credentials from the system; editing them here would either be overwritten
/// the next time GOA is read, or quietly disagree with what the rest of the
/// desktop uses. What stays editable is what Hylki owns: the sender's display
/// name, signature, color, emoji and label.
pub(super) fn set_connection_editable(widgets: &AccountsWindowWidgets, editable: bool) {
    for row in [
        widgets.form.email_row.upcast_ref::<gtk::Widget>(),
        widgets.form.host_row.upcast_ref(),
        widgets.form.port_row.upcast_ref(),
        widgets.form.smtp_row.upcast_ref(),
        widgets.form.smtp_port_row.upcast_ref(),
        widgets.form.user_row.upcast_ref(),
        widgets.form.pass_row.upcast_ref(),
        widgets.form.smtp_user_row.upcast_ref(),
        widgets.form.smtp_pass_row.upcast_ref(),
    ] {
        row.set_sensitive(editable);
    }
    widgets.form.provider_row.set_sensitive(editable);
    widgets.form.protocol_row.set_sensitive(editable);
    widgets.form.smtp_separate_row.set_sensitive(editable);
    widgets.form.tls_mismatch_row.set_sensitive(editable);
    // OAuth client details belong to a natively-added account; a GOA one gets its
    // tokens from the system.
    for row in [
        widgets.form.oauth_client_id_row.upcast_ref::<gtk::Widget>(),
        widgets.form.oauth_secret_row.upcast_ref(),
        widgets.form.oauth_auth_url_row.upcast_ref(),
        widgets.form.oauth_token_url_row.upcast_ref(),
        widgets.form.oauth_scope_row.upcast_ref(),
        widgets.form.ms_advanced.upcast_ref(),
    ] {
        row.set_sensitive(editable);
    }
    widgets.form.oauth_signin_btn.set_sensitive(editable);
    // Testing stays available: it is read-only, and confirming that the imported
    // settings actually connect is exactly what someone would want here.
}

pub(super) fn clear_editor(widgets: &AccountsWindowWidgets) {
    widgets.form.name_row.set_text("");
    widgets.form.email_row.set_text("");
    widgets.form.provider_row.set_selected(manual_index());
    widgets.form.protocol_row.set_selected(0);
    widgets.form.jmap_token_row.set_active(false);
    widgets.form.host_row.set_text("");
    widgets.form.port_row.set_text("993");
    widgets.form.smtp_row.set_text("");
    widgets.form.smtp_port_row.set_text("587");
    widgets.form.user_row.set_text("");
    widgets.form.pass_row.set_text("");
    widgets.form.smtp_separate_row.set_active(false);
    widgets.form.tls_mismatch_row.set_active(false);
    widgets.form.push_row.set_selected(0);
    widgets.form.empty_junk_row.set_selected(0);
    widgets.form.empty_trash_row.set_selected(0);
    widgets.form.clean_lists_row.set_selected(0);
    widgets.form.offline_mail_row.set_selected(0);
    widgets.form.offline_attachments_row.set_active(false);
    widgets.form.copy_local_row.set_active(false);
    fill_pgp_key_row(widgets, None);
    widgets.form.in_unified_row.set_active(true);
    widgets.form.folder_sort_row.set_selected(0);
    widgets.form.sign_default_row.set_active(false);
    widgets.form.hide_subject_row.set_active(true);
    widgets.form.reply_copies_row.set_selected(0);
    widgets.form.graph_directory_row.set_active(false);
    widgets.form.directory_only_row.set_active(false);
    widgets.form.graph_status.set_visible(false);
    widgets.form.graph_labels_row.set_active(false);
    widgets.form.smtp_user_row.set_text("");
    widgets.form.smtp_pass_row.set_text("");
    widgets.form.label_row.set_text("");
    widgets.form.oauth_client_id_row.set_text("");
    widgets.form.oauth_secret_row.set_text("");
    widgets.form.oauth_auth_url_row.set_text("");
    widgets.form.oauth_token_url_row.set_text("");
    widgets.form.oauth_scope_row.set_text("");
    for row in [
        &widgets.form.ms_client_row,
        &widgets.form.ms_tenant_row,
        &widgets.form.ms_scopes_row,
        &widgets.form.ms_redirect_row,
    ] {
        row.set_text("");
    }
    widgets.form.ms_advanced.set_expanded(false);
    widgets.form.oauth_status.set_visible(false);
    widgets.form.test_result.set_visible(false);
    widgets.form.test_btn.set_sensitive(true);
}

pub(super) fn trimmed(row: &impl IsA<gtk::Editable>) -> String {
    row.text().trim().to_string()
}

/// The protocol the form stands for: the provider's own for one that has
/// one (the JMAP entries'), otherwise the Incoming Protocol row's.
pub(super) fn form_protocol(widgets: &AccountsWindowWidgets) -> Protocol {
    match provider_at(widgets.form.provider_row.selected()).kind {
        ProviderKind::Jmap => Protocol::Jmap,
        // Hylki's own Microsoft sign-in asks for Graph, not IMAP (#329).
        ProviderKind::Microsoft => Protocol::Graph,
        _ => protocol_at(widgets.form.protocol_row.selected()),
    }
}

/// Adapt the editor to the Incoming Protocol row: a JMAP account sends
/// through its server, so the SMTP rows go, and the server row takes a host
/// or a URL rather than an IMAP host and port.
pub(super) fn apply_protocol(widgets: &AccountsWindowWidgets) {
    let jmap = form_protocol(widgets) == Protocol::Jmap;
    // Only where the servers are shown at all (OAuth providers hide them).
    let servers_shown = widgets.form.host_row.is_visible();
    widgets.form.smtp_row.set_visible(servers_shown && !jmap);
    widgets
        .form
        .smtp_port_row
        .set_visible(servers_shown && !jmap);
    widgets
        .form
        .smtp_separate_row
        .set_visible(widgets.form.protocol_row.is_visible() && !jmap);
    // JMAP runs over a different TLS stack (rustls), which offers no such waiver.
    widgets
        .form
        .tls_mismatch_row
        .set_visible(widgets.form.protocol_row.is_visible() && !jmap);
    // A token stands in for the username and password (#356); the rows
    // only change where the password is asked for at all.
    let password_shown = widgets.form.pass_row.is_visible();
    widgets
        .form
        .jmap_token_row
        .set_visible(servers_shown && jmap && password_shown);
    let token = jmap && widgets.form.jmap_token_row.is_active();
    if password_shown {
        widgets.form.user_row.set_visible(!token);
        widgets.form.pass_row.set_title(&if token {
            i18n("API Token")
        } else {
            i18n("Password")
        });
    }
    if jmap {
        widgets.form.smtp_separate_row.set_active(false);
        widgets
            .form
            .host_row
            .set_title(&i18n("Server (host name or URL)"));
        widgets.form.port_row.set_title(&i18n("Port (default 443)"));
    } else {
        widgets.form.host_row.set_title(&i18n("Incoming Server"));
        widgets
            .form
            .port_row
            .set_title(&i18n("Port (IMAP 993 / POP3 995)"));
    }
}

pub(super) fn parse_color(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or_else(|_| gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_choices_map_to_combo_indices_and_labels() {
        assert_eq!(auto_empty_index(0), 0);
        assert_eq!(auto_empty_index(7), 1);
        assert_eq!(auto_empty_index(30), 3);
        assert_eq!(auto_empty_index(5), 0);
        assert_eq!(
            age_labels(&[0, 1, 30]),
            ["Never", "After 1 day", "After 30 days"]
        );
        assert_eq!(auto_empty_labels().len(), AUTO_EMPTY_DAYS.len());
        assert_eq!(CLEAN_LISTS_DAYS, [0, 30, 60, 90, 180]);
    }
}
