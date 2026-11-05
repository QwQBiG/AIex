use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use ai_ex_domain::{AppError, NativeReply};

use super::NativeRuntime;

impl NativeRuntime {
    pub(super) async fn storage_space(
        &self,
        expected_revision: u64,
    ) -> Result<NativeReply, AppError> {
        self.storage_space_with(expected_revision, |path| {
            ai_ex_platform_windows::available_disk_space(&path)
        })
        .await
    }

    pub(super) async fn storage_space_with(
        &self,
        expected_revision: u64,
        query: impl FnOnce(PathBuf) -> io::Result<u64> + Send + 'static,
    ) -> Result<NativeReply, AppError> {
        let (directory, epoch, configured) = {
            let state = self.lock()?;
            let config = state.config.as_ref().map_err(Clone::clone)?;
            if config.revision != expected_revision {
                return Err(AppError::invalid_transition(
                    "本地设置已改变，请刷新后再查看空间",
                ));
            }
            (
                self.store()?.resolve_storage(config)?.packages,
                state.epoch,
                state.configured,
            )
        };
        let available_bytes = tokio::task::spawn_blocking(move || query(directory).ok())
            .await
            .map_err(|_| AppError::unavailable("无法完成磁盘空间查询"))?;
        let state = self.lock()?;
        if self.closed.load(Ordering::Acquire)
            || state.epoch != epoch
            || state.configured != configured
            || state
                .config
                .as_ref()
                .map_or(true, |config| config.revision != expected_revision)
        {
            return Err(AppError::invalid_transition(
                "本地设置已改变，请刷新后再查看空间",
            ));
        }
        Ok(NativeReply::StorageSpace {
            revision: expected_revision,
            available_bytes,
        })
    }
}
