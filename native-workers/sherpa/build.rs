#![forbid(unsafe_code)]

fn main() {
    println!("cargo:rerun-if-changed=worker.rc");
    println!("cargo:rerun-if-changed=worker.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // eSpeak uses narrow filesystem APIs. The code page is local to this
        // worker process, preserving Unicode model paths without system changes.
        embed_resource::compile("worker.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Sherpa worker requires its UTF-8 application manifest");
    }
}
