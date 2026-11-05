use super::*;
use std::cell::RefCell;
use std::path::Path;

fn options(values: &[&str]) -> Result<Options, String> {
    parse(&values.iter().map(OsString::from).collect::<Vec<_>>())
}

#[test]
fn parses_packaging_options_and_rejects_invalid_values() {
    let result = options(&[
        "--project",
        "项目 空格",
        "--output",
        "输出",
        "--jobs",
        "3",
        "--minimum-free-gib",
        "2",
        "--offline",
        "--skip-build",
    ])
    .unwrap();
    assert_eq!(result.project, Path::new("项目 空格"));
    assert_eq!(result.output, Path::new("输出"));
    assert_eq!((result.jobs, result.minimum_free_gib), (3, 2));
    assert!(result.offline && result.skip_build);
    assert!(options(&["--project", ".", "--output", "out", "--jobs", "0"]).is_err());
    assert!(
        options(&[
            "--project",
            ".",
            "--output",
            "out",
            "--offline",
            "--offline"
        ])
        .is_err()
    );
    assert!(options(&["--project", "."]).is_err());
}

#[test]
fn checks_conflicts_before_build_and_stops_on_first_error() {
    let calls = RefCell::new(Vec::new());
    let result = orchestrate(
        false,
        || {
            calls.borrow_mut().push("preflight");
            Ok(())
        },
        |workspace| {
            calls.borrow_mut().push(if workspace == "core" {
                "core"
            } else {
                "desktop"
            });
            Ok(())
        },
        || {
            calls.borrow_mut().push("package");
            Ok(())
        },
    );
    assert!(result.is_ok());
    assert_eq!(*calls.borrow(), ["preflight", "core", "desktop", "package"]);
    calls.borrow_mut().clear();
    let result = orchestrate(
        false,
        || Err("output already exists".into()),
        |_| {
            calls.borrow_mut().push("build");
            Ok(())
        },
        || {
            calls.borrow_mut().push("package");
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(calls.borrow().is_empty());
    let result = orchestrate(
        false,
        || Ok(()),
        |workspace| {
            calls.borrow_mut().push(if workspace == "core" {
                "core"
            } else {
                "desktop"
            });
            Err("build failed".into())
        },
        || {
            calls.borrow_mut().push("package");
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(*calls.borrow(), ["core"]);
    calls.borrow_mut().clear();
    let result = orchestrate(
        true,
        || Ok(()),
        |_| {
            calls.borrow_mut().push("build");
            Ok(())
        },
        || {
            calls.borrow_mut().push("package");
            Ok(())
        },
    );
    assert!(result.is_ok());
    assert_eq!(*calls.borrow(), ["package"]);
}
