//! The country pack of Spain, as it is in this repository, for the tests of the engine. The pack
//! itself is data (`pack.toml`, `data/`, `sources/`): `geko` reads it at run time through
//! `geko-country-pack`, and this crate is never part of the binary.

use text_rules::{Pack, RulesNormalizer, RulesParser};

pub fn pack() -> &'static Pack {
    geko_country_pack::repo("ES").text
}

pub fn normalizer() -> RulesNormalizer {
    geko_country_pack::repo("ES").normalizer()
}

pub fn parser() -> RulesParser {
    geko_country_pack::repo("ES").parser()
}
