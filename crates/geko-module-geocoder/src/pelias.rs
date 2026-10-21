//! Pelias-compatible routes (`/pelias/v1/*`, PROJECT.md §10): the parameters and the answer
//! format of Pelias, so a client written for Pelias works by changing its base URL. The search
//! is geko's own; only names and shapes are translated.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use geko_core::model::{MatchType, PublicId};
use geko_core::query::Hit;
use geko_server::ApiError;
use serde_json::{Map, Value, json};

use super::{Answer, Confidence, Found, Output, Shared, loaded, respond, reverse_now, search_now, structured_now};

type Params = Query<HashMap<String, String>>;
type Reply = std::result::Result<Answer, ApiError>;

pub(super) fn router() -> Router<Arc<Shared>> {
    Router::new()
        .route("/pelias/v1/search", get(search))
        .route("/pelias/v1/autocomplete", get(autocomplete))
        .route("/pelias/v1/search/structured", get(structured))
        .route("/pelias/v1/reverse", get(reverse))
        .route("/pelias/v1/place", get(place))
}

/// Pelias layer names as geko's: `venue` is a POI, `locality` a town (a municipality or a
/// smaller settlement), `localadmin` the municipality, `county` the province.
fn layers(pelias: &str) -> String {
    pelias
        .split(',')
        .flat_map(|l| match l.trim() {
            "venue" => vec!["poi"],
            "postalcode" => vec!["postcode"],
            "locality" => vec!["municipality", "locality"],
            "localadmin" => vec!["municipality"],
            "neighbourhood" | "borough" => vec!["neighbourhood"],
            "county" => vec!["province"],
            "region" | "macroregion" => vec!["region"],
            other => vec![other],
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `boundary.circle.*`: its centre and radius in metres. Pelias's default radius is 50 km.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Circle {
    lat: f64,
    lon: f64,
    radius_m: f64,
}

/// Pelias's parameters as geko's. A circle is searched as the box around it, and the results
/// outside it are dropped from the answer.
fn translate(mut p: HashMap<String, String>) -> std::result::Result<(HashMap<String, String>, Option<Circle>), ApiError> {
    if let Some(l) = p.remove("layers") {
        p.insert("layers".into(), layers(&l));
    }
    p.insert("admin".into(), "true".into());
    let number = |k: &str| -> std::result::Result<Option<f64>, ApiError> {
        p.get(k).map(|v| v.trim().parse::<f64>().map_err(|_| ApiError::bad_request(format!("invalid `{k}`: {v:?}")))).transpose()
    };
    let circle = match (number("boundary.circle.lat")?, number("boundary.circle.lon")?) {
        (Some(lat), Some(lon)) => {
            let radius_km = number("boundary.circle.radius")?.unwrap_or(50.0);
            if radius_km.is_nan() || radius_km <= 0.0 {
                return Err(ApiError::bad_request("`boundary.circle.radius` must be positive"));
            }
            Some(Circle { lat, lon, radius_m: radius_km * 1000.0 })
        }
        (None, None) => None,
        _ => return Err(ApiError::bad_request("boundary.circle needs both boundary.circle.lat and boundary.circle.lon")),
    };
    if let Some(c) = circle {
        let dlat = c.radius_m / 111_320.0;
        let dlon = dlat / c.lat.to_radians().cos().max(0.01);
        for (k, v) in [
            ("boundary.rect.min_lat", (c.lat - dlat).max(-90.0)),
            ("boundary.rect.max_lat", (c.lat + dlat).min(90.0)),
            ("boundary.rect.min_lon", (c.lon - dlon).max(-180.0)),
            ("boundary.rect.max_lon", (c.lon + dlon).min(180.0)),
        ] {
            p.entry(k.into()).or_insert_with(|| v.to_string());
        }
    }
    Ok((p, circle))
}

fn layer(geko: &str) -> &str {
    match geko {
        "poi" => "venue",
        "postcode" => "postalcode",
        "municipality" | "locality" => "locality",
        "province" => "county",
        other => other,
    }
}

/// Properties in the Pelias schema. Pelias fields geko has no data for are left out.
fn properties(p: &Map<String, Value>) -> Map<String, Value> {
    let s = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
    let geko_layer = p.get("layer").and_then(Value::as_str).unwrap_or_default();
    let mut out = Map::new();
    out.insert("id".into(), s("gid"));
    out.insert("gid".into(), s("gid"));
    out.insert("layer".into(), json!(layer(geko_layer)));
    out.insert("source".into(), p.get("sources").and_then(|v| v.get(0)).cloned().unwrap_or_else(|| json!("geko")));
    out.insert("source_id".into(), s("gid"));
    out.insert("name".into(), s("name"));
    for (from, to) in [("housenumber", "housenumber"), ("street", "street"), ("postcode", "postalcode")] {
        if let Some(v) = p.get(from) {
            out.insert(to.into(), v.clone());
        }
    }
    out.insert("confidence".into(), s("confidence"));
    let match_type = match p.get("match_type").and_then(Value::as_str).unwrap_or_default() {
        m if m == MatchType::Exact.as_str() || m == MatchType::Coordinates.as_str() => "exact",
        m if m == MatchType::Interpolated.as_str() => "interpolated",
        _ => "fallback",
    };
    out.insert("match_type".into(), json!(match_type));
    let point = matches!(geko_layer, "address" | "poi");
    out.insert("accuracy".into(), json!(if point { "point" } else { "centroid" }));
    if let Some(d) = p.get("distance_m").and_then(Value::as_f64) {
        out.insert("distance".into(), json!((d / 10.0).round() / 100.0));
    }
    out.insert("country_code".into(), s("country"));
    // The units that contain the result, under Pelias's names: a province is a county, a
    // municipality its localadmin and locality, unless a smaller settlement is the locality.
    for unit in p.get("admin").and_then(Value::as_array).into_iter().flatten() {
        let (Some(kind), Some(name)) = (unit.get("kind").and_then(Value::as_str), unit.get("name")) else { continue };
        let levels: &[&str] = match kind {
            "country" => &["country"],
            "region" => &["region"],
            "province" => &["county"],
            "municipality" => &["localadmin", "locality"],
            "locality" => &["locality"],
            "neighbourhood" => &["neighbourhood"],
            _ => &[],
        };
        for level in levels {
            out.insert((*level).into(), name.clone());
            // Pelias abbreviates only countries, regions and counties (`region_a`).
            if matches!(*level, "country" | "region" | "county")
                && let Some(code) = unit.get("code").filter(|c| !c.is_null())
            {
                out.insert(format!("{level}_a"), code.clone());
            }
        }
    }
    if let Some(m) = p.get("municipality") {
        for level in ["localadmin", "locality"] {
            out.entry(level).or_insert_with(|| m.clone());
            out.insert(format!("{level}_gid"), s("municipality_id"));
        }
    }
    out.insert("label".into(), s("label"));
    if let Some(v) = p.get("wikidata") {
        out.insert("addendum".into(), json!({ "concordances": { "wd:id": v } }));
    }
    out
}

fn distance_m(lat: f64, lon: f64, c: Circle) -> f64 {
    let dlat = (lat - c.lat).to_radians();
    let dlon = (lon - c.lon).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + c.lat.to_radians().cos() * lat.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * a.sqrt().asin()
}

/// An answer of geko's routes in the Pelias shape, without the results outside `circle`.
fn pelias(answer: Answer, circle: Option<Circle>) -> Reply {
    let mut v: Value = serde_json::from_str(&answer.0).map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let g = v.get("geocoding").cloned().unwrap_or_default();
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    v["geocoding"] = json!({
        "version": "0.2",
        "attribution": g.get("attribution"),
        "query": g.get("query"),
        "engine": { "name": "geko", "author": "geko", "version": env!("CARGO_PKG_VERSION") },
        "timestamp": timestamp,
    });
    if let Some(features) = v.get_mut("features").and_then(Value::as_array_mut) {
        if let Some(c) = circle {
            features.retain(|f| {
                let xy = f["geometry"]["coordinates"].as_array();
                let (lon, lat) = (xy.and_then(|a| a.first()?.as_f64()), xy.and_then(|a| a.get(1)?.as_f64()));
                lon.zip(lat).is_some_and(|(lon, lat)| distance_m(lat, lon, c) <= c.radius_m)
            });
        }
        for f in features {
            if let Some(p) = f.get("properties").and_then(Value::as_object).map(properties) {
                f["properties"] = Value::Object(p);
            }
        }
    }
    Ok(Answer(v.to_string()))
}

async fn search(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let (q, circle) = translate(p)?;
    geko_server::blocking(move || pelias(search_now(&shared, &q, false, Instant::now())?, circle)).await
}

async fn autocomplete(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let (q, circle) = translate(p)?;
    geko_server::blocking(move || pelias(search_now(&shared, &q, true, Instant::now())?, circle)).await
}

/// Pelias's structured fields: its several administrative levels narrow to geko's locality.
async fn structured(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let (mut q, circle) = translate(p)?;
    if !q.contains_key("locality")
        && let Some(l) = ["neighbourhood", "borough"].iter().find_map(|k| q.get(*k).cloned())
    {
        q.insert("locality".into(), l);
    }
    if !q.contains_key("locality")
        && !q.contains_key("address")
        && !q.contains_key("postalcode")
        && let Some(l) = ["county", "region"].iter().find_map(|k| q.get(*k).cloned())
    {
        q.insert("locality".into(), l);
    }
    geko_server::blocking(move || pelias(structured_now(&shared, &q, Instant::now())?, circle)).await
}

async fn reverse(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let (q, circle) = translate(p)?;
    geko_server::blocking(move || pelias(reverse_now(&shared, &q, Instant::now())?, circle)).await
}

/// `ids` holds several gids, comma separated; the unknown ones are left out, as in Pelias.
async fn place(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let started = Instant::now();
    let loaded = loaded(&shared)?;
    let ids = p.get("ids").filter(|v| !v.is_empty()).ok_or_else(|| ApiError::bad_request("`ids` is required"))?;
    let mut found = Vec::new();
    for id in ids.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Ok(public) = PublicId::parse(id) else { continue };
        let Some(segment) = loaded.segments.iter().find(|s| s.country == public.country()) else { continue };
        let Some(local) = segment.engine.resolve(&public) else { continue };
        let hit = Hit { country: segment.country, id: local, score: 1.0, match_type: MatchType::Exact };
        found.push(Found { segment, hit, distance_m: None });
    }
    let query = json!({ "ids": ids });
    let output = Output { debug: false, lang: p.get("lang").map(String::as_str), admin: true };
    pelias(respond(&loaded, query, &found, None, started, output, Confidence::Relative), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn layers_and_a_circle_become_geko_parameters() {
        let (q, circle) = translate(params(&[("layers", "venue,locality"), ("boundary.circle.lat", "39.57"), ("boundary.circle.lon", "2.65"), ("boundary.circle.radius", "2")])).unwrap();
        assert_eq!(q["layers"], "poi,municipality,locality");
        assert_eq!(q["admin"], "true");
        assert_eq!(circle, Some(Circle { lat: 39.57, lon: 2.65, radius_m: 2000.0 }));
        let min_lat: f64 = q["boundary.rect.min_lat"].parse().unwrap();
        assert!((39.57 - min_lat - 2000.0 / 111_320.0).abs() < 1e-9);
        assert!(translate(params(&[("boundary.circle.lat", "39.57")])).is_err(), "a centre needs both coordinates");
    }

    #[test]
    fn results_outside_the_circle_are_dropped() {
        let near = json!({ "type": "Feature", "geometry": { "type": "Point", "coordinates": [2.65, 39.575] }, "properties": { "layer": "poi" } });
        let far = json!({ "type": "Feature", "geometry": { "type": "Point", "coordinates": [2.65, 39.7] }, "properties": { "layer": "poi" } });
        let answer = Answer(json!({ "type": "FeatureCollection", "geocoding": {}, "features": [near, far] }).to_string());
        let out: Value = serde_json::from_str(&pelias(answer, Some(Circle { lat: 39.57, lon: 2.65, radius_m: 2000.0 })).unwrap().0).unwrap();
        assert_eq!(out["features"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn administrative_units_take_pelias_names() {
        let p = json!({
            "gid": "geko:poi:es-osm-n1", "layer": "poi", "name": "Farmacia", "country": "ES", "municipality": "Palma", "municipality_id": "geko:municipality:es-07040",
            "admin": [
                { "kind": "country", "name": "España", "code": "ES" },
                { "kind": "region", "name": "Illes Balears", "code": "ES-IB" },
                { "kind": "province", "name": "Illes Balears", "code": "ES-PM" },
                { "kind": "municipality", "name": "Palma", "code": "07040" },
                { "kind": "neighbourhood", "name": "Son Armadams", "code": null },
            ],
        });
        let out = properties(p.as_object().unwrap());
        assert_eq!(out["layer"], "venue");
        assert_eq!(out["country"], "España");
        assert_eq!(out["region_a"], "ES-IB");
        assert_eq!(out["county"], "Illes Balears");
        assert_eq!(out["localadmin"], "Palma");
        assert_eq!(out["locality"], "Palma");
        assert_eq!(out["locality_gid"], "geko:municipality:es-07040");
        assert_eq!(out["neighbourhood"], "Son Armadams");
        assert!(!out.contains_key("neighbourhood_a") && !out.contains_key("localadmin_a"), "Pelias abbreviates no municipality");
        assert!(!out.contains_key("admin"), "geko's own field is not passed on");
    }
}
