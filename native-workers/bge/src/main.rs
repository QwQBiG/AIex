#![forbid(unsafe_code)]

mod model;

use ai_ex_native_worker_support::{ModelBackend, VerifiedConfig, serve};
use model::{Bge, Files};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = async {
        let config = VerifiedConfig::<Files>::from_args(ModelBackend::OnnxEmbedding)?;
        serve(Bge::load(config)?).await
    }
    .await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
