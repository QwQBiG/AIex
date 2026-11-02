#![forbid(unsafe_code)]

mod model;

use ai_ex_native_worker_support::{ModelBackend, VerifiedConfig, serve};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = async {
        let config = VerifiedConfig::<model::Files>::from_args_for(&[
            ModelBackend::SherpaWhisper,
            ModelBackend::SherpaKokoro,
        ])?;
        serve(model::Sherpa::load(config)?).await
    }
    .await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
