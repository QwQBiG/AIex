use std::collections::BTreeMap;

use ai_ex_domain::{
    AppError, MemoryEntityPage, MemoryEntry, MemoryKind, MemoryKnowledge, MemoryMedia,
    MemoryModality, MemoryPage, MemoryRequest, MemoryResponse,
};

#[path = "memory_panel_editor.rs"]
mod editor;
#[cfg(test)]
#[path = "memory_panel_rich_tests.rs"]
mod rich_tests;
#[cfg(test)]
#[path = "memory_panel_tests.rs"]
mod tests;
#[path = "memory_panel_ui.rs"]
mod ui;
use editor::MemoryMediaDraft;

const PAGE_SIZE: usize = 12;

#[derive(Clone, Default, PartialEq, Eq)]
enum MemoryView {
    #[default]
    Records,
    Entities,
    Media,
    Evidence {
        key: String,
        name: String,
    },
}

struct Pending {
    id: u64,
    mutation: bool,
    clear_draft: bool,
    request: MemoryRequest,
}

#[derive(Default)]
struct SavedDraft {
    text: String,
    editing: Option<MemoryEntry>,
    media: MemoryMediaDraft,
    requires_review: bool,
}

#[derive(Default)]
pub struct MemoryPanel {
    profile_id: String,
    page: Option<MemoryPage>,
    entity_page: Option<MemoryEntityPage>,
    view: MemoryView,
    storage_enabled: Option<bool>,
    query: String,
    kind: Option<MemoryKind>,
    filter_modality: Option<MemoryModality>,
    filter_tags: String,
    filter_importance: u16,
    draft: String,
    media_draft: MemoryMediaDraft,
    editing: Option<MemoryEntry>,
    forget: Option<MemoryEntry>,
    drafts: BTreeMap<String, SavedDraft>,
    pending: Option<Pending>,
    next_id: u64,
    loaded: bool,
    feedback: Option<String>,
    requires_review: bool,
    focus_editor: bool,
    offsets: Vec<usize>,
}

impl MemoryPanel {
    pub fn set_profile(&mut self, profile_id: &str) {
        if self.profile_id == profile_id {
            return;
        }
        if self.editor_has_draft() || self.requires_review {
            self.drafts.insert(
                self.profile_id.clone(),
                SavedDraft {
                    text: std::mem::take(&mut self.draft),
                    editing: self.editing.take(),
                    media: std::mem::take(&mut self.media_draft),
                    requires_review: self.requires_review,
                },
            );
        }
        let saved = self.drafts.remove(profile_id).unwrap_or_default();
        self.profile_id = profile_id.to_owned();
        self.draft = saved.text;
        self.editing = saved.editing;
        self.media_draft = saved.media;
        self.requires_review = saved.requires_review;
        self.forget = None;
        self.page = None;
        self.entity_page = None;
        self.view = MemoryView::Records;
        self.storage_enabled = None;
        self.query.clear();
        self.kind = None;
        self.filter_modality = None;
        self.filter_tags.clear();
        self.filter_importance = 0;
        self.loaded = false;
        self.pending = None;
        self.feedback = None;
        self.focus_editor = false;
        self.offsets.clear();
    }

    pub fn mutation_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|item| item.mutation)
    }

    fn editor_has_draft(&self) -> bool {
        !self.draft.is_empty() || self.editing.is_some() || self.media_draft.has_content()
    }

    pub fn has_unsaved(&self) -> bool {
        self.editor_has_draft()
            || !self.drafts.is_empty()
            || self.mutation_pending()
            || self.requires_review
    }

    pub fn begin(&mut self, request: MemoryRequest) -> Option<crate::worker::WorkerCommand> {
        let mutation = request.is_mutation();
        if self.pending.is_some() || (mutation && self.requires_review) {
            return None;
        }
        if request.profile_id() != self.profile_id || request.validate().is_err() {
            self.feedback = Some("请检查记忆内容与当前角色后重试。".to_owned());
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let clear_draft = matches!(
            request,
            MemoryRequest::Remember { .. }
                | MemoryRequest::Correct { .. }
                | MemoryRequest::RememberMedia { .. }
                | MemoryRequest::CorrectMedia { .. }
        );
        self.pending = Some(Pending {
            id: self.next_id,
            mutation,
            clear_draft,
            request: request.clone(),
        });
        self.loaded = true;
        self.feedback = None;
        Some(crate::worker::WorkerCommand::Memory {
            request_id: self.next_id,
            request,
        })
    }

    pub fn receive(&mut self, request_id: u64, result: Result<MemoryResponse, AppError>) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|item| item.id != request_id)
        {
            return false;
        }
        let pending = self.pending.take().expect("matching request");
        match result {
            Ok(MemoryResponse::Page(page)) if self.accept_page(&pending.request, &page) => {
                self.record_offset(page.offset);
                self.storage_enabled = Some(page.enabled);
                self.page = Some(page);
                self.entity_page = None;
                self.feedback = None;
                self.requires_review = false;
            }
            Ok(MemoryResponse::Entities(page)) if self.accept_entities(&pending.request, &page) => {
                self.record_offset(page.offset);
                self.storage_enabled = Some(page.enabled);
                self.entity_page = Some(page);
                self.page = None;
                self.feedback = None;
                self.requires_review = false;
            }
            Ok(MemoryResponse::Changed) if pending.mutation => {
                if pending.clear_draft {
                    self.clear_editor();
                }
                self.forget = None;
                self.page = None;
                self.entity_page = None;
                self.offsets.clear();
                self.loaded = false;
                self.feedback = Some("记忆已更新，下一轮对话会使用新的内容。".to_owned());
                return true;
            }
            Ok(_) => {
                self.requires_review |= pending.mutation;
                self.feedback = Some("记忆响应与当前操作不一致，请刷新后核对。".to_owned());
            }
            Err(error) => {
                let uncertain = pending.mutation
                    && matches!(
                        error.kind,
                        ai_ex_domain::ErrorKind::Connectivity
                            | ai_ex_domain::ErrorKind::Protocol
                            | ai_ex_domain::ErrorKind::Internal
                    );
                self.requires_review |= uncertain;
                if uncertain {
                    self.view = MemoryView::Records;
                    self.query.clear();
                    self.kind = None;
                    self.page = None;
                    self.entity_page = None;
                    self.offsets.clear();
                }
                let hint = if uncertain {
                    "操作结果尚未确认，草稿已保留。请先刷新核对，避免重复记住。"
                } else {
                    "操作未完成，草稿已保留。结束当前回复后可刷新或重试。"
                };
                self.feedback = Some(format!("{hint}\n{error}"));
            }
        }
        false
    }

    fn accept_page(&self, request: &MemoryRequest, page: &MemoryPage) -> bool {
        let (offset, limit) = match request {
            MemoryRequest::List { offset, limit, .. }
            | MemoryRequest::Evidence { offset, limit, .. }
            | MemoryRequest::QueryMedia { offset, limit, .. } => (*offset, *limit),
            _ => return false,
        };
        page.profile_id == self.profile_id
            && page.offset == offset
            && page.entries.len() <= limit
            && self.list_request(offset) == *request
            && page
                .entries
                .iter()
                .all(|entry| entry.profile_id == self.profile_id)
            && page
                .knowledge
                .keys()
                .all(|id| page.entries.iter().any(|entry| entry.id == *id))
    }

    fn accept_entities(&self, request: &MemoryRequest, page: &MemoryEntityPage) -> bool {
        let MemoryRequest::Entities { offset, limit, .. } = request else {
            return false;
        };
        page.profile_id == self.profile_id
            && page.offset == *offset
            && page.entities.len() <= *limit
            && self.list_request(*offset) == *request
    }

    fn record_offset(&mut self, offset: usize) {
        if offset == 0 {
            self.offsets.clear();
        } else if let Some(index) = self.offsets.iter().position(|item| *item == offset) {
            self.offsets.truncate(index);
        }
        self.offsets.push(offset);
    }

    fn set_view(&mut self, view: MemoryView) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.query.clear();
        self.invalidate_results();
        self.loaded = false;
    }

    fn invalidate_results(&mut self) {
        self.page = None;
        self.entity_page = None;
        self.offsets.clear();
        self.loaded = true;
    }

    fn list_request(&self, offset: usize) -> MemoryRequest {
        let profile_id = self.profile_id.clone();
        let query = self.query.trim().to_owned();
        match &self.view {
            MemoryView::Records => MemoryRequest::List {
                profile_id,
                query,
                kind: self.kind,
                offset,
                limit: PAGE_SIZE,
            },
            MemoryView::Entities => MemoryRequest::Entities {
                profile_id,
                query,
                offset,
                limit: PAGE_SIZE,
            },
            MemoryView::Evidence { key, .. } => MemoryRequest::Evidence {
                profile_id,
                key: key.clone(),
                offset,
                limit: PAGE_SIZE,
            },
            MemoryView::Media => MemoryRequest::QueryMedia {
                profile_id,
                query,
                modality: self.filter_modality,
                tags: editor::tags(&self.filter_tags),
                min_importance: self.filter_importance,
                offset,
                limit: PAGE_SIZE,
            },
        }
    }

    fn start_edit(
        &mut self,
        entry: &MemoryEntry,
        knowledge: Option<&MemoryKnowledge>,
        truncated: bool,
    ) {
        self.draft = if truncated {
            String::new()
        } else {
            entry.user_text.clone()
        };
        self.editing = Some(entry.clone());
        self.media_draft =
            MemoryMediaDraft::from_media(knowledge.and_then(|item| item.media.as_ref()));
        self.focus_editor = true;
        self.forget = None;
    }

    fn editor_request(&self) -> Result<MemoryRequest, String> {
        let text = self.draft.trim().to_owned();
        let profile_id = self.profile_id.clone();
        let media = self.media_draft.to_media()?;
        let request = match (&self.editing, media) {
            (Some(entry), Some(media)) => MemoryRequest::CorrectMedia {
                profile_id,
                id: entry.id,
                expected_revision: entry.revision,
                text,
                media,
            },
            (None, Some(media)) => MemoryRequest::RememberMedia {
                profile_id,
                text,
                media,
            },
            (Some(entry), None) => MemoryRequest::Correct {
                profile_id,
                id: entry.id,
                expected_revision: entry.revision,
                text,
            },
            (None, None) => MemoryRequest::Remember { profile_id, text },
        };
        request
            .validate()
            .map_err(|error| format!("请检查正文、标签和描述长度：{error}"))?;
        Ok(request)
    }

    fn clear_editor(&mut self) {
        self.draft.clear();
        self.editing = None;
        self.media_draft = MemoryMediaDraft::default();
        self.focus_editor = false;
    }

    fn previous_offset(&self) -> usize {
        self.offsets.iter().rev().nth(1).copied().unwrap_or(0)
    }
}
