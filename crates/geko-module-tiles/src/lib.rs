use axum::Router;
use axum::routing::get;
use geko_core::Result;
use geko_server::{ApiError, BundleView, Module, ModuleName};

#[derive(Default)]
pub struct TilesModule;

impl TilesModule {
    pub fn new() -> Self {
        Self
    }
}

async fn no_bundle() -> ApiError {
    ApiError::unavailable("no tiles bundle loaded")
}

impl Module for TilesModule {
    fn name(&self) -> ModuleName {
        ModuleName::Tiles
    }

    fn load(&self, _bundle: &BundleView) -> Result<()> {
        Ok(())
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/tiles/{z}/{x}/{y}", get(no_bundle))
            .route("/tiles/tilejson.json", get(no_bundle))
    }

    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
