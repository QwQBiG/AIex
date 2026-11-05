use std::{fs, process::Command};

use image::{Rgba, RgbaImage};
use serde_json::json;
use std::{
    ops::Deref,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Deref for Fixture {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for path in [
            "request.json",
            "new.png",
            "source/profile.json",
            "copy/marker",
        ] {
            let _ = fs::remove_file(self.0.join(path));
        }
        for path in ["source", "copy"] {
            let _ = fs::remove_dir(self.0.join(path));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn template_import_cli_previews_and_rejects_existing_output_without_loading_config() {
    let root =
        Fixture(std::env::temp_dir().join(format!("aiex-template-cli-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&*root).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    let profile =
        json!({"display_name":"测试", "description":"", "vlm_prompts":[], "default_templates":{}});
    let original = serde_json::to_vec(&profile).unwrap();
    fs::write(root.join("source/profile.json"), &original).unwrap();
    RgbaImage::from_fn(20, 20, |x, y| Rgba([x as u8, y as u8, 20, 255]))
        .save(root.join("new.png"))
        .unwrap();
    let mut request = json!({"profile":"source/profile.json", "image":"new.png", "target":"new"});
    let path = root.join("request.json");
    fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
            .current_dir(&*root)
            .args([
                "--config",
                "missing.toml",
                "--template-import",
                "request.json",
            ])
            .output()
            .unwrap()
    };
    let result = run();
    assert!(result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "preview");
    assert_eq!(report["profile"]["default_templates"]["new"], "new.png");
    fs::create_dir(root.join("copy")).unwrap();
    fs::write(root.join("copy/marker"), b"original").unwrap();
    request["output"] = json!("copy");
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read(root.join("copy/marker")).unwrap(), b"original");
    assert_eq!(
        fs::read(root.join("source/profile.json")).unwrap(),
        original
    );
}
