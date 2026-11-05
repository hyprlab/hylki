//! Folder, label, move and permanent-delete operations, each mirroring the
//! official client's `MailFacade`/`MailModel` (tutanota
//! `src/applications/common/api/worker/facades/lazy/MailFacade.ts` and
//! `src/applications/mail-app/mail/model/MailModel.ts`).
//!
//! Folders and labels are both `MailSet`s: a label is one of folder type 8.
//! Their names are encrypted with a fresh session key that is itself wrapped
//! with the mail group key, exactly as the official client creates them; the
//! SDK's service executor does the encryption (`ExtraServiceParams.session_key`).
use super::{diag, id, resume, tuple, Result, Session, MAIL_BATCH_LIMIT, TRASH_FOLDER_TYPE};
use crypto_primitives::{
    aes::{Aes256Key, InitializationVector},
    key::GenericAesKey,
    randomizer_facade::RandomizerFacade,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tutasdk::{
    entities::generated::tutanota::{
        ApplyLabelServicePostIn, CreateMailFolderData, DeleteMailData, DeleteMailFolderData,
        MailSet, MailSetEntry, ManageLabelServiceDeleteIn, ManageLabelServiceLabelData,
        ManageLabelServicePostIn, ManageLabelServicePutIn, MoveMailData, UpdateMailFolderData,
    },
    rest_error::HttpError,
    services::{
        generated::tutanota::{
            ApplyLabelService, MailFolderService, MailService, ManageLabelService, MoveMailService,
        },
        ExtraServiceParams,
    },
    ApiCallError, GeneratedId, IdTupleGenerated, ListLoadDirection, LoggedInSdk, Sdk,
};

// TS `MailSetKind`.
pub(crate) const CUSTOM_FOLDER_TYPE: i64 = 0;
pub(crate) const SPAM_FOLDER_TYPE: i64 = 5;
pub(crate) const LABEL_FOLDER_TYPE: i64 = 8;
pub(crate) const IMPORTED_FOLDER_TYPE: i64 = 9;
/// TS `MAX_LABELS_PER_MAIL`: the official client never applies more.
const MAX_LABELS_PER_MAIL: usize = 5;
/// Longest folder or label name accepted, in characters. The official client
/// sets no explicit limit; this only keeps a pathological request off the
/// wire.
const MAX_NAME_CHARS: usize = 256;
/// A folder tree deeper than this is malformed; parent walks stop here.
const MAX_FOLDER_DEPTH: usize = 64;

/// The fixed code for a failed SDK call. Session, version and transport
/// classes keep their shared codes; a missing instance reports `not_found`;
/// everything else reports the operation's own `fallback`.
pub(crate) fn api_code(
    error: ApiCallError,
    fallback: &'static str,
    not_found: &'static str,
) -> &'static str {
    match error {
        ApiCallError::RestClient { .. } => "transport_failed",
        ApiCallError::ServerResponseError { source } => match source {
            HttpError::InvalidSoftwareVersionError => "client_outdated",
            HttpError::SessionExpiredError
            | HttpError::NotAuthenticatedError
            | HttpError::AccessExpiredError
            | HttpError::AccessDeactivatedError
            | HttpError::AccessBlockedError => "session_revoked",
            HttpError::NotFoundError => not_found,
            HttpError::RequestTimeoutError => "timeout",
            HttpError::LimitReachedError => "limit_reached",
            HttpError::ConnectionError
            | HttpError::InternalServerError
            | HttpError::BadGatewayError
            | HttpError::ServiceUnavailableError { .. }
            | HttpError::TooManyRequestsError { .. }
            | HttpError::LockedError => "transport_failed",
            _ => fallback,
        },
        ApiCallError::InternalSdkError { .. } => fallback,
    }
}

/// `api_code` plus a diagnostic line naming the stage.
fn fail(
    stage: &'static str,
    fallback: &'static str,
    not_found: &'static str,
) -> impl FnOnce(ApiCallError) -> &'static str {
    move |error| {
        diag::failed(stage, &error);
        api_code(error, fallback, not_found)
    }
}

/// The user's mailbox: its mail group and every mail set (folders and
/// labels), loaded as the official client's `MailboxModel` does.
pub(crate) struct Mailbox {
    pub group: GeneratedId,
    pub sets: Vec<MailSet>,
}

pub(crate) async fn load_mailbox(sdk: &LoggedInSdk) -> Result<Mailbox> {
    let facade = sdk.mail_facade();
    let mailbox = facade.load_user_mailbox().await.map_err(fail(
        "mailbox load",
        "request_failed",
        "request_failed",
    ))?;
    let sets: Vec<MailSet> = facade
        .get_crypto_entity_client()
        .load_all(&mailbox.mailSets.mailSets, ListLoadDirection::ASC)
        .await
        .map_err(fail("mail set load", "request_failed", "request_failed"))?;
    if sets.iter().any(|set| !set._errors.is_empty()) {
        return Err("decryption_failed");
    }
    let group = mailbox._ownerGroup.ok_or("incomplete_response")?;
    Ok(Mailbox { group, sets })
}

fn same(set: &MailSet, wanted: &IdTupleGenerated) -> bool {
    set._id.as_ref() == Some(wanted)
}

fn find<'a>(sets: &'a [MailSet], wanted: &IdTupleGenerated) -> Option<&'a MailSet> {
    sets.iter().find(|set| same(set, wanted))
}

/// Whether `folder` is Trash, Spam or one of their descendants: the only
/// places the server deletes mail permanently. TS `isSpamOrTrashFolder`.
pub(crate) fn in_trash_or_spam(sets: &[MailSet], folder: &IdTupleGenerated) -> bool {
    let mut current = find(sets, folder);
    for _ in 0..MAX_FOLDER_DEPTH {
        let Some(set) = current else { return false };
        if set.folderType == TRASH_FOLDER_TYPE || set.folderType == SPAM_FOLDER_TYPE {
            return true;
        }
        current = set.parentFolder.as_ref().and_then(|p| find(sets, p));
    }
    false
}

/// The descendants of `folder`, deepest first. TS
/// `FolderSystem.getDescendantFoldersOfParent` sorted as `removeAllEmpty` does.
fn descendants_deepest_first<'a>(
    sets: &'a [MailSet],
    folder: &IdTupleGenerated,
) -> Vec<&'a MailSet> {
    let mut levels: Vec<(usize, &MailSet)> = Vec::new();
    let mut frontier = vec![folder.clone()];
    for level in 1..=MAX_FOLDER_DEPTH {
        let children: Vec<&MailSet> = sets
            .iter()
            .filter(|set| set.folderType != LABEL_FOLDER_TYPE)
            .filter(|set| {
                set.parentFolder
                    .as_ref()
                    .is_some_and(|parent| frontier.contains(parent))
            })
            .collect();
        if children.is_empty() {
            break;
        }
        frontier = children.iter().filter_map(|c| c._id.clone()).collect();
        levels.extend(children.into_iter().map(|c| (level, c)));
    }
    levels.sort_by_key(|(level, _)| std::cmp::Reverse(*level));
    levels.into_iter().map(|(_, set)| set).collect()
}

/// A folder or label name: trimmed, non-empty, no control characters.
fn valid_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_NAME_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return Err("invalid_name");
    }
    Ok(trimmed.to_string())
}

/// A label colour as the official client stores it: `#rrggbb`.
fn valid_color(color: &str) -> Result<String> {
    let hex = color.strip_prefix('#').ok_or("invalid_color")?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_color");
    }
    Ok(color.to_ascii_lowercase())
}

/// Whether a sibling of `parent` already carries `name`. The official
/// client's folder dialog refuses such a name (`checkFolderName`).
fn name_taken(
    sets: &[MailSet],
    parent: Option<&IdTupleGenerated>,
    name: &str,
    except: Option<&IdTupleGenerated>,
) -> bool {
    sets.iter().any(|set| {
        set.folderType != LABEL_FOLDER_TYPE
            && set.parentFolder.as_ref() == parent
            && set._id.as_ref() != except
            && set.name.trim().eq_ignore_ascii_case(name)
    })
}

/// A fresh session key and its encryption with the mail group's current
/// key, as TS `aes256RandomKey` + `encryptKeyWithVersionedKey`.
async fn new_owned_session_key(
    sdk: &LoggedInSdk,
    group: &GeneratedId,
    fallback: &'static str,
) -> Result<(GenericAesKey, Vec<u8>, i64)> {
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let group_key = sdk.get_current_sym_group_key(group).await.map_err(fail(
        "mail group key",
        fallback,
        fallback,
    ))?;
    let session_key = GenericAesKey::Aes256(Aes256Key::generate(&randomizer));
    let owner_enc =
        group_key.encrypt_key(&session_key, InitializationVector::generate(&randomizer));
    let version = i64::try_from(owner_enc.version).map_err(|_| fallback)?;
    Ok((session_key, owner_enc.object, version))
}

/// A random key for a service payload whose model is marked encrypted but
/// carries no encrypted value. TS passes `sessionKey: "dummy"` for the same
/// reason (`deleteFolder`); nothing is encrypted with it.
fn throwaway_key() -> GenericAesKey {
    GenericAesKey::Aes256(Aes256Key::generate(&RandomizerFacade::from_core(
        rand_core::OsRng,
    )))
}

// ---------------------------------------------------------------- listings

pub(crate) fn is_label(set: &MailSet) -> bool {
    set.folderType == LABEL_FOLDER_TYPE
}

pub(crate) fn label_row(set: &MailSet) -> Option<Value> {
    let label_id = id(set._id.as_ref()?);
    Some(json!({"id": label_id, "name": set.name, "color": set.color.clone().unwrap_or_default()}))
}

/// The labels a mail carries: its `sets` that are labels.
pub(crate) fn mail_labels(sets: &[IdTupleGenerated], label_ids: &HashSet<String>) -> Vec<String> {
    sets.iter()
        .map(id)
        .filter(|set| label_ids.contains(set))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListLabels {
    session: Session,
}

pub(crate) async fn list_labels(sdk: &Sdk, request: ListLabels) -> Result<Value> {
    let logged_in = resume(sdk, request.session).await?;
    let mailbox = load_mailbox(&logged_in).await?;
    let labels: Vec<Value> = mailbox
        .sets
        .iter()
        .filter(|set| is_label(set))
        .filter_map(label_row)
        .collect();
    Ok(json!({"labels": labels}))
}

// ----------------------------------------------------------------- folders

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateFolder {
    pub session: Session,
    name: String,
    parent: Option<String>,
}

pub(crate) async fn create_folder(sdk: &Sdk, request: CreateFolder) -> Result<Value> {
    let name = valid_name(&request.name)?;
    let parent = request.parent.as_deref().map(tuple).transpose()?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    if let Some(parent) = &parent {
        // Folders nest under folders, never under labels.
        match find(&mailbox.sets, parent) {
            Some(set) if !is_label(set) => {}
            _ => return Err("folder_not_found"),
        }
    }
    if name_taken(&mailbox.sets, parent.as_ref(), &name, None) {
        return Err("folder_exists");
    }
    let (session_key, owner_enc, version) =
        new_owned_session_key(sdk, &mailbox.group, "folder_failed").await?;
    let created = sdk
        .get_service_executor()
        .post::<MailFolderService>(
            CreateMailFolderData {
                _format: 0,
                folderName: name,
                ownerEncSessionKey: owner_enc,
                ownerGroup: Some(mailbox.group),
                ownerKeyVersion: version,
                parentFolder: parent,
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(session_key),
                ..Default::default()
            },
        )
        .await
        .map_err(fail("folder create", "folder_failed", "folder_not_found"))?;
    Ok(json!({"id": id(&created.newFolder)}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameFolder {
    pub session: Session,
    id: String,
    name: Option<String>,
    /// Absent keeps the parent, `""` moves the folder to the top level.
    parent: Option<String>,
}

/// What a rename/re-parent request asks for, validated before any I/O.
struct FolderEdit {
    folder: IdTupleGenerated,
    name: Option<String>,
    parent: Option<Option<IdTupleGenerated>>,
}

fn folder_edit(request: &RenameFolder) -> Result<FolderEdit> {
    let folder = tuple(&request.id)?;
    let name = request.name.as_deref().map(valid_name).transpose()?;
    let parent = match request.parent.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(parent) => Some(Some(tuple(parent)?)),
    };
    if name.is_none() && parent.is_none() {
        return Err("invalid_request");
    }
    if parent.as_ref().is_some_and(|p| p.as_ref() == Some(&folder)) {
        return Err("invalid_parent");
    }
    Ok(FolderEdit {
        folder,
        name,
        parent,
    })
}

/// Rename and/or re-parent a custom folder: the name through an update of
/// the `MailSet` itself (TS `updateMailFolderName`, `entityClient.update`),
/// the parent through `MailFolderService` PUT (TS `updateMailFolderParent`).
pub(crate) async fn rename_folder(sdk: &Sdk, request: RenameFolder) -> Result<Value> {
    let edit = folder_edit(&request)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    let folder = find(&mailbox.sets, &edit.folder).ok_or("folder_not_found")?;
    if folder.folderType != CUSTOM_FOLDER_TYPE {
        return Err("not_custom_folder");
    }
    let new_parent = match &edit.parent {
        Some(Some(parent)) => {
            match find(&mailbox.sets, parent) {
                Some(set) if !is_label(set) => {}
                _ => return Err("folder_not_found"),
            }
            // A folder cannot move under its own subtree.
            if descendants_deepest_first(&mailbox.sets, &edit.folder)
                .iter()
                .any(|d| same(d, parent))
            {
                return Err("invalid_parent");
            }
            Some(Some(parent.clone()))
        }
        Some(None) => Some(None),
        None => None,
    };
    let target_parent = match &new_parent {
        Some(parent) => parent.clone(),
        None => folder.parentFolder.clone(),
    };
    let target_name = edit.name.clone().unwrap_or_else(|| folder.name.clone());
    if name_taken(
        &mailbox.sets,
        target_parent.as_ref(),
        &target_name,
        Some(&edit.folder),
    ) {
        return Err("folder_exists");
    }
    if let Some(name) = edit.name.filter(|name| *name != folder.name) {
        let mut renamed = folder.clone();
        renamed.name = name;
        sdk.mail_facade()
            .get_crypto_entity_client()
            .update_instance(renamed)
            .await
            .map_err(fail("folder rename", "folder_failed", "folder_not_found"))?;
    }
    if let Some(parent) = new_parent.filter(|p| *p != folder.parentFolder) {
        sdk.get_service_executor()
            .put::<MailFolderService>(
                UpdateMailFolderData {
                    _format: 0,
                    folder: edit.folder.clone(),
                    newParent: parent,
                },
                ExtraServiceParams::default(),
            )
            .await
            .map_err(fail(
                "folder re-parent",
                "folder_failed",
                "folder_not_found",
            ))?;
    }
    Ok(json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteFolder {
    pub session: Session,
    id: String,
}

/// Whether a folder holds no mail. TS `isEmptyFolder`: one entry is enough.
async fn is_empty_folder(sdk: &LoggedInSdk, folder: &MailSet) -> Result<bool> {
    let entries: Vec<MailSetEntry> = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load_range(
            &folder.entries,
            // TS `CUSTOM_MIN_ID`. Entry ids are base64url, where the
            // generated-id minimum "------------" sorts high and would find
            // nothing in a folder that holds mail.
            &tutasdk::CustomId(String::new()),
            1,
            ListLoadDirection::ASC,
        )
        .await
        .map_err(fail("folder entries", "request_failed", "folder_not_found"))?;
    Ok(entries.is_empty())
}

/// TS `finallyDeleteCustomMailFolder`: `MailFolderService` DELETE. A folder
/// that is already gone counts as deleted; 412 means the server is still
/// working on it.
async fn finally_delete_folder(sdk: &LoggedInSdk, folder: &IdTupleGenerated) -> Result<()> {
    match sdk
        .get_service_executor()
        .delete::<MailFolderService>(
            DeleteMailFolderData {
                _format: 0,
                folders: vec![folder.clone()],
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(throwaway_key()),
                ..Default::default()
            },
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(ApiCallError::ServerResponseError {
            source: HttpError::NotFoundError,
        }) => Ok(()),
        Err(ApiCallError::ServerResponseError {
            source: HttpError::PreconditionFailedError(_),
        }) => Err("operation_still_active"),
        Err(error) => Err(fail("folder delete", "folder_failed", "folder_not_found")(
            error,
        )),
    }
}

/// Delete a custom folder the way the official client's sidebar does
/// (`MailView.deleteCustomMailFolder`): inside Trash or Spam it is deleted
/// for good; elsewhere empty descendants -- and the folder itself, when it
/// and all of them are empty -- are deleted, and a folder that still holds
/// mail is moved under Trash instead (`MailModel.trashFolderAndSubfolders`).
pub(crate) async fn delete_folder(sdk: &Sdk, request: DeleteFolder) -> Result<Value> {
    let target = tuple(&request.id)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    let folder = find(&mailbox.sets, &target).ok_or("folder_not_found")?;
    if folder.folderType != CUSTOM_FOLDER_TYPE && folder.folderType != IMPORTED_FOLDER_TYPE {
        return Err("not_custom_folder");
    }
    if in_trash_or_spam(&mailbox.sets, &target) {
        finally_delete_folder(sdk, &target).await?;
        return Ok(json!({"outcome": "deleted"}));
    }
    // TS `removeAllEmpty`.
    let mut deleted: HashSet<String> = HashSet::new();
    let mut some_non_empty = false;
    let children_deleted = |parent: &IdTupleGenerated, deleted: &HashSet<String>| {
        mailbox
            .sets
            .iter()
            .filter(|set| set.folderType == CUSTOM_FOLDER_TYPE)
            .filter(|set| set.parentFolder.as_ref() == Some(parent))
            .all(|set| set._id.as_ref().is_some_and(|i| deleted.contains(&id(i))))
    };
    for descendant in descendants_deepest_first(&mailbox.sets, &target) {
        let Some(descendant_id) = descendant._id.as_ref() else {
            continue;
        };
        if is_empty_folder(sdk, descendant).await? && children_deleted(descendant_id, &deleted) {
            finally_delete_folder(sdk, descendant_id).await?;
            deleted.insert(id(descendant_id));
        } else {
            some_non_empty = true;
        }
    }
    if !some_non_empty && is_empty_folder(sdk, folder).await? && children_deleted(&target, &deleted)
    {
        finally_delete_folder(sdk, &target).await?;
        return Ok(json!({"outcome": "deleted"}));
    }
    let trash = mailbox
        .sets
        .iter()
        .find(|set| set.folderType == TRASH_FOLDER_TYPE)
        .and_then(|set| set._id.clone())
        .ok_or("folder_not_found")?;
    sdk.get_service_executor()
        .put::<MailFolderService>(
            UpdateMailFolderData {
                _format: 0,
                folder: target,
                newParent: Some(trash),
            },
            ExtraServiceParams::default(),
        )
        .await
        .map_err(fail("folder to trash", "folder_failed", "folder_not_found"))?;
    Ok(json!({"outcome": "trashed"}))
}

// ------------------------------------------------------------------ labels

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateLabel {
    pub session: Session,
    name: String,
    color: String,
}

/// TS `MailFacade.createLabel`: `ManageLabelService` POST.
pub(crate) async fn create_label(sdk: &Sdk, request: CreateLabel) -> Result<Value> {
    let name = valid_name(&request.name)?;
    let color = valid_color(&request.color)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    if mailbox
        .sets
        .iter()
        .any(|set| is_label(set) && set.name.trim().eq_ignore_ascii_case(&name))
    {
        return Err("label_exists");
    }
    let (session_key, owner_enc, version) =
        new_owned_session_key(sdk, &mailbox.group, "label_failed").await?;
    let created = sdk
        .get_service_executor()
        .post::<ManageLabelService>(
            ManageLabelServicePostIn {
                _format: 0,
                ownerEncSessionKey: owner_enc,
                ownerKeyVersion: version,
                ownerGroup: mailbox.group,
                data: ManageLabelServiceLabelData {
                    _id: None,
                    name,
                    color,
                    parentLabel: None,
                    _errors: Default::default(),
                },
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(session_key),
                ..Default::default()
            },
        )
        .await
        .map_err(fail("label create", "label_failed", "label_failed"))?;
    Ok(json!({"id": id(&created.label)}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateLabel {
    pub session: Session,
    id: String,
    name: Option<String>,
    color: Option<String>,
}

/// The session key a stored mail set was encrypted with, as TS
/// `updateLabel` derives it: its owner group key, then `_ownerEncSessionKey`.
async fn mail_set_session_key(sdk: &LoggedInSdk, set: &MailSet) -> Result<GenericAesKey> {
    let group = set._ownerGroup.as_ref().ok_or("incomplete_response")?;
    let encrypted = set
        ._ownerEncSessionKey
        .as_ref()
        .ok_or("incomplete_response")?;
    let version = set._ownerKeyVersion.unwrap_or(0).unsigned_abs();
    let group_key = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .get_crypto_facade()
        .get_key_loader_facade()
        .load_sym_group_key(group, version, None)
        .await
        .map_err(|error| {
            diag::failed("label key", &error);
            "label_failed"
        })?;
    group_key.decrypt_aes_key(encrypted).map_err(|error| {
        diag::failed("label key", &error);
        "decryption_failed"
    })
}

/// TS `MailFacade.updateLabel`: `ManageLabelService` PUT.
pub(crate) async fn update_label(sdk: &Sdk, request: UpdateLabel) -> Result<Value> {
    let target = tuple(&request.id)?;
    let name = request.name.as_deref().map(valid_name).transpose()?;
    let color = request.color.as_deref().map(valid_color).transpose()?;
    if name.is_none() && color.is_none() {
        return Err("invalid_request");
    }
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    let label = find(&mailbox.sets, &target)
        .filter(|set| is_label(set))
        .ok_or("label_not_found")?;
    let name = name.unwrap_or_else(|| label.name.clone());
    let color = color.unwrap_or_else(|| label.color.clone().unwrap_or_default());
    if name == label.name && Some(&color) == label.color.as_ref() {
        return Ok(json!({}));
    }
    let session_key = mail_set_session_key(sdk, label).await?;
    sdk.get_service_executor()
        .put::<ManageLabelService>(
            ManageLabelServicePutIn {
                _format: 0,
                label: target,
                data: ManageLabelServiceLabelData {
                    _id: None,
                    name,
                    color,
                    parentLabel: label.parentFolder.clone(),
                    _errors: Default::default(),
                },
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(session_key),
                ..Default::default()
            },
        )
        .await
        .map_err(fail("label update", "label_failed", "label_not_found"))?;
    Ok(json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteLabel {
    pub session: Session,
    id: String,
}

/// TS `MailFacade.deleteLabel`: `ManageLabelService` DELETE. A label that is
/// already gone counts as deleted.
pub(crate) async fn delete_label(sdk: &Sdk, request: DeleteLabel) -> Result<Value> {
    let target = tuple(&request.id)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    match find(&mailbox.sets, &target) {
        None => return Ok(json!({})),
        Some(set) if !is_label(set) => return Err("label_not_found"),
        Some(_) => {}
    }
    match sdk
        .get_service_executor()
        .delete::<ManageLabelService>(
            ManageLabelServiceDeleteIn {
                _format: 0,
                label: target,
            },
            ExtraServiceParams::default(),
        )
        .await
    {
        Ok(()) => Ok(json!({})),
        Err(ApiCallError::ServerResponseError {
            source: HttpError::NotFoundError,
        }) => Ok(json!({})),
        Err(error) => Err(fail("label delete", "label_failed", "label_not_found")(
            error,
        )),
    }
}

// ------------------------------------------------------- per-mail batches

/// Unique ids grouped by mail list (the server locks per list), first-seen
/// order, at most `MAIL_BATCH_LIMIT` per group. TS `groupBy(listIdPart)` +
/// `splitInChunks`.
fn list_chunks(mails: &[IdTupleGenerated]) -> Vec<Vec<IdTupleGenerated>> {
    let mut groups: Vec<Vec<IdTupleGenerated>> = Vec::new();
    let mut by_list: HashMap<String, usize> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for mail in mails {
        if !seen.insert(id(mail)) {
            continue;
        }
        let index = *by_list.entry(mail.list_id.to_string()).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[index].push(mail.clone());
    }
    groups
        .into_iter()
        .flat_map(|group| {
            group
                .chunks(MAIL_BATCH_LIMIT)
                .map(<[IdTupleGenerated]>::to_vec)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Per-id outcome of a batch: `None` applied, `Some(code)` failed or never
/// attempted. Duplicated ids share their outcome.
struct Batch {
    outcomes: HashMap<String, Option<&'static str>>,
}

impl Batch {
    fn new(mails: &[IdTupleGenerated]) -> Self {
        Self {
            outcomes: mails
                .iter()
                .map(|m| (id(m), Some("not_attempted")))
                .collect(),
        }
    }
    fn settle(&mut self, chunk: &[IdTupleGenerated], code: Option<&'static str>) {
        for mail in chunk {
            self.outcomes.insert(id(mail), code);
        }
    }
    fn rows(&self, ids: &[String], extra: impl Fn(&str) -> Value) -> Value {
        let rows: Vec<Value> = ids
            .iter()
            .map(|mail| {
                let mut row =
                    json!({"id": mail, "code": self.outcomes.get(mail).copied().flatten()});
                if let (Value::Object(row), Value::Object(more)) = (&mut row, extra(mail)) {
                    row.extend(more);
                }
                row
            })
            .collect();
        json!({"outcomes": rows})
    }
}

fn parse_batch(ids: &[String]) -> Result<Vec<IdTupleGenerated>> {
    if ids.is_empty() || ids.len() > MAIL_BATCH_LIMIT {
        return Err("invalid_batch");
    }
    ids.iter().map(|s| tuple(s)).collect()
}

/// Move mails, recording what the server reports it moved: Tuta mails keep
/// their id across moves, and `MoveMailPostOut.movedMails` names each moved
/// id with its source folder -- what the official client's undo moves back
/// (`MailGuiUtils.runPostMoveActions`). An id the server does not list (it
/// was already in the target) gets `new_id: null`.
pub(crate) async fn move_mails(
    sdk: &LoggedInSdk,
    ids: &[String],
    destination: &str,
) -> Result<Value> {
    let parsed = parse_batch(ids)?;
    let destination = tuple(destination)?;
    let mut batch = Batch::new(&parsed);
    let mut moved: HashMap<String, String> = HashMap::new();
    for chunk in list_chunks(&parsed) {
        let reply = sdk
            .get_service_executor()
            .post::<MoveMailService>(
                MoveMailData {
                    _format: 0,
                    moveReason: None,
                    targetFolder: destination.clone(),
                    mails: chunk.clone(),
                    excludeMailSet: None,
                },
                ExtraServiceParams::default(),
            )
            .await;
        match reply {
            Ok(reply) => {
                for group in reply.movedMails {
                    for mail in group.mailIds {
                        moved.insert(
                            format!("{}/{}", mail.listId, mail.listElementId),
                            id(&group.sourceFolder),
                        );
                    }
                }
                batch.settle(&chunk, None);
            }
            Err(error) => {
                batch.settle(
                    &chunk,
                    Some(fail("move", "request_failed", "request_failed")(error)),
                );
                break;
            }
        }
    }
    Ok(batch.rows(ids, |mail| match moved.get(mail) {
        Some(source) => json!({"new_id": mail, "source": source}),
        None => json!({"new_id": null, "source": null}),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteMails {
    pub session: Session,
    ids: Vec<String>,
    /// The Trash or Spam folder (or a subfolder) the mails are in; the server
    /// deletes only what is in it.
    folder: String,
}

/// Permanently delete mails from Trash or Spam. TS
/// `MailModel.finallyDeleteMails` -> `MailFacade.deleteMails`.
pub(crate) async fn delete_mails(sdk: &Sdk, request: DeleteMails) -> Result<Value> {
    let parsed = parse_batch(&request.ids)?;
    let folder = tuple(&request.folder)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    if find(&mailbox.sets, &folder).is_none() {
        return Err("folder_not_found");
    }
    if !in_trash_or_spam(&mailbox.sets, &folder) {
        return Err("not_trash_or_spam");
    }
    let mut batch = Batch::new(&parsed);
    for chunk in list_chunks(&parsed) {
        match sdk
            .get_service_executor()
            .delete::<MailService>(
                DeleteMailData {
                    _format: 0,
                    mails: chunk.clone(),
                    folder: Some(folder.clone()),
                },
                ExtraServiceParams::default(),
            )
            .await
        {
            Ok(()) => batch.settle(&chunk, None),
            Err(error) => {
                batch.settle(
                    &chunk,
                    Some(fail("delete", "delete_failed", "delete_failed")(error)),
                );
                break;
            }
        }
    }
    Ok(batch.rows(&request.ids, |_| json!({})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyFolder {
    pub session: Session,
    id: String,
}

/// Empty Trash or Spam. TS `MailViewModel.finallyDeleteAllMailsInSelectedFolder`
/// -> `MailFacade.clearFolder`: one `MailService` DELETE naming only the
/// folder; the server deletes its mail in batches itself. A subfolder of
/// Trash or Spam is emptied by deleting it (`delete_folder`), as there.
pub(crate) async fn empty_folder(sdk: &Sdk, request: EmptyFolder) -> Result<Value> {
    let target = tuple(&request.id)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    let folder = find(&mailbox.sets, &target).ok_or("folder_not_found")?;
    if folder.folderType != TRASH_FOLDER_TYPE && folder.folderType != SPAM_FOLDER_TYPE {
        return Err("not_trash_or_spam");
    }
    let entries: Vec<MailSetEntry> = sdk
        .mail_facade()
        .get_crypto_entity_client()
        // DESC, as `sync` enumerates: the SDK's ASC walk starts at the
        // generated-id minimum, which skips base64url entry ids.
        .load_all(&folder.entries, ListLoadDirection::DESC)
        .await
        .map_err(fail("folder entries", "request_failed", "folder_not_found"))?;
    match sdk
        .get_service_executor()
        .delete::<MailService>(
            DeleteMailData {
                _format: 0,
                mails: Vec::new(),
                folder: Some(target),
            },
            ExtraServiceParams::default(),
        )
        .await
    {
        Ok(()) => Ok(json!({"deleted": entries.len()})),
        Err(ApiCallError::ServerResponseError {
            source: HttpError::PreconditionFailedError(_),
        }) => Err("operation_still_active"),
        Err(error) => Err(fail("empty folder", "delete_failed", "folder_not_found")(
            error,
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyLabels {
    pub session: Session,
    ids: Vec<String>,
    add: Vec<String>,
    remove: Vec<String>,
}

/// Validated label changes: no overlap, at most `MAX_LABELS_PER_MAIL` added.
fn label_changes(request: &ApplyLabels) -> Result<(Vec<IdTupleGenerated>, Vec<IdTupleGenerated>)> {
    let add = request
        .add
        .iter()
        .map(|s| tuple(s))
        .collect::<Result<Vec<_>>>()?;
    let remove = request
        .remove
        .iter()
        .map(|s| tuple(s))
        .collect::<Result<Vec<_>>>()?;
    if (add.is_empty() && remove.is_empty())
        || add.len() > MAX_LABELS_PER_MAIL
        || add.iter().any(|label| remove.contains(label))
    {
        return Err("invalid_request");
    }
    Ok((add, remove))
}

/// TS `MailModel.applyLabels` -> `MailFacade.applyLabels`: `ApplyLabelService`
/// POST per mail list, in chunks.
pub(crate) async fn apply_labels(sdk: &Sdk, request: ApplyLabels) -> Result<Value> {
    let parsed = parse_batch(&request.ids)?;
    let (add, remove) = label_changes(&request)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    let mailbox = load_mailbox(sdk).await?;
    if add
        .iter()
        .chain(remove.iter())
        .any(|label| !find(&mailbox.sets, label).is_some_and(is_label))
    {
        return Err("label_not_found");
    }
    let mut batch = Batch::new(&parsed);
    for chunk in list_chunks(&parsed) {
        match sdk
            .get_service_executor()
            .post::<ApplyLabelService>(
                ApplyLabelServicePostIn {
                    _format: 0,
                    mails: chunk.clone(),
                    addedLabels: add.clone(),
                    removedLabels: remove.clone(),
                },
                ExtraServiceParams::default(),
            )
            .await
        {
            Ok(()) => batch.settle(&chunk, None),
            Err(error) => {
                batch.settle(
                    &chunk,
                    Some(fail("apply labels", "label_failed", "request_failed")(
                        error,
                    )),
                );
                break;
            }
        }
    }
    Ok(batch.rows(&request.ids, |_| json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(element: &str, kind: i64, parent: Option<&str>) -> MailSet {
        MailSet {
            _id: Some(tuple(&format!("sets/{element}")).unwrap()),
            _permissions: GeneratedId("p".into()),
            _format: 0,
            _ownerEncSessionKey: None,
            name: element.to_string(),
            folderType: kind,
            _ownerGroup: None,
            _ownerKeyVersion: None,
            color: None,
            _kdfNonce: None,
            parentFolder: parent.map(|p| tuple(&format!("sets/{p}")).unwrap()),
            entries: GeneratedId("e".into()),
            _errors: Default::default(),
        }
    }

    fn t(element: &str) -> IdTupleGenerated {
        tuple(&format!("sets/{element}")).unwrap()
    }

    #[test]
    fn trash_and_spam_subtrees_allow_final_delete() {
        let sets = vec![
            set("inbox", 1, None),
            set("trash", TRASH_FOLDER_TYPE, None),
            set("spam", SPAM_FOLDER_TYPE, None),
            set("old", CUSTOM_FOLDER_TYPE, Some("trash")),
            set("older", CUSTOM_FOLDER_TYPE, Some("old")),
            set("work", CUSTOM_FOLDER_TYPE, Some("inbox")),
        ];
        assert!(in_trash_or_spam(&sets, &t("trash")));
        assert!(in_trash_or_spam(&sets, &t("spam")));
        assert!(in_trash_or_spam(&sets, &t("older")));
        assert!(!in_trash_or_spam(&sets, &t("work")));
        assert!(!in_trash_or_spam(&sets, &t("inbox")));
        assert!(!in_trash_or_spam(&sets, &t("missing")));
    }

    #[test]
    fn a_parent_cycle_does_not_hang() {
        let sets = vec![
            set("a", CUSTOM_FOLDER_TYPE, Some("b")),
            set("b", CUSTOM_FOLDER_TYPE, Some("a")),
        ];
        assert!(!in_trash_or_spam(&sets, &t("a")));
        assert!(descendants_deepest_first(&sets, &t("a")).len() <= 2 * MAX_FOLDER_DEPTH);
    }

    #[test]
    fn descendants_come_deepest_first_and_skip_labels() {
        let sets = vec![
            set("top", CUSTOM_FOLDER_TYPE, None),
            set("child", CUSTOM_FOLDER_TYPE, Some("top")),
            set("grandchild", CUSTOM_FOLDER_TYPE, Some("child")),
            set("label", LABEL_FOLDER_TYPE, Some("top")),
        ];
        let names: Vec<&str> = descendants_deepest_first(&sets, &t("top"))
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["grandchild", "child"]);
    }

    #[test]
    fn names_and_colours_are_validated() {
        assert_eq!(valid_name("  Work  ").unwrap(), "Work");
        assert_eq!(valid_name("   "), Err("invalid_name"));
        assert_eq!(valid_name("a\u{7}b"), Err("invalid_name"));
        assert_eq!(
            valid_name(&"x".repeat(MAX_NAME_CHARS + 1)),
            Err("invalid_name")
        );
        assert_eq!(valid_color("#A1b2C3").unwrap(), "#a1b2c3");
        assert_eq!(valid_color("a1b2c3"), Err("invalid_color"));
        assert_eq!(valid_color("#a1b2c"), Err("invalid_color"));
        assert_eq!(valid_color("#a1b2cg"), Err("invalid_color"));
    }

    #[test]
    fn sibling_names_collide_case_insensitively() {
        let sets = vec![
            set("Work", CUSTOM_FOLDER_TYPE, None),
            set("Sub", CUSTOM_FOLDER_TYPE, Some("Work")),
            set("Tag", LABEL_FOLDER_TYPE, None),
        ];
        assert!(name_taken(&sets, None, "work", None));
        assert!(!name_taken(&sets, None, "work", Some(&t("Work"))));
        assert!(!name_taken(&sets, None, "sub", None));
        assert!(name_taken(&sets, Some(&t("Work")), "SUB", None));
        // Labels are a separate namespace.
        assert!(!name_taken(&sets, None, "tag", None));
    }

    #[test]
    fn chunks_group_by_list_dedupe_and_cap() {
        let mut mails = Vec::new();
        for n in 0..60 {
            mails.push(tuple(&format!("a/m{n}")).unwrap());
        }
        mails.push(tuple("b/x").unwrap());
        mails.push(tuple("a/m0").unwrap());
        let chunks = list_chunks(&mails);
        let sizes: Vec<usize> = chunks.iter().map(Vec::len).collect();
        assert_eq!(sizes, [MAIL_BATCH_LIMIT, 10, 1]);
        assert!(chunks
            .iter()
            .all(|c| c.iter().all(|m| m.list_id == c[0].list_id)));
    }

    #[test]
    fn batch_rows_keep_input_order_and_mark_the_unattempted() {
        let ids: Vec<String> = ["a/1", "b/2", "a/1"].map(String::from).to_vec();
        let parsed = parse_batch(&ids).unwrap();
        let mut batch = Batch::new(&parsed);
        batch.settle(&parsed[..1], None);
        let rows = batch.rows(&ids, |m| json!({"new_id": m}));
        let rows = rows["outcomes"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["code"], Value::Null);
        assert_eq!(rows[1]["code"], "not_attempted");
        assert_eq!(rows[2]["code"], Value::Null);
        assert_eq!(rows[1]["new_id"], "b/2");
    }

    #[test]
    fn batches_are_bounded() {
        assert_eq!(parse_batch(&[]).err(), Some("invalid_batch"));
        let many: Vec<String> = (0..=MAIL_BATCH_LIMIT).map(|n| format!("a/m{n}")).collect();
        assert_eq!(parse_batch(&many).err(), Some("invalid_batch"));
        assert_eq!(parse_batch(&["nope".into()]).err(), Some("invalid_id"));
    }

    fn rename(name: Option<&str>, parent: Option<&str>) -> RenameFolder {
        serde_json::from_value(json!({
            "session": {"login": "", "user_id": "", "access_token": "", "encrypted_passphrase_key": []},
            "id": "sets/f", "name": name, "parent": parent,
        }))
        .unwrap()
    }

    #[test]
    fn folder_edits_need_a_change_and_a_sane_parent() {
        assert_eq!(
            folder_edit(&rename(None, None)).err(),
            Some("invalid_request")
        );
        assert_eq!(
            folder_edit(&rename(None, Some("sets/f"))).err(),
            Some("invalid_parent")
        );
        let top = folder_edit(&rename(None, Some(""))).unwrap();
        assert_eq!(top.parent, Some(None));
        let named = folder_edit(&rename(Some(" New "), None)).unwrap();
        assert_eq!(named.name.as_deref(), Some("New"));
        assert!(named.parent.is_none());
    }

    fn labels(add: &[&str], remove: &[&str]) -> ApplyLabels {
        serde_json::from_value(json!({
            "session": {"login": "", "user_id": "", "access_token": "", "encrypted_passphrase_key": []},
            "ids": ["a/1"], "add": add, "remove": remove,
        }))
        .unwrap()
    }

    #[test]
    fn label_changes_are_validated() {
        assert_eq!(
            label_changes(&labels(&[], &[])).err(),
            Some("invalid_request")
        );
        assert_eq!(
            label_changes(&labels(&["l/1"], &["l/1"])).err(),
            Some("invalid_request")
        );
        assert_eq!(
            label_changes(&labels(&["l/1", "l/2", "l/3", "l/4", "l/5", "l/6"], &[])).err(),
            Some("invalid_request")
        );
        assert_eq!(
            label_changes(&labels(&["bad"], &[])).err(),
            Some("invalid_id")
        );
        let (add, remove) = label_changes(&labels(&["l/1"], &["l/2"])).unwrap();
        assert_eq!((add.len(), remove.len()), (1, 1));
    }

    #[test]
    fn mail_labels_keep_only_labels() {
        let sets = vec![t("inbox"), t("tag")];
        let label_ids: HashSet<String> = [id(&t("tag"))].into_iter().collect();
        assert_eq!(mail_labels(&sets, &label_ids), [id(&t("tag"))]);
    }

    #[test]
    fn api_codes_are_fixed() {
        let server = |source| ApiCallError::ServerResponseError { source };
        assert_eq!(
            api_code(server(HttpError::NotFoundError), "x", "gone"),
            "gone"
        );
        assert_eq!(
            api_code(server(HttpError::SessionExpiredError), "x", "gone"),
            "session_revoked"
        );
        assert_eq!(
            api_code(server(HttpError::LimitReachedError), "x", "gone"),
            "limit_reached"
        );
        assert_eq!(
            api_code(server(HttpError::BadRequestError), "x", "gone"),
            "x"
        );
        assert_eq!(
            api_code(ApiCallError::internal("secret".into()), "x", "gone"),
            "x"
        );
    }

    #[test]
    fn new_ops_parse_through_the_envelope_and_refuse_unknown_fields() {
        let session =
            json!({"login": "", "user_id": "", "access_token": "", "encrypted_passphrase_key": []});
        let parses = |request: Value| serde_json::from_value::<crate::Request>(request).is_ok();
        let requests = [
            json!({"op": "list_labels"}),
            json!({"op": "create_folder", "name": "x"}),
            json!({"op": "create_folder", "name": "x", "parent": "a/b"}),
            json!({"op": "rename_folder", "id": "a/b", "name": "y"}),
            json!({"op": "rename_folder", "id": "a/b", "parent": ""}),
            json!({"op": "delete_folder", "id": "a/b"}),
            json!({"op": "create_label", "name": "x", "color": "#000000"}),
            json!({"op": "update_label", "id": "a/b", "color": "#000000"}),
            json!({"op": "delete_label", "id": "a/b"}),
            json!({"op": "apply_labels", "ids": ["a/b"], "add": ["l/1"], "remove": []}),
            json!({"op": "delete", "ids": ["a/b"], "folder": "t/t"}),
            json!({"op": "empty_folder", "id": "t/t"}),
            json!({"op": "delete_draft", "id": "a/b"}),
            json!({"op": "watch"}),
        ];
        for mut request in requests {
            let fields = request.as_object_mut().unwrap();
            fields.insert("version".into(), json!(1));
            fields.insert("session".into(), session.clone());
            assert!(parses(request.clone()), "{request}");
            request
                .as_object_mut()
                .unwrap()
                .insert("unexpected".into(), json!(true));
            assert!(!parses(request.clone()), "{request}");
        }
        // Required fields stay required.
        assert!(!parses(
            json!({"version": 1, "op": "delete", "session": session, "ids": ["a/b"]})
        ));
    }
}
