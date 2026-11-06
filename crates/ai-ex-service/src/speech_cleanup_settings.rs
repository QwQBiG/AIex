use ai_ex_audio::SpeechCleanup;
use ai_ex_config::{SpeechCleanupConfig, native::NativeSpeechCleanup};

/// Capture one consistent policy when the service creates its speech queue.
pub(super) fn startup_cleanup(
    service: &SpeechCleanupConfig,
    native: Option<&NativeSpeechCleanup>,
) -> SpeechCleanup {
    if let Some(native) = native {
        SpeechCleanup {
            remove_emoji: native.remove_emoji,
            remove_markdown: native.remove_markdown,
            remove_parenthetical: native.remove_parenthetical,
        }
    } else {
        SpeechCleanup {
            remove_emoji: service.remove_emoji,
            remove_markdown: service.remove_markdown,
            remove_parenthetical: service.remove_parenthetical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_cleanup_uses_native_override_or_service_config() {
        let service = SpeechCleanupConfig {
            remove_emoji: false,
            remove_markdown: false,
            remove_parenthetical: false,
        };
        let text = "你好😀 **朋友**（轻声）";
        assert_eq!(startup_cleanup(&service, None).clean(text), text);
        assert_eq!(
            startup_cleanup(&service, Some(&NativeSpeechCleanup::default())).clean(text),
            "你好 朋友"
        );
    }
}
