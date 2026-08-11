//! A bounded, client-side projection of ACP session output.
//!
//! The ACP session API streams updates but deliberately has no history API.
//! This module materializes the useful update families locally and provides an
//! Atlas extension for fetching a small window of older or newer records.

use crate::{v2, AcpError};
use atlas_rpc::interface;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptPageError {
    #[error("invalid transcript cursor")]
    InvalidCursor,
    #[error("stale transcript cursor")]
    StaleCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranscriptItemId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranscriptCursor(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranscriptRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageDirection {
    Older,
    Newer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<TranscriptCursor>,
    pub direction: PageDirection,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPage {
    pub direction: PageDirection,
    pub items: Vec<TranscriptItem>,
    pub revision: TranscriptRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<TranscriptCursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_cursor: Option<TranscriptCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Agent,
    Thought,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageItem {
    pub message_id: String,
    pub role: MessageRole,
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_updates: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallItem {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_updates: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryItem {
    pub session_update: String,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptItemKind {
    Message(MessageItem),
    ToolCall(ToolCallItem),
    Auxiliary(AuxiliaryItem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptItem {
    pub id: TranscriptItemId,
    #[serde(flatten)]
    pub kind: TranscriptItemKind,
}

#[derive(Debug, Clone)]
pub enum TranscriptChange {
    Inserted(TranscriptItem),
    Updated(TranscriptItem),
    Evicted(TranscriptItemId),
    PageLoaded(TranscriptPage),
    StalePage,
}

#[derive(Debug, Clone)]
pub struct TranscriptWindowConfig {
    pub page_size: usize,
    pub before: usize,
    pub after: usize,
}

impl Default for TranscriptWindowConfig {
    fn default() -> Self {
        Self {
            page_size: 100,
            before: 50,
            after: 50,
        }
    }
}

#[derive(Clone)]
pub struct Transcript {
    state: Arc<Mutex<TranscriptState>>,
    changes: broadcast::Sender<TranscriptChange>,
}

/// A descriptive name for the bounded resident transcript store.
pub type TranscriptWindow = Transcript;

struct TranscriptState {
    config: TranscriptWindowConfig,
    items: Vec<TranscriptItem>,
    positions: HashMap<TranscriptItemId, usize>,
    revision: u64,
    older_cursor: Option<TranscriptCursor>,
    newer_cursor: Option<TranscriptCursor>,
    visible: Option<(TranscriptItemId, TranscriptItemId)>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    revision: u64,
    offset: usize,
}

impl Transcript {
    pub fn new(config: TranscriptWindowConfig) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            state: Arc::new(Mutex::new(TranscriptState {
                config,
                items: Vec::new(),
                positions: HashMap::new(),
                revision: 0,
                older_cursor: None,
                newer_cursor: None,
                visible: None,
            })),
            changes,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TranscriptChange> {
        self.changes.subscribe()
    }

    pub fn items(&self) -> Vec<TranscriptItem> {
        self.state.lock().unwrap().items.clone()
    }

    pub fn set_visible(&self, first: TranscriptItemId, last: TranscriptItemId) {
        let mut state = self.state.lock().unwrap();
        state.visible = Some((first, last));
        Self::trim(&mut state, &self.changes);
    }

    pub fn next_page_request(&self, direction: PageDirection) -> Option<TranscriptPageRequest> {
        let state = self.state.lock().unwrap();
        let cursor = match direction {
            PageDirection::Older => state.older_cursor.clone(),
            PageDirection::Newer => state.newer_cursor.clone(),
        }?;
        Some(TranscriptPageRequest {
            cursor: Some(cursor),
            direction,
            limit: state.config.page_size,
        })
    }

    pub fn apply_page(&self, page: TranscriptPage) {
        let mut state = self.state.lock().unwrap();
        state.revision = page.revision.0;
        state.older_cursor = page.older_cursor.clone();
        state.newer_cursor = page.newer_cursor.clone();
        let mut new_items = Vec::new();
        for item in &page.items {
            if let Some(index) = state.positions.get(&item.id).copied() {
                state.items[index] = item.clone();
                let _ = self.changes.send(TranscriptChange::Updated(item.clone()));
            } else {
                new_items.push(item.clone());
            }
        }
        if page.direction == PageDirection::Older {
            state.items.splice(0..0, new_items.clone());
        } else {
            state.items.extend(new_items.clone());
        }
        state.positions = state
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect();
        for item in new_items {
            let _ = self.changes.send(TranscriptChange::Inserted(item));
        }
        Self::trim(&mut state, &self.changes);
        let _ = self.changes.send(TranscriptChange::PageLoaded(page));
    }

    pub fn apply_raw_update(&self, update: Value) -> Result<(), AcpError> {
        let mut state = self.state.lock().unwrap();
        let object = update
            .as_object()
            .ok_or_else(|| AcpError::new("session update must be an object"))?;
        let name = object
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::new("session update is missing sessionUpdate"))?;
        let item = match name {
            "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" => {
                let message_id = required_string(object, "messageId")?;
                let role = match name {
                    "user_message_chunk" => MessageRole::User,
                    "agent_message_chunk" => MessageRole::Agent,
                    _ => MessageRole::Thought,
                };
                let id = TranscriptItemId(format!("message:{message_id}"));
                let content = object
                    .get("content")
                    .cloned()
                    .ok_or_else(|| AcpError::new("message chunk is missing content"))?;
                match state.positions.get(&id).copied() {
                    Some(index) => match &mut state.items[index].kind {
                        TranscriptItemKind::Message(message) if message.role == role => {
                            message.content.push(content);
                            message.raw_updates.push(update.clone());
                            state.items[index].clone()
                        }
                        _ => return Err(AcpError::new("message ID changed transcript item kind")),
                    },
                    None => TranscriptItem {
                        id,
                        kind: TranscriptItemKind::Message(MessageItem {
                            message_id,
                            role,
                            content: vec![content],
                            raw_updates: vec![update.clone()],
                        }),
                    },
                }
            }
            "user_message" | "agent_message" | "agent_thought" => {
                let message_id = required_string(object, "messageId")?;
                let role = match name {
                    "user_message" => MessageRole::User,
                    "agent_message" => MessageRole::Agent,
                    _ => MessageRole::Thought,
                };
                let id = TranscriptItemId(format!("message:{message_id}"));
                let replacement = object.get("content");
                match state.positions.get(&id).copied() {
                    Some(index) => match &mut state.items[index].kind {
                        TranscriptItemKind::Message(message) if message.role == role => {
                            if let Some(content) = replacement {
                                message.content = content.as_array().cloned().unwrap_or_default();
                            }
                            message.raw_updates.push(update.clone());
                            state.items[index].clone()
                        }
                        _ => return Err(AcpError::new("message ID changed transcript item kind")),
                    },
                    None => TranscriptItem {
                        id,
                        kind: TranscriptItemKind::Message(MessageItem {
                            message_id,
                            role,
                            content: replacement
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default(),
                            raw_updates: vec![update.clone()],
                        }),
                    },
                }
            }
            "tool_call_content_chunk" | "tool_call_update" => {
                let tool_call_id = required_string(object, "toolCallId")?;
                let id = TranscriptItemId(format!("tool:{tool_call_id}"));
                match state.positions.get(&id).copied() {
                    Some(index) => match &mut state.items[index].kind {
                        TranscriptItemKind::ToolCall(tool) => {
                            if name == "tool_call_content_chunk" {
                                let content = object.get("content").cloned().ok_or_else(|| {
                                    AcpError::new("tool content chunk is missing content")
                                })?;
                                tool.content.get_or_insert_with(Vec::new).push(content);
                            } else {
                                if let Some(title) = object.get("title") {
                                    tool.title = title.as_str().map(str::to_owned);
                                }
                                if let Some(status) = object.get("status") {
                                    tool.status = status.as_str().map(str::to_owned);
                                }
                                if let Some(content) = object.get("content") {
                                    tool.content = content.as_array().cloned();
                                }
                            }
                            tool.raw_updates.push(update.clone());
                            state.items[index].clone()
                        }
                        _ => {
                            return Err(AcpError::new("tool call ID changed transcript item kind"))
                        }
                    },
                    None => TranscriptItem {
                        id,
                        kind: TranscriptItemKind::ToolCall(ToolCallItem {
                            tool_call_id,
                            title: object
                                .get("title")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            status: object
                                .get("status")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            content: if name == "tool_call_content_chunk" {
                                object.get("content").cloned().map(|value| vec![value])
                            } else {
                                object.get("content").and_then(Value::as_array).cloned()
                            },
                            raw_updates: vec![update.clone()],
                        }),
                    },
                }
            }
            _ => TranscriptItem {
                id: TranscriptItemId(format!("update:{}", state.revision.wrapping_add(1))),
                kind: TranscriptItemKind::Auxiliary(AuxiliaryItem {
                    session_update: name.into(),
                    raw: update.clone(),
                }),
            },
        };
        Self::upsert(&mut state, item, &self.changes);
        state.revision = state.revision.wrapping_add(1);
        Self::trim(&mut state, &self.changes);
        Ok(())
    }

    pub fn page(
        &self,
        request: TranscriptPageRequest,
    ) -> Result<TranscriptPage, TranscriptPageError> {
        let state = self.state.lock().unwrap();
        let boundary = match request.cursor {
            Some(cursor) => {
                let cursor: Cursor = serde_json::from_str(&cursor.0)
                    .map_err(|_| TranscriptPageError::InvalidCursor)?;
                if cursor.revision != state.revision {
                    let _ = self.changes.send(TranscriptChange::StalePage);
                    return Err(TranscriptPageError::StaleCursor);
                }
                cursor.offset
            }
            None => match request.direction {
                PageDirection::Older => state.items.len(),
                PageDirection::Newer => 0,
            },
        };
        let limit = request.limit.clamp(1, 1000);
        let (start, end) = match request.direction {
            PageDirection::Older => (
                boundary.saturating_sub(limit),
                boundary.min(state.items.len()),
            ),
            PageDirection::Newer => (
                boundary.min(state.items.len()),
                boundary.saturating_add(limit).min(state.items.len()),
            ),
        };
        let cursor = |offset| {
            TranscriptCursor(
                serde_json::to_string(&Cursor {
                    revision: state.revision,
                    offset,
                })
                .expect("cursor serializes"),
            )
        };
        Ok(TranscriptPage {
            direction: request.direction,
            items: state.items[start..end].to_vec(),
            revision: TranscriptRevision(state.revision),
            older_cursor: (start > 0).then(|| cursor(start)),
            newer_cursor: (end < state.items.len()).then(|| cursor(end)),
        })
    }

    fn upsert(
        state: &mut TranscriptState,
        item: TranscriptItem,
        changes: &broadcast::Sender<TranscriptChange>,
    ) {
        if let Some(index) = state.positions.get(&item.id).copied() {
            state.items[index] = item.clone();
            let _ = changes.send(TranscriptChange::Updated(item));
        } else {
            let index = state.items.len();
            state.positions.insert(item.id.clone(), index);
            state.items.push(item.clone());
            let _ = changes.send(TranscriptChange::Inserted(item));
        }
    }

    fn trim(state: &mut TranscriptState, changes: &broadcast::Sender<TranscriptChange>) {
        let capacity = state
            .config
            .page_size
            .saturating_add(state.config.before)
            .saturating_add(state.config.after)
            .max(1);
        if state.items.len() <= capacity {
            return;
        }
        let (mut start, mut end) = match &state.visible {
            Some((first, last)) => match (state.positions.get(first), state.positions.get(last)) {
                (Some(first), Some(last)) => (
                    first.saturating_sub(state.config.before),
                    last.saturating_add(state.config.after).saturating_add(1),
                ),
                _ => (
                    state.items.len().saturating_sub(capacity),
                    state.items.len(),
                ),
            },
            None => (
                state.items.len().saturating_sub(capacity),
                state.items.len(),
            ),
        };
        if end.saturating_sub(start) > capacity {
            end = start.saturating_add(capacity);
        }
        if end > state.items.len() {
            end = state.items.len();
            start = end.saturating_sub(capacity);
        }
        let evicted: Vec<_> = state.items[..start]
            .iter()
            .chain(state.items[end..].iter())
            .map(|item| item.id.clone())
            .collect();
        state.items = state.items[start..end].to_vec();
        state.positions = state
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect();
        for id in evicted {
            let _ = changes.send(TranscriptChange::Evicted(id));
        }
    }
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<String, AcpError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AcpError::new(format!("session update is missing {name}")))
}

#[derive(Clone)]
pub struct TranscriptClient<C> {
    inner: C,
    attachment: Arc<Mutex<Option<(v2::SessionId, Transcript)>>>,
}

impl<C> TranscriptClient<C> {
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            attachment: Arc::new(Mutex::new(None)),
        }
    }
    pub fn attach(&self, session_id: v2::SessionId, transcript: Transcript) {
        *self.attachment.lock().unwrap() = Some((session_id, transcript));
    }
    pub fn detach(&self) -> Option<Transcript> {
        self.attachment
            .lock()
            .unwrap()
            .take()
            .map(|(_, transcript)| transcript)
    }
}

impl<C: v2::Client + Send + Sync> v2::Client for TranscriptClient<C> {
    async fn session_update(
        &self,
        session_id: v2::SessionId,
        update: Value,
    ) -> Result<(), AcpError> {
        self.inner
            .session_update(session_id.clone(), update.clone())
            .await?;
        if let Some((attached, transcript)) = self.attachment.lock().unwrap().clone() {
            if attached == session_id {
                transcript.apply_raw_update(update)?;
            }
        }
        Ok(())
    }
    async fn request_permission(
        &self,
        session_id: v2::SessionId,
        title: String,
        subject: Option<String>,
        options: Vec<Value>,
    ) -> Result<v2::PermissionResponse, AcpError> {
        self.inner
            .request_permission(session_id, title, subject, options)
            .await
    }
}

#[interface]
pub trait TranscriptAgent {
    #[rpc(method = "atlas/session/transcript/list")]
    async fn list_transcript(
        &self,
        #[serde(rename = "sessionId")] session_id: v2::SessionId,
        request: TranscriptPageRequest,
    ) -> Result<TranscriptPage, AcpError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::Client as _;
    use serde_json::json;

    #[test]
    fn chunks_and_replacements_materialize_a_message() {
        let transcript = Transcript::new(TranscriptWindowConfig::default());
        transcript.apply_raw_update(json!({"sessionUpdate":"agent_message_chunk","messageId":"m","content":{"type":"text","text":"one"}})).unwrap();
        transcript.apply_raw_update(json!({"sessionUpdate":"agent_message","messageId":"m","content":[{"type":"text","text":"two"}]})).unwrap();
        let TranscriptItemKind::Message(message) = &transcript.items()[0].kind else {
            panic!()
        };
        assert_eq!(message.content, vec![json!({"type":"text","text":"two"})]);
    }

    #[test]
    fn unknown_updates_are_preserved() {
        let transcript = Transcript::new(TranscriptWindowConfig::default());
        transcript
            .apply_raw_update(json!({"sessionUpdate":"future_update","answer":42}))
            .unwrap();
        let TranscriptItemKind::Auxiliary(item) = &transcript.items()[0].kind else {
            panic!()
        };
        assert_eq!(item.raw["answer"], 42);
    }

    #[test]
    fn page_cursor_is_revision_bound() {
        let transcript = Transcript::new(TranscriptWindowConfig::default());
        transcript
            .apply_raw_update(json!({"sessionUpdate":"state_update"}))
            .unwrap();
        transcript
            .apply_raw_update(json!({"sessionUpdate":"state_update"}))
            .unwrap();
        let first = transcript
            .page(TranscriptPageRequest {
                cursor: None,
                direction: PageDirection::Older,
                limit: 1,
            })
            .unwrap();
        transcript
            .apply_raw_update(json!({"sessionUpdate":"state_update"}))
            .unwrap();
        let error = transcript
            .page(TranscriptPageRequest {
                cursor: first.older_cursor,
                direction: PageDirection::Older,
                limit: 1,
            })
            .unwrap_err();
        assert_eq!(error, TranscriptPageError::StaleCursor);
    }

    #[tokio::test]
    async fn decorator_forwards_and_broadcasts_matching_updates() {
        #[derive(Clone)]
        struct Client;
        impl v2::Client for Client {
            async fn session_update(&self, _: String, _: Value) -> Result<(), AcpError> {
                Ok(())
            }
            async fn request_permission(
                &self,
                _: String,
                _: String,
                _: Option<String>,
                _: Vec<Value>,
            ) -> Result<v2::PermissionResponse, AcpError> {
                Ok(v2::PermissionResponse {
                    outcome: Value::Null,
                })
            }
        }
        let transcript = Transcript::new(TranscriptWindowConfig::default());
        let mut changes = transcript.subscribe();
        let client = TranscriptClient::new(Client);
        client.attach("s".into(), transcript.clone());
        client.session_update("s".into(), json!({"sessionUpdate":"agent_message_chunk","messageId":"m","content":{"type":"text","text":"hi"}})).await.unwrap();
        assert!(matches!(
            changes.recv().await.unwrap(),
            TranscriptChange::Inserted(_)
        ));
        assert_eq!(transcript.items().len(), 1);
    }
}
