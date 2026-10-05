use super::*;
use serde_json::{Map, Value};

#[derive(Default)]
pub(super) struct MemoryMediaDraft {
    pub(super) modality: MemoryModality,
    pub(super) tags: String,
    pub(super) importance: u16,
    pub(super) audio_description: String,
    pub(super) visual_description: String,
    original: Option<MemoryMedia>,
}

impl MemoryMediaDraft {
    pub(super) fn from_media(media: Option<&MemoryMedia>) -> Self {
        let Some(media) = media else {
            return Self::default();
        };
        Self {
            modality: media.modality,
            tags: media.tags.join(", "),
            importance: media.importance,
            audio_description: description(media.audio_features.as_ref()),
            visual_description: description(media.visual_features.as_ref()),
            original: Some(media.clone()),
        }
    }

    pub(super) fn has_content(&self) -> bool {
        self.original.is_some()
            || self.modality != MemoryModality::Text
            || !self.tags.is_empty()
            || self.importance != 0
            || !self.audio_description.is_empty()
            || !self.visual_description.is_empty()
    }

    pub(super) fn clears_features(&self) -> bool {
        self.original.as_ref().is_some_and(|media| {
            (media.audio_features.is_some() && !has_audio(self.modality))
                || (media.visual_features.is_some() && !has_visual(self.modality))
        })
    }

    pub(super) fn to_media(&self) -> Result<Option<MemoryMedia>, String> {
        if !self.has_content() {
            return Ok(None);
        }
        let mut media = self.original.clone().unwrap_or_default();
        media.modality = self.modality;
        if self
            .original
            .as_ref()
            .is_none_or(|original| self.tags != original.tags.join(", "))
        {
            media.tags = tags(&self.tags);
        }
        media.importance = self.importance;
        // Legacy multimodal records may omit either feature map. Merely editing
        // their text must not turn an absent map into an invented empty one.
        let preserve_missing = self.modality == MemoryModality::Multimodal
            && self
                .original
                .as_ref()
                .is_some_and(|original| original.modality == self.modality);
        patch_description(
            &mut media.audio_features,
            &self.audio_description,
            has_audio(self.modality),
            preserve_missing,
        );
        patch_description(
            &mut media.visual_features,
            &self.visual_description,
            has_visual(self.modality),
            preserve_missing,
        );
        media
            .validate()
            .map_err(|error| format!("请检查媒体描述和标签：{error}"))?;
        Ok(Some(media))
    }
}

pub(super) fn tags(text: &str) -> Vec<String> {
    text.split([',', '，', '\n'])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn has_audio(modality: MemoryModality) -> bool {
    matches!(modality, MemoryModality::Audio | MemoryModality::Multimodal)
}

pub(super) fn has_visual(modality: MemoryModality) -> bool {
    matches!(
        modality,
        MemoryModality::Visual | MemoryModality::Multimodal
    )
}

fn description(features: Option<&Map<String, Value>>) -> String {
    features
        .and_then(|features| features.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn patch_description(
    features: &mut Option<Map<String, Value>>,
    text: &str,
    enabled: bool,
    preserve_missing: bool,
) {
    if !enabled {
        *features = None;
        return;
    }
    if preserve_missing && features.is_none() && text.is_empty() {
        return;
    }
    let original = description(features.as_ref());
    let map = features.get_or_insert_with(Map::new);
    if original != text {
        if text.is_empty() {
            map.remove("description");
        } else {
            map.insert("description".to_owned(), Value::String(text.to_owned()));
        }
    }
}
