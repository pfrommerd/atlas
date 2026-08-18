use std::path::Path;

use jj_lib::{
    config::StackedConfig, settings::UserSettings, signing::Signer, simple_backend::SimpleBackend,
    workspace::Workspace,
};

/// Initializes a Jujutsu workspace backed by jj's native content-addressed
/// backend rather than Git. The repository format is pinned by the exact
/// `jj-lib` dependency in this crate.
pub async fn init_workspace(path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(path)?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let signer = Signer::from_settings(&settings)?;
    Workspace::init_with_backend(
        &settings,
        path,
        &|_settings, store_path| Ok(Box::new(SimpleBackend::init(store_path))),
        signer,
    )
    .await?;
    Ok(())
}
