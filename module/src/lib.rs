//! The geocoder, as a module of geko (docs/MODULES_PLAN.md): it downloads its sources and the
//! country packs of the scope, fuses and indexes every country into its part of the bundle, and
//! serves search, autocomplete, reverse and place lookups under `/geocoder/`. Everything the
//! host asks of it goes through `geko_module_sdk::module::Module`.

mod cache;
mod commands;
mod config;
mod pipeline;
mod serve;
mod sources;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geko_core::{GekoError, Result};
use geko_module_sdk::module::{self, BundleView, Built, Context, Description, Method, Module, ModuleName, Purge, Request, Response, Route};

use crate::pipeline::{Prepared, Setup};

pub const NAME: &str = "geocoder";

/// Opens the geocoder with what the host tells it.
pub fn open(context: Context) -> Result<Box<dyn Module>> {
    Ok(Box::new(Geocoder::open(context)?))
}

pub struct Geocoder {
    setup: Setup,
    packs_dir: PathBuf,
    /// The packs are those of `GEKO_PACKS`, never downloaded: those of the repository, for the checks.
    given_packs: bool,
    prepared: Mutex<Option<Arc<Prepared>>>,
    server: serve::Server,
}

impl Geocoder {
    fn open(context: Context) -> Result<Self> {
        let config = config::Config::parse(&context.config, &context.config_file)?;
        let given = std::env::var_os("GEKO_PACKS");
        let packs_dir = given.clone().map_or_else(|| context.data_dir.join("packs"), PathBuf::from);
        geko_country_pack::init(packs_dir.clone());
        let budget = |ms: u64| (ms > 0).then(|| Duration::from_millis(ms));
        let s = &config.serve;
        let server = serve::Server::new(
            sources::segment_opener(s.preload),
            serve::GeocoderConfig {
                countries: config.served_countries()?,
                search_budget: budget(s.search_budget_ms),
                autocomplete_budget: budget(s.autocomplete_budget_ms),
                slow_query: budget(s.slow_query_ms),
                same_entity_m: s.same_entity_m,
                batch_max: s.batch_max,
                cache_entries: s.cache_entries,
                ..Default::default()
            },
        );
        let setup = Setup { config, scope: context.scope, data_dir: context.data_dir, data: context.data };
        Ok(Self { setup, packs_dir, given_packs: given.is_some(), prepared: Mutex::new(None), server })
    }

    /// The sources of the scope, prepared by `prepare` (or now, if it was not called).
    fn prepared(&self) -> Result<Arc<Prepared>> {
        let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = prepared.as_ref() {
            return Ok(Arc::clone(p));
        }
        let p = Arc::new(pipeline::prepare(&self.setup, &sources::registry())?);
        *prepared = Some(Arc::clone(&p));
        Ok(p)
    }
}

fn route(method: Method, path: &str, pool: bool, public: bool) -> Route {
    Route { method, path: path.to_owned(), pool, public }
}

impl Module for Geocoder {
    fn describe(&self) -> Description {
        Description {
            name: ModuleName::new(NAME).expect("a valid name"),
            version: geko_module_sdk::module_version!().to_owned(),
            contract: module::CONTRACT,
            // Search, autocomplete and batches parse and rank: on the pool. Reverse and lookups
            // by id are cheap and answered where they arrive.
            routes: vec![
                route(Method::Get, "openapi.yaml", false, false),
                route(Method::Get, "v1/attribution/method", false, true),
                route(Method::Get, "v1/search", true, false),
                route(Method::Get, "v1/autocomplete", true, false),
                route(Method::Get, "v1/search/structured", true, false),
                route(Method::Post, "v1/batch", true, false),
                route(Method::Get, "v1/reverse", false, false),
                route(Method::Get, "v1/place/{id}", false, false),
            ],
            openapi: Some("openapi.yaml".into()),
            attribution: Some(serde_json::json!({
                "derivative_database": {
                    "license": "ODbL 1.0",
                    "notice": "The data served are a derivative database of OpenStreetMap, fused with the other sources listed. As ODbL 4.6 allows, the method of making it is described at /geocoder/v1/attribution/method, and the code needed to reproduce it is available on request.",
                    "method": "/geocoder/v1/attribution/method",
                },
            })),
            commands: commands::list(),
        }
    }

    fn prepare(&self) -> Result<String> {
        let mut out = String::new();
        if !self.given_packs {
            out.push_str(&pipeline::ensure_packs(&self.setup, &self.packs_dir)?);
        }
        *self.prepared.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.prepared()?;
        Ok(out)
    }

    fn plan(&self) -> Result<String> {
        Ok(pipeline::plan(&self.setup, &*self.prepared()?, &pipeline::downloader(&self.setup, false)))
    }

    fn fetch(&self, refresh: bool) -> Result<String> {
        pipeline::fetch(&self.setup, &*self.prepared()?, &pipeline::downloader(&self.setup, refresh))
    }

    fn ready(&self) -> Result<()> {
        pipeline::licenses_accepted(&self.setup, &*self.prepared()?)
    }

    fn build(&self, force: bool, refresh: bool) -> Result<Built> {
        pipeline::build(&self.setup, &*self.prepared()?, &pipeline::downloader(&self.setup, refresh), force)
    }

    fn published(&self) -> Result<String> {
        pipeline::published(&self.setup, &*self.prepared()?)
    }

    fn next_build(&self) -> Option<Duration> {
        pipeline::next_build(&self.setup)
    }

    fn purge(&self, what: Purge) -> Result<String> {
        match what {
            Purge::Data => pipeline::purge(&self.setup, &*self.prepared()?),
            Purge::Packages { all } => {
                let scope: Vec<_> = self.setup.scope.countries().collect();
                let mut out = String::new();
                for dir in geko_country_pack::registry::purge(&self.packs_dir, (!all).then_some(scope.as_slice()))? {
                    writeln!(out, "purged {}", dir.display()).unwrap();
                }
                Ok(out)
            }
        }
    }

    fn load(&self, bundle: &BundleView) -> Result<()> {
        self.server.load(bundle)
    }

    fn handle(&self, request: Request) -> Response {
        self.server.handle(request)
    }

    fn metrics(&self, out: &mut String) {
        self.server.metrics(out);
    }

    fn shutdown(&self) -> Result<()> {
        self.server.shutdown();
        Ok(())
    }

    fn command(&self, name: &str, args: &[String]) -> Result<String> {
        if !commands::list().iter().any(|c| c.name == name) && !matches!(name, "help" | "--help" | "-h") {
            return Err(GekoError::Config(format!("the geocoder has no command `{name}`")));
        }
        commands::run(&self.setup.data_dir, name, args)
    }
}
