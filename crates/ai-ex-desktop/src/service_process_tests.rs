use super::*;

#[cfg(windows)]
const FIXTURE_MODE: &str = "AIEX_MANAGED_SERVICE_TEST_MODE";
#[cfg(windows)]
const FIXTURE_READY: &str = "managed-service-fixture-ready";

#[cfg(windows)]
struct ServiceFixture {
    service: ManagedService,
    output: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl ServiceFixture {
    fn spawn(mode: &str) -> Self {
        use std::io::BufRead;

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "service_process::tests::managed_service_child_fixture",
                "--nocapture",
            ])
            .env(FIXTURE_MODE, mode)
            .stdout(Stdio::piped());
        let mut service = ManagedService::start_command(&mut command).unwrap();
        let stdout = service.child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let output = std::thread::spawn(move || {
            let mut ready = Some(sender);
            for line in std::io::BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if line == FIXTURE_READY => {
                        if let Some(sender) = ready.take() {
                            let _ignored = sender.send(Ok(()));
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if let Some(sender) = ready.take() {
                            let _ignored = sender.send(Err(error.to_string()));
                        }
                        return;
                    }
                }
            }
            if let Some(sender) = ready {
                let _ignored = sender.send(Err("fixture stdout closed before ready".to_owned()));
            }
        });
        let fixture = Self {
            service,
            output: Some(output),
        };
        let ready = receiver.recv_timeout(Duration::from_secs(10));
        assert!(
            matches!(ready, Ok(Ok(()))),
            "fixture startup failed: {ready:?}"
        );
        fixture
    }
}

#[cfg(windows)]
impl Drop for ServiceFixture {
    fn drop(&mut self) {
        // Reap the owned process before joining the thread that drains its stdout.
        let _ignored = self.service.shutdown();
        if let Some(output) = self.output.take() {
            let _ignored = output.join();
        }
    }
}

#[cfg(windows)]
#[test]
fn managed_service_child_fixture() {
    use std::io::{Read, Write};

    let Ok(mode) = std::env::var(FIXTURE_MODE) else {
        return;
    };
    assert!(matches!(mode.as_str(), "lifetime" | "early-exit"));
    let mut output = std::io::stdout().lock();
    writeln!(output, "\n{FIXTURE_READY}").unwrap();
    output.flush().unwrap();
    drop(output);
    if mode == "lifetime" {
        std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
    } else {
        let mut trigger = [0];
        std::io::stdin().read_exact(&mut trigger).unwrap();
        assert_eq!(trigger, [7]);
        std::process::exit(7);
    }
}

#[test]
fn launch_failure_is_reported() {
    let missing = std::env::temp_dir().join(format!("aiex-absent-{}.exe", uuid::Uuid::new_v4()));
    assert!(ManagedService::start_command(&mut Command::new(missing)).is_err());
}

#[cfg(windows)]
#[test]
fn owned_process_exits_after_its_lifetime_pipe_closes() {
    let mut fixture = ServiceFixture::spawn("lifetime");
    let service = &mut fixture.service;
    assert!(service.child.try_wait().unwrap().is_none());
    service
        .shutdown()
        .expect("pipe closure permits graceful exit");
    assert!(service.child.try_wait().unwrap().unwrap().success());
}

#[cfg(windows)]
#[test]
fn early_service_exit_is_reported_once_without_waiting_for_window_close() {
    use std::io::Write;

    let mut fixture = ServiceFixture::spawn("early-exit");
    let service = &mut fixture.service;
    assert_eq!(service.take_failure(), None);
    service
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&[7])
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let failure = loop {
        if let Some(failure) = service.take_failure() {
            break failure;
        }
        assert!(
            Instant::now() < deadline,
            "owned child should exit promptly"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(failure.contains("7"));
    assert!(failure.contains("连接设置"));
    assert_eq!(service.take_failure(), None);
}
