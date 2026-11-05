use super::*;
use image::{Rgba, RgbaImage};
use std::ops::Deref;

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
            "manifest.json",
            "new.png",
            "mismatch.jpg",
            "source/profile.json",
            "source/templates/old.png",
            "source/templates/new.png",
            "copy/profile.json",
            "copy/templates/old.png",
            "copy/templates/new.png",
        ] {
            let _ = fs::remove_file(self.0.join(path));
        }
        for path in ["source/templates", "source", "copy/templates", "copy"] {
            let _ = fs::remove_dir(self.0.join(path));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("aiex-template-import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    fs::create_dir(root.join("source/templates")).unwrap();
    Fixture(root)
}

fn image(path: &Path, seed: u8) {
    let image = RgbaImage::from_fn(20, 20, |x, y| {
        Rgba([
            (x as u8).wrapping_mul(7).wrapping_add(seed),
            (y * 5) as u8,
            40,
            255,
        ])
    });
    image.save(path).unwrap();
}

fn profile(root: &Path, templates: Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(&json!({
        "display_name": "旧角色", "description": "说明", "vlm_prompts": ["观察"],
        "default_templates": templates, "action_cooldowns": {"click": 1.2}
    }))
    .unwrap();
    fs::write(root.join("source/profile.json"), &bytes).unwrap();
    bytes
}

fn request(root: &Path, body: Value) -> PathBuf {
    let path = root.join("request.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    path
}

#[test]
fn preview_and_copy_preserve_profile_and_existing_images_without_overwrite() {
    let root = fixture();
    let original = profile(&root, json!({"old": "old.png"}));
    image(&root.join("source/templates/old.png"), 3);
    image(&root.join("new.png"), 9);
    let mut spec = json!({"profile":"source/profile.json", "image":"new.png", "target":"new"});
    let path = request(&root, spec.clone());
    let preview = execute(&path).unwrap();
    assert_eq!(preview["status"], "preview");
    assert_eq!(preview["profile"]["default_templates"]["old"], "old.png");
    assert_eq!(preview["profile"]["default_templates"]["new"], "new.png");
    assert!(!root.join("copy").exists());
    spec["output"] = json!("copy");
    let path = request(&root, spec);
    assert_eq!(execute(&path).unwrap()["status"], "written");
    assert_eq!(
        fs::read(root.join("source/profile.json")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(root.join("copy/templates/old.png")).unwrap(),
        fs::read(root.join("source/templates/old.png")).unwrap()
    );
    assert_eq!(
        fs::read(root.join("copy/templates/new.png")).unwrap(),
        fs::read(root.join("new.png")).unwrap()
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(root.join("copy/profile.json")).unwrap()).unwrap();
    assert_eq!(saved["action_cooldowns"]["click"], 1.2);
    let manifest = json!({"schema_version":1, "profile":"copy/profile.json", "target":"new",
        "desktop":{"left":0,"top":0,"width":20,"height":20},
        "frames":[{"image":"new.png","timestamp_ms":0}]});
    let manifest_path = root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(crate::template_game::load_session(&manifest_path).is_ok());
    assert!(execute(&path).is_err());
    assert_eq!(
        fs::read(root.join("source/profile.json")).unwrap(),
        original
    );
}

#[test]
fn empty_profile_can_be_prepared_but_unsupported_and_conflicting_data_are_rejected() {
    let root = fixture();
    profile(&root, json!({}));
    image(&root.join("new.png"), 9);
    let spec = json!({"profile":"source/profile.json", "image":"new.png", "target":"new"});
    let path = request(&root, spec.clone());
    assert_eq!(
        execute(&path).unwrap()["profile"]["default_templates"]["new"],
        "new.png"
    );
    let mut old: Value =
        serde_json::from_slice(&fs::read(root.join("source/profile.json")).unwrap()).unwrap();
    old["unrecognized"] = json!({"private": true});
    fs::write(
        root.join("source/profile.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    assert!(execute(&path).is_err());
    profile(&root, json!({"old": "new.png"}));
    image(&root.join("source/templates/new.png"), 2);
    assert!(execute(&path).is_err());
    assert!(!root.join("copy").exists());
    profile(&root, json!({}));
    fs::copy(root.join("new.png"), root.join("mismatch.jpg")).unwrap();
    let mismatch = request(
        &root,
        json!({"profile":"source/profile.json", "image":"mismatch.jpg", "target":"new"}),
    );
    assert!(execute(&mismatch).is_err());
}

#[test]
fn preparation_rejects_a_profile_that_would_exceed_replay_limit() {
    let root = fixture();
    image(&root.join("new.png"), 9);
    let mut old =
        json!({"display_name":"name", "description":"", "vlm_prompts":[], "default_templates":{}});
    let empty_size = serde_json::to_vec(&old).unwrap().len();
    old["description"] = json!("x".repeat(MAX_INPUT - empty_size));
    let bytes = serde_json::to_vec(&old).unwrap();
    assert_eq!(bytes.len(), MAX_INPUT);
    fs::write(root.join("source/profile.json"), bytes).unwrap();
    let path = request(
        &root,
        json!({"profile":"source/profile.json", "image":"new.png", "target":"new"}),
    );
    assert!(execute(&path).is_err());
}
