use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use super::*;

#[derive(Default)]
struct State {
    owned: BTreeSet<i32>,
    register: Vec<i32>,
    unregister: Vec<i32>,
    fail_id: Option<i32>,
}
struct Fake(Arc<Mutex<State>>);
impl HotkeySystem for Fake {
    fn register(&mut self, binding: HotkeyBinding) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.register.push(binding.id);
        if state.fail_id == Some(binding.id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "fixture conflict",
            ));
        }
        state.owned.insert(binding.id);
        Ok(())
    }
    fn unregister(&mut self, id: i32) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.unregister.push(id);
        assert!(
            state.owned.remove(&id),
            "must never unregister another owner's ID"
        );
        Ok(())
    }
}
fn binding(id: i32, key: u16) -> HotkeyBinding {
    HotkeyBinding {
        id,
        modifiers: 3,
        virtual_key: key,
    }
}

#[test]
fn registration_failure_releases_only_successfully_owned_bindings() {
    let state = Arc::new(Mutex::new(State {
        owned: BTreeSet::from([70]),
        fail_id: Some(2),
        ..Default::default()
    }));
    let error = match HotkeyRegistrations::register(
        Fake(state.clone()),
        &[binding(1, 65), binding(2, 66)],
        9,
    ) {
        Ok(_) => panic!("registration conflict must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    let state = state.lock().unwrap();
    assert_eq!(state.register, vec![1, 2]);
    assert_eq!(state.unregister, vec![1]);
    assert_eq!(state.owned, BTreeSet::from([70]));
}

#[test]
fn stable_event_id_and_generation_are_retired_with_registrations() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut owner =
        HotkeyRegistrations::register(Fake(state.clone()), &[binding(5, 65)], 123).unwrap();
    assert_eq!(
        owner.event(5),
        Some(HotkeyEvent {
            id: 5,
            generation: 123
        })
    );
    assert_eq!(owner.event(6), None);
    owner.close().unwrap();
    assert_eq!(owner.event(5), None);
    drop(owner);
    assert_eq!(state.lock().unwrap().unregister, vec![5]);
}

#[test]
fn invalid_or_duplicate_bindings_do_not_call_the_system() {
    for bindings in [
        vec![binding(1, 65), binding(1, 66)],
        vec![binding(1, 65), binding(2, 65)],
        vec![binding(1, 0x7B)],
        vec![binding(0, 65)],
        vec![HotkeyBinding {
            id: 1,
            modifiers: 0x8000,
            virtual_key: 65,
        }],
    ] {
        let state = Arc::new(Mutex::new(State::default()));
        assert!(HotkeyRegistrations::register(Fake(state.clone()), &bindings, 1).is_err());
        assert!(state.lock().unwrap().register.is_empty());
    }
}
