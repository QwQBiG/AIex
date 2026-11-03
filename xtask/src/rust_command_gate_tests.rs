use super::scan;

#[test]
fn detects_standard_tokio_and_import_aliases() {
    let code = r#"
        use std::process::{Command as Child, self as proc};
        use tokio::process::Command;
        fn run() {
            std::process::Command::new("python");
            ::tokio::process::Command::new("python3.11.exe");
            Child::new(r"C:\Program Files\Python\python.exe");
            proc::Command::new("pythonw.exe");
            Command::new("pip3");
        }
    "#;
    assert_eq!(scan(code).unwrap(), 5);
}

#[test]
fn ignores_comments_strings_other_programs_and_dynamic_values() {
    let code = r#"
        use std::process::Command;
        fn run(name: &str) {
            // Command::new("python")
            let _example = "std::process::Command::new(\"python\")";
            Command::new("python-helper.exe");
            Command::new("cargo");
            Command::new(name);
            Command::new(format!("{}", "python"));
        }
    "#;
    assert_eq!(scan(code).unwrap(), 0);
}

#[test]
fn respects_nested_module_and_block_shadowing() {
    let code = r#"
        use std::process::Command;
        mod separate {
            struct Command;
            impl Command { fn new(_: &str) {} }
            fn run() { Command::new("python"); }
        }
        fn outer() {
            { struct Command; impl Command { fn new(_: &str) {} } Command::new("python"); }
            Command::new("python");
            { use tokio::process::Command as Worker; Worker::new("python3"); }
        }
    "#;
    assert_eq!(scan(code).unwrap(), 2);
}

#[test]
fn supports_glob_and_child_module_imports() {
    let code = r#"
        mod child {
            use std::process::*;
            fn run() { Command::new("python3"); }
        }
    "#;
    assert_eq!(scan(code).unwrap(), 1);
    assert!(scan("fn incomplete(").is_err());
}

#[test]
fn resolves_explicit_and_glob_parent_imports_with_shadowing() {
    let code = r#"
        use std::process::{Command, self as process};
        mod explicit {
            use super::Command as Child;
            fn run() { Child::new("python"); super::Command::new("python"); }
        }
        mod glob {
            use super::*;
            fn run() {
                Command::new("python");
                process::Command::new("python");
                { struct Command; impl Command { fn new(_: &str) {} } Command::new("python"); }
            }
        }
        mod module_import {
            use super::process::Command as Child;
            fn run() { Child::new("python"); super::process::Command::new("python"); }
        }
        mod shadowed {
            use super::*;
            struct Command;
            impl Command { fn new(_: &str) {} }
            fn run() { Command::new("python"); }
        }
        mod isolated {
            struct Command;
            impl Command { fn new(_: &str) {} }
            fn run() { Command::new("python"); }
        }
    "#;
    assert_eq!(scan(code).unwrap(), 6);
}
