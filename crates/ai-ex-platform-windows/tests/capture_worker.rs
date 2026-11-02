#![cfg(all(windows, feature = "test-worker"))]

// The shared fixture also serves process-tree and listener tests in job.rs.
#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ai_ex_platform_windows::desktop::{DesktopSystem, PixelBounds, WindowIdentity, WindowSnapshot};
use ai_ex_platform_windows::native::WindowsDesktop;
use support::{Fixture, SERIAL};

fn target(mode: u64) -> WindowSnapshot {
    WindowSnapshot {
        identity: WindowIdentity {
            handle: mode,
            process_id: 0,
            process_started_ticks: 0,
        },
        title: "fake child protocol, never inspected".into(),
        executable: "fixture".into(),
        client: PixelBounds {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        },
        desktop: PixelBounds {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        },
        dpi: 96,
        foreground: true,
    }
}

#[test]
fn worker_output_is_bounded_and_timeout_reaps_only_owned_child() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let jobs = fixture.0.join("jobs");
    let mut desktop = WindowsDesktop::new(fixture.0.join("程序 空格.exe"), jobs.clone()).unwrap();
    let frame = desktop
        .capture(&target(3), Duration::from_secs(2), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(frame.rgba, [255; 16]);
    assert!(
        desktop
            .capture(&target(2), Duration::from_secs(2), &AtomicBool::new(false))
            .is_err()
    );
    let started = Instant::now();
    let error = desktop
        .capture(
            &target(1),
            Duration::from_millis(200),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
    desktop.shutdown().unwrap();
    assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 0);
    drop(desktop);
    std::fs::remove_dir(jobs).unwrap();
}

#[test]
fn cancellation_after_worker_started_discards_output_and_reaps() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let jobs = fixture.0.join("jobs");
    let mut desktop = WindowsDesktop::new(fixture.0.join("程序 空格.exe"), jobs.clone()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let watching = jobs.clone();
    let signal = cancelled.clone();
    let watcher = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let ready = std::fs::read_dir(&watching)
                .unwrap()
                .flatten()
                .any(|entry| entry.path().join("worker.started").is_file());
            if ready {
                signal.store(true, Ordering::Release);
                return true;
            }
            if started.elapsed() > Duration::from_secs(3) {
                signal.store(true, Ordering::Release);
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let error = desktop
        .capture(&target(1), Duration::from_secs(5), &cancelled)
        .unwrap_err();
    assert!(
        watcher.join().unwrap(),
        "cancel applies after the owned child actually started"
    );
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "{error}");
    desktop.shutdown().unwrap();
    assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 0);
    drop(desktop);
    std::fs::remove_dir(jobs).unwrap();
}
