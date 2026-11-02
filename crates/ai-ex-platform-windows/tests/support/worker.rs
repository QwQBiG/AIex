//! Explicit fixture for owned process tests; excluded from normal builds.
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|mode| mode == "--capture") {
        capture_fixture(&args);
        return;
    }
    let directory = Path::new(&args[1]);
    match args[0].to_str().unwrap() {
        "tree" => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.arg("child").arg(directory);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000 | 0x4000);
            }
            let mut child = command.spawn().unwrap();
            std::fs::write(directory.join("child.pid"), child.id().to_string()).unwrap();
            child.wait().unwrap();
            return;
        }
        "child" => {
            std::fs::write(directory.join("child.ready"), b"ready").unwrap();
        }
        "listen" => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            std::fs::write(
                directory.join("port.txt"),
                listener.local_addr().unwrap().port().to_string(),
            )
            .unwrap();
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        "echo" => {
            let text: Vec<_> = args[2..]
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect();
            std::fs::write(directory.join("arguments.txt"), text.join("\0")).unwrap();
            std::fs::write(
                directory.join("environment.txt"),
                std::env::var("OWNED_JOB_TEST").unwrap(),
            )
            .unwrap();
            return;
        }
        _ => std::process::exit(2),
    }
    std::thread::sleep(Duration::from_secs(60));
}

fn capture_fixture(args: &[std::ffi::OsString]) {
    // This binary never reads windows or devices. It only emulates a capture
    // child's process and bounded file protocol for ownership tests.
    let output = Path::new(args.last().unwrap());
    std::fs::write(
        output.parent().unwrap().join("worker.started"),
        std::process::id().to_string(),
    )
    .unwrap();
    match args[1].to_str().unwrap() {
        "1" => std::thread::sleep(Duration::from_secs(60)),
        "2" => std::fs::write(output, b"bad frame").unwrap(),
        "3" => {
            let mut frame = b"AEXCAP01".to_vec();
            frame.extend_from_slice(&2_u32.to_le_bytes());
            frame.extend_from_slice(&2_u32.to_le_bytes());
            frame.extend_from_slice(&[255; 16]);
            std::fs::write(output, frame).unwrap();
        }
        _ => std::process::exit(7),
    }
}
