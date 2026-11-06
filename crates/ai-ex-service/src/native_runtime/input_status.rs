use std::sync::atomic::{AtomicU8, Ordering};

use ai_ex_domain::{AppError, NativeInputStatus};
use uuid::Uuid;

pub(crate) fn validate_instance(expected: Uuid, current: Option<Uuid>) -> Result<(), AppError> {
    if current != Some(expected) {
        return Err(AppError::invalid_transition("service instance changed"));
    }
    Ok(())
}

/// Observations do not control capture and never contain device details.
#[derive(Default)]
pub(super) struct InputStatus(AtomicU8);

impl InputStatus {
    pub(super) fn set(&self, status: NativeInputStatus) {
        self.0.store(status as u8, Ordering::Release);
    }

    pub(super) fn get(&self) -> NativeInputStatus {
        match self.0.load(Ordering::Acquire) {
            1 => NativeInputStatus::Disabled,
            2 => NativeInputStatus::Preparing,
            3 => NativeInputStatus::Ready,
            4 => NativeInputStatus::Unavailable,
            _ => NativeInputStatus::Unknown,
        }
    }

    #[cfg(any(feature = "native-capture", test))]
    pub(super) async fn until_stopped<F>(&self, capture: F) -> Result<(), AppError>
    where
        F: std::future::Future<Output = Result<(), AppError>>,
    {
        let _finished = CaptureFinished(self);
        capture.await
    }
}

#[cfg(any(feature = "native-capture", test))]
struct CaptureFinished<'a>(&'a InputStatus);

#[cfg(any(feature = "native-capture", test))]
impl Drop for CaptureFinished<'_> {
    fn drop(&mut self) {
        self.0.set(NativeInputStatus::Unavailable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_queries_reject_a_former_or_missing_service_instance() {
        let current = Uuid::new_v4();
        assert!(validate_instance(current, Some(current)).is_ok());
        assert!(validate_instance(Uuid::new_v4(), Some(current)).is_err());
        assert!(validate_instance(current, None).is_err());
    }

    #[tokio::test]
    async fn synthetic_input_lifecycle_reports_terminal_states_without_error_details() {
        let status = InputStatus::default();
        assert_eq!(status.get(), NativeInputStatus::Unknown);
        status.set(NativeInputStatus::Disabled);
        assert_eq!(status.get(), NativeInputStatus::Disabled);
        status.set(NativeInputStatus::Preparing);
        let failed = status
            .until_stopped(async { Err(AppError::unavailable("private-device-marker")) })
            .await;
        assert!(failed.is_err());
        assert_eq!(status.get(), NativeInputStatus::Unavailable);
        status.set(NativeInputStatus::Preparing);
        status
            .until_stopped(async {
                status.set(NativeInputStatus::Ready);
                assert_eq!(status.get(), NativeInputStatus::Ready);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(status.get(), NativeInputStatus::Unavailable);
        assert!(!format!("{:?}", status.get()).contains("private-device-marker"));
    }

    #[tokio::test]
    async fn cancelled_synthetic_capture_does_not_remain_ready() {
        let status = std::sync::Arc::new(InputStatus::default());
        status.set(NativeInputStatus::Ready);
        let (started, ready) = tokio::sync::oneshot::channel();
        let shared = std::sync::Arc::clone(&status);
        let task = tokio::spawn(async move {
            shared
                .until_stopped(async move {
                    let _ = started.send(());
                    std::future::pending::<Result<(), AppError>>().await
                })
                .await
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(status.get(), NativeInputStatus::Unavailable);
    }
}
