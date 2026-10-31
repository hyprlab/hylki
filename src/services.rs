//! Accounts and remote services: signing in (OAuth, GNOME Online Accounts,
//! Microsoft's broker), Microsoft Graph, cloud storage, translation, grammar
//! checking, and the pictures fetched for senders.

pub(crate) mod avatar;
pub(crate) mod cloud;
pub(crate) mod goa;
pub(crate) mod grammar;
pub(crate) mod graph_api;
pub(crate) mod graph_labels;
pub(crate) mod logo;
pub(crate) mod ms_broker;
pub(crate) mod oauth;
pub(crate) mod remote_content;
pub(crate) mod translate;
