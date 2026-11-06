#[cfg(any(feature = "native-capture", test))]
use std::sync::Arc;
#[cfg(any(feature = "native-capture", test))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(feature = "native-capture", test))]
use std::time::Duration;

#[cfg(any(feature = "native-capture", test))]
use ai_ex_domain::NativeInputDevice;
use ai_ex_domain::{AppError, NativeReply};

use super::NativeRuntime;

impl NativeRuntime {
    pub(super) async fn input_devices(&self) -> Result<NativeReply, AppError> {
        #[cfg(feature = "native-capture")]
        {
            let devices = discover_with(
                self.input_devices_busy.clone(),
                Duration::from_secs(3),
                || {
                    ai_ex_capture::list_input_devices().map(|choices| {
                        choices
                            .into_iter()
                            .map(|choice| NativeInputDevice {
                                name: choice.name,
                                ambiguous: choice.ambiguous,
                            })
                            .collect()
                    })
                },
            )
            .await?;
            Ok(NativeReply::InputDevices(devices))
        }
        #[cfg(not(feature = "native-capture"))]
        {
            Err(AppError::unavailable("此程序未启用原生麦克风支持"))
        }
    }
}

#[cfg(any(feature = "native-capture", test))]
struct Busy(Arc<AtomicBool>);

#[cfg(any(feature = "native-capture", test))]
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(any(feature = "native-capture", test))]
async fn discover_with<F>(
    slot: Arc<AtomicBool>,
    timeout: Duration,
    query: F,
) -> Result<Vec<NativeInputDevice>, AppError>
where
    F: FnOnce() -> Result<Vec<NativeInputDevice>, AppError> + Send + 'static,
{
    if slot
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(AppError::unavailable("设备列表仍在读取，请稍后重试"));
    }
    let busy = Busy(slot);
    let (reply, response) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("input-device-list".into())
        .spawn(move || {
            let _busy = busy;
            let _ = reply.send(query());
        })
        .map_err(|_| AppError::unavailable("无法开始读取设备列表"))?;
    tokio::time::timeout(timeout, response)
        .await
        .map_err(|_| AppError::unavailable("读取设备列表超时，请稍后重试"))?
        .map_err(|_| AppError::unavailable("无法读取设备列表"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn synthetic_discovery_timeout_keeps_one_worker_until_it_returns() {
        let slot = Arc::new(AtomicBool::new(false));
        let (release, blocked) = std::sync::mpsc::channel();
        let error = discover_with(slot.clone(), Duration::from_millis(25), move || {
            blocked.recv().unwrap();
            Ok(Vec::new())
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("超时"));
        assert!(slot.load(Ordering::Acquire));
        assert!(
            discover_with(slot.clone(), Duration::from_secs(1), || Ok(Vec::new()))
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while slot.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let expected = vec![NativeInputDevice {
            name: "合成麦克风".into(),
            ambiguous: false,
        }];
        let copy = expected.clone();
        assert_eq!(
            discover_with(slot, Duration::from_secs(1), move || Ok(copy))
                .await
                .unwrap(),
            expected
        );
    }
}
