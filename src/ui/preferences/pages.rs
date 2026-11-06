//! The settings window's category pages, one builder each.
//!
//! Each page is a standalone `relm4::view!` run from `Preferences::init`
//! before the window's own view, which places the page in its stack with
//! `#[local_ref]`; the rows the window reaches later are kept in the page's
//! struct (`widgets.general.fetch_row`). A standalone view has no update
//! step, so a property the window's view once marked `#[watch]` is set here
//! when the page is built and again by the page's `update_view`, which the
//! component's `pre_view` calls on every update, in the order the single
//! view had them.

mod appearance;
mod assistants;
mod backup;
mod composing;
mod conversations;
mod datetime;
mod focus;
mod general;
mod list;
mod privacy;
mod reading;
mod sidebar;
mod system;

pub(super) use appearance::AppearancePage;
pub(super) use assistants::AssistantsPage;
pub(super) use backup::BackupPage;
pub(super) use composing::ComposingPage;
pub(super) use conversations::ConversationsPage;
pub(super) use datetime::DateTimePage;
pub(super) use focus::FocusPage;
pub(super) use general::GeneralPage;
pub(super) use list::ListPage;
pub(super) use privacy::PrivacyPage;
pub(super) use reading::ReadingPage;
pub(super) use sidebar::SidebarPage;
pub(super) use system::SystemPage;
