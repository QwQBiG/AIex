use ai_ex_domain::{NativeReply, NativeRequest, NativeState};

use super::*;

impl NativeRuntime {
    pub(crate) async fn request(
        self: &Arc<Self>,
        request: NativeRequest,
    ) -> Result<NativeReply, AppError> {
        request.validate()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        let reply = match request {
            NativeRequest::State => {
                let state = self.lock()?;
                NativeReply::State(NativeState {
                    configured: state.configured,
                    configuration_path: self.configuration_path.to_string_lossy().into_owned(),
                    preview_active: state.preview.is_some(),
                    preview_generation: state.preview_generation,
                })
            }
            NativeRequest::Settings => self.settings_reply()?,
            NativeRequest::InputDevices => self.input_devices().await?,
            NativeRequest::InputStatus { instance_id } => NativeReply::InputStatus {
                instance_id,
                status: self.input_status(),
            },
            NativeRequest::Packages => NativeReply::Packages(self.packages()?),
            NativeRequest::StorageSpace { expected_revision } => {
                self.storage_space(expected_revision).await?
            }
            NativeRequest::Download { package_id } => {
                let _mutation = self.mutation.lock().await;
                let manager = self.manager()?;
                let id = manager.start_download(&package_id).await?;
                NativeReply::Operation(packages::operation(manager.operation(id)?))
            }
            NativeRequest::Import { package_id, path } => {
                let _mutation = self.mutation.lock().await;
                let manager = self.manager()?;
                let id = manager
                    .start_import(&package_id, PathBuf::from(path))
                    .await?;
                NativeReply::Operation(packages::operation(manager.operation(id)?))
            }
            NativeRequest::Remove { package_id } => self.remove_package(&package_id).await?,
            NativeRequest::Operation { id } => {
                NativeReply::Operation(match self.operations.get(id)? {
                    Some(operation) => operation,
                    None => packages::operation(self.manager()?.operation(id)?),
                })
            }
            NativeRequest::Cancel { id } => {
                if self.operations.cancel(id)?.is_none() {
                    self.manager()?.cancel(id)?;
                }
                NativeReply::Accepted
            }
            NativeRequest::SaveSettings {
                expected_revision,
                document,
            } => self.save_settings(expected_revision, &document).await?,
            NativeRequest::CreateDefaults => self.create_defaults().await?,
            NativeRequest::BindVoice {
                profile_id,
                voice_id,
                expected_revision,
            } => {
                self.bind_voice(profile_id, voice_id, expected_revision)
                    .await?
            }
            NativeRequest::ImportVoice {
                name,
                path,
                text,
                language,
                expected_revision,
            } => {
                self.import_voice(name, PathBuf::from(path), text, language, expected_revision)
                    .await?
            }
            NativeRequest::PreviewVoice {
                voice_id,
                text,
                expected_generation,
            } => {
                self.start_preview(voice_id, text, expected_generation)
                    .await?
            }
            NativeRequest::StopPreview => {
                self.stop_preview().await?;
                NativeReply::Accepted
            }
        };
        reply.validate()?;
        Ok(reply)
    }
}
