use ai_ex_domain::AppError;

pub(super) trait OwnedHotkeys: Send {
    fn poll(&mut self) -> Result<Vec<(i32, u64)>, AppError>;
    fn shutdown(&mut self) -> Result<(), AppError>;
}

pub(super) type Factory =
    std::sync::Arc<dyn Fn(u64) -> Result<Box<dyn OwnedHotkeys>, AppError> + Send + Sync>;

pub(super) fn factory() -> Factory {
    std::sync::Arc::new(|generation| {
        Owner::register(generation).map(|owner| Box::new(owner) as Box<dyn OwnedHotkeys>)
    })
}

#[cfg(windows)]
pub(super) struct Owner(ai_ex_platform_windows::native::WindowsHotkeys);
#[cfg(not(windows))]
pub(super) struct Owner;

impl Owner {
    pub(super) fn register(generation: u64) -> Result<Self, AppError> {
        #[cfg(windows)]
        {
            use ai_ex_platform_windows::hotkeys::HotkeyBinding;
            // Ctrl+Alt+F6..F11; MOD_NOREPEAT is added by the owning OS thread.
            let bindings = (1..=6)
                .map(|id| HotkeyBinding {
                    id,
                    modifiers: 3,
                    virtual_key: 0x74 + id as u16,
                })
                .collect();
            ai_ex_platform_windows::native::WindowsHotkeys::register(bindings, generation)
                .map(Self)
                .map_err(|_| {
                    AppError::unavailable("global shortcuts unavailable; a binding may be occupied")
                })
        }
        #[cfg(not(windows))]
        {
            let _generation = generation;
            Err(AppError::unavailable("global shortcuts require Windows"))
        }
    }
}

impl OwnedHotkeys for Owner {
    fn poll(&mut self) -> Result<Vec<(i32, u64)>, AppError> {
        #[cfg(windows)]
        {
            self.0
                .poll()
                .map(|events| {
                    events
                        .into_iter()
                        .map(|event| (event.id, event.generation))
                        .collect()
                })
                .map_err(|_| {
                    AppError::unavailable("global shortcuts stopped; apply settings again")
                })
        }
        #[cfg(not(windows))]
        {
            Ok(Vec::new())
        }
    }

    fn shutdown(&mut self) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.0
                .shutdown()
                .map_err(|_| AppError::unavailable("global shortcuts could not stop"))
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }
}
