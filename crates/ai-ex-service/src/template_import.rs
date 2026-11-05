//! Prepare an independent profile for offline template replay.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use ai_ex_automation::{GameProfile, MatchOptions, TemplateMatcher};
use ai_ex_domain::AppError;
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;
const MAX_TEMPLATES: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    profile: PathBuf,
    image: PathBuf,
    target: String,
    output: Option<PathBuf>,
}

// Unknown fields are rejected so an independent copy cannot silently lose data.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfile {
    display_name: String,
    description: String,
    vlm_prompts: Vec<String>,
    default_templates: BTreeMap<String, String>,
    #[serde(default)]
    action_cooldowns: BTreeMap<String, f64>,
}

struct Prepared {
    profile: Vec<u8>,
    images: BTreeMap<String, Vec<u8>>,
    target: String,
    output: Option<PathBuf>,
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn prepare(path: &Path) -> Result<Prepared, AppError> {
    let bytes = crate::template_game::read_limited(path, MAX_INPUT)?;
    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::configuration(format!("invalid template import request: {e}")))?;
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let profile_path = resolve(base, &request.profile);
    let image_path = resolve(base, &request.image);
    let profile_bytes = crate::template_game::read_limited(&profile_path, MAX_INPUT)?;
    let old: LegacyProfile = serde_json::from_slice(&profile_bytes).map_err(|e| {
        AppError::configuration(format!("invalid or unsupported legacy profile: {e}"))
    })?;
    if request.target.trim().is_empty()
        || request.target.len() > 512
        || request.target.chars().any(char::is_control)
    {
        return Err(AppError::configuration("invalid template target"));
    }
    if old.default_templates.len() >= MAX_TEMPLATES {
        return Err(AppError::configuration(
            "too many templates in legacy profile",
        ));
    }
    let name = image_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::configuration("template image needs a Unicode file name"))?;
    let mut profile = GameProfile {
        display_name: old.display_name,
        description: old.description,
        vlm_prompts: old.vlm_prompts,
        default_templates: old.default_templates,
        action_cooldowns: old.action_cooldowns,
    };
    if profile.default_templates.contains_key(&request.target) {
        return Err(AppError::configuration("template target already exists"));
    }
    profile
        .default_templates
        .insert(request.target.clone(), name.to_owned());
    profile.validate()?;
    let bytes = crate::template_game::read_limited(&image_path, MAX_IMAGE)?;
    validate_image(name, &bytes)?;
    let mut images = BTreeMap::from([(name.to_owned(), bytes)]);
    let mut names = BTreeSet::from([name.to_lowercase()]);
    let template_dir = profile_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("templates");
    for (key, file) in &profile.default_templates {
        if key == &request.target {
            continue;
        }
        let same_name = file == name;
        if !same_name && !names.insert(file.to_lowercase()) {
            if images.contains_key(file) {
                continue;
            }
            return Err(AppError::configuration(
                "template file names conflict on Windows",
            ));
        }
        let directory = fs::canonicalize(&template_dir)
            .map_err(|_| AppError::configuration("legacy templates directory is unavailable"))?;
        let source = fs::canonicalize(directory.join(file))
            .map_err(|_| AppError::configuration("referenced legacy template is unavailable"))?;
        if !source.starts_with(&directory) {
            return Err(AppError::configuration(
                "legacy template escapes its directory",
            ));
        }
        let bytes = crate::template_game::read_limited(&source, MAX_IMAGE)?;
        validate_image(file, &bytes)?;
        if same_name {
            if images.get(file) != Some(&bytes) {
                return Err(AppError::configuration(
                    "new image conflicts with an existing template file",
                ));
            }
            continue;
        }
        if images
            .values()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_add(bytes.len())
            > MAX_TOTAL
        {
            return Err(AppError::configuration(
                "template images exceed the combined size limit",
            ));
        }
        images.insert(file.clone(), bytes);
    }
    if images.values().map(Vec::len).sum::<usize>() > MAX_TOTAL {
        return Err(AppError::configuration(
            "template images exceed the combined size limit",
        ));
    }
    let output = request.output.map(|p| resolve(base, &p));
    if let Some(output) = &output {
        validate_output(output, &profile_path)?;
    }
    let profile = serde_json::to_vec_pretty(&profile)
        .map_err(|_| AppError::protocol("cannot encode game profile"))?;
    if profile.len() > MAX_INPUT {
        return Err(AppError::configuration(
            "prepared profile exceeds the replay size limit",
        ));
    }
    Ok(Prepared {
        profile,
        images,
        target: request.target,
        output,
    })
}

fn validate_image(name: &str, bytes: &[u8]) -> Result<(), AppError> {
    let extension = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let format = image::guess_format(bytes)
        .map_err(|_| AppError::configuration("cannot identify template image format"))?;
    let matches = (extension.eq_ignore_ascii_case("png") && format == image::ImageFormat::Png)
        || ((extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg"))
            && format == image::ImageFormat::Jpeg);
    if !matches {
        return Err(AppError::configuration(
            "template extension does not match image format",
        ));
    }
    let image = crate::template_game::load_image_bytes(bytes.to_vec())?;
    TemplateMatcher::new(&image, MatchOptions::default())?;
    Ok(())
}

fn validate_output(output: &Path, profile: &Path) -> Result<(), AppError> {
    if output.file_name().is_none() {
        return Err(AppError::configuration(
            "output requires a new directory name",
        ));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)
        .map_err(|_| AppError::configuration("output parent directory is unavailable"))?;
    let source = fs::canonicalize(profile.parent().unwrap_or(Path::new(".")))
        .map_err(|_| AppError::configuration("legacy profile directory is unavailable"))?;
    let actual_profile = fs::canonicalize(profile)
        .map_err(|_| AppError::configuration("legacy profile is unavailable"))?;
    if parent.starts_with(&source)
        || actual_profile
            .parent()
            .is_some_and(|source| parent.starts_with(source))
    {
        return Err(AppError::configuration(
            "output must be outside the legacy profile directory",
        ));
    }
    match fs::symlink_metadata(output) {
        Ok(_) => Err(AppError::configuration("output already exists")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AppError::configuration("cannot inspect output directory")),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| AppError::unavailable("cannot create new template output file"))?;
    file.write_all(bytes)
        .map_err(|_| AppError::unavailable("cannot write template output file"))
}

pub(crate) fn execute(path: &Path) -> Result<Value, AppError> {
    let prepared = prepare(path)?;
    let names: Vec<_> = prepared.images.keys().cloned().collect();
    let written = if let Some(output) = &prepared.output {
        fs::create_dir(output)
            .map_err(|_| AppError::unavailable("cannot create new template output directory"))?;
        let templates = output.join("templates");
        fs::create_dir(&templates)
            .map_err(|_| AppError::unavailable("cannot create new template image directory"))?;
        for (name, bytes) in &prepared.images {
            write_new(&templates.join(name), bytes)?;
        }
        write_new(&output.join("profile.json"), &prepared.profile)?;
        true
    } else {
        false
    };
    Ok(
        json!({"status": if written { "written" } else { "preview" },
        "execution_mode": "offline_dry_run_preparation", "target": prepared.target,
        "templates": names, "profile": serde_json::from_slice::<Value>(&prepared.profile)
            .map_err(|_| AppError::protocol("cannot decode prepared profile"))?}),
    )
}

#[cfg(test)]
#[path = "template_import_tests.rs"]
mod tests;
