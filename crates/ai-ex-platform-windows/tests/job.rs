#![cfg(all(windows, feature = "test-worker"))]

mod support;

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessHandleCount, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

use ai_ex_platform_windows::OwnedJobProcess;
use support::{Fixture, SERIAL, wait};

#[test]
fn terminate_reaps_parent_and_descendants_without_touching_external_process() {
    let _serial = SERIAL.lock().unwrap();
    let external_root = Fixture::new();
    let mut external = external_root.external();
    external_root.wait_file("child.ready");
    let root = Fixture::new();
    let process = OwnedJobProcess::spawn(root.launch("tree")).unwrap();
    root.wait_file("child.ready");
    wait(|| process.active_processes().unwrap() >= 2);
    assert!(process.exit_code().unwrap().is_none());
    process.terminate().unwrap();
    wait(|| process.active_processes().unwrap() == 0);
    wait(|| process.exit_code().unwrap().is_some());
    assert_eq!(process.exit_code().unwrap(), Some(1));
    assert!(external.0.try_wait().unwrap().is_none());
}

#[test]
fn dropping_last_job_handle_terminates_the_owned_tree() {
    let _serial = SERIAL.lock().unwrap();
    let root = Fixture::new();
    let process = OwnedJobProcess::spawn(root.launch("tree")).unwrap();
    root.wait_file("child.ready");
    let child: u32 = root.wait_file("child.pid").parse().unwrap();
    let parent = observe(process.process_id());
    let child = observe(child);
    drop(process);
    // SAFETY: these read-only handles refer to the two fixture processes opened above.
    assert_eq!(
        unsafe { WaitForSingleObject(parent.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
    // SAFETY: child remains owned by this observation handle until the wait ends.
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
}

fn observe(id: u32) -> OwnedHandle {
    // SAFETY: id came only from our just-launched fixture; no termination right is requested.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, id) };
    assert!(!handle.is_null());
    // SAFETY: successful OpenProcess transferred one handle ownership reference.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

#[test]
fn listener_ownership_requires_the_exact_loopback_port_and_owned_process() {
    let _serial = SERIAL.lock().unwrap();
    let root = Fixture::new();
    let process = OwnedJobProcess::spawn(root.launch("listen")).unwrap();
    let port: u16 = root.wait_file("port.txt").parse().unwrap();
    wait(|| process.owns_loopback_listener(port).unwrap());
    let external = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(
        !process
            .owns_loopback_listener(external.local_addr().unwrap().port())
            .unwrap()
    );
    process.terminate().unwrap();
    wait(|| process.active_processes().unwrap() == 0);
    wait(|| process.exit_code().unwrap().is_some());
    assert!(!process.owns_loopback_listener(port).unwrap());
}

#[test]
fn unicode_space_arguments_and_explicit_environment_round_trip() {
    let _serial = SERIAL.lock().unwrap();
    let root = Fixture::new();
    let mut launch = root.launch("echo");
    let expected = [
        "",
        "two words",
        "quote\"middle",
        "trailing\\",
        "中文 空格",
        "slash\\\"quoted\\",
    ];
    launch
        .arguments
        .extend(expected.iter().map(|item| support::text(item)));
    launch
        .environment
        .insert("OWNED_JOB_TEST".into(), "显式环境 value".into());
    let process = OwnedJobProcess::spawn(launch).unwrap();
    wait(|| process.exit_code().unwrap().is_some());
    assert_eq!(process.exit_code().unwrap(), Some(0));
    assert_eq!(
        std::fs::read_to_string(root.0.join("arguments.txt")).unwrap(),
        expected.join("\0")
    );
    assert_eq!(root.wait_file("environment.txt"), "显式环境 value");
    let mut invalid = root.launch("echo");
    invalid
        .environment
        .insert("OWNED_JOB_TEST".into(), "one".into());
    invalid
        .environment
        .insert("owned_job_test".into(), "two".into());
    assert!(OwnedJobProcess::spawn(invalid).is_err());
}

#[test]
fn failed_process_creation_does_not_leak_job_handles() {
    let _serial = SERIAL.lock().unwrap();
    let root = Fixture::new();
    std::fs::write(root.0.join("invalid.exe"), b"invalid portable executable").unwrap();
    let fail = || {
        let mut launch = root.launch("echo");
        launch.executable = root.0.join("invalid.exe");
        assert!(OwnedJobProcess::spawn(launch).is_err());
    };
    fail();
    let before = handles();
    for _ in 0..8 {
        fail();
    }
    assert!(handles() <= before + 2, "failed launches leaked handles");
}

fn handles() -> u32 {
    let mut count = 0;
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle; count is writable.
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
        0
    );
    count
}
