//! Photon-compatible routes (`/photon/api`, `/photon/reverse`, PROJECT.md §10): the parameters
//! and the answer format of Photon, so a client written for Photon works by changing its base
//! URL. The search is geko's own; only names and shapes are translated.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use geko_server::ApiError;
use serde_json::{Map, Value, json};

use super::{Answer, Shared, reverse_now, search_now};

/// Photon repeats some parameters (`layer`, `osm_tag`): they are read in order, not as a map.
type Params = Query<Vec<(String, String)>>;
type Reply = std::result::Result<Answer, ApiError>;

pub(super) fn router() -> Router<Arc<Shared>> {
    Router::new().route("/photon/api", get(search)).route("/photon/reverse", get(reverse))
}

/// One `osm_tag` filter: `key`, `key:value`, `:value`, each negated with a leading `!`, or
/// `key:!value` (that key with any other value).
#[derive(Debug, Clone, PartialEq)]
struct TagFilter {
    key: Option<String>,
    value: Option<String>,
    /// The whole filter is negated: the result must not match it.
    not: bool,
    /// Only the value is negated: the key must match and the value must not.
    not_value: bool,
}

impl TagFilter {
    fn parse(s: &str) -> Option<Self> {
        let (not, s) = s.strip_prefix('!').map_or((false, s), |rest| (true, rest));
        let (key, value) = s.split_once(':').map_or((s, None), |(k, v)| (k, Some(v)));
        let (not_value, value) = match value {
            Some(v) => v.strip_prefix('!').map_or((false, Some(v)), |rest| (true, Some(rest))),
            None => (false, None),
        };
        let some = |x: &str| (!x.is_empty()).then(|| x.to_owned());
        let filter = TagFilter { key: some(key), value: value.and_then(some), not, not_value };
        (filter.key.is_some() || filter.value.is_some()).then_some(filter)
    }

    /// Whether the key and value of a result match the filter, before any negation of it.
    fn matches(&self, key: &str, value: &str) -> bool {
        let key_ok = self.key.as_deref().is_none_or(|k| k == key);
        let value_ok = self.value.as_deref().is_none_or(|v| (v == value) != self.not_value);
        key_ok && value_ok
    }
}

/// Whether a result with this OSM key and value passes the filters: at least one of the
/// positive ones, if there are any, and none of the negated.
fn passes(filters: &[TagFilter], key: &str, value: &str) -> bool {
    let (negated, positive): (Vec<&TagFilter>, Vec<&TagFilter>) = filters.iter().partition(|f| f.not);
    (positive.is_empty() || positive.iter().any(|f| f.matches(key, value))) && !negated.iter().any(|f| f.matches(key, value))
}

/// What a Photon request asks for, in geko's parameters.
#[derive(Debug, Default)]
struct Request {
    params: HashMap<String, String>,
    tags: Vec<TagFilter>,
    /// Results to give; with tag filters, geko is asked for more and the answer cut to this.
    limit: Option<usize>,
}

/// Photon's parameters as geko's: `q`, `limit`, `lat`/`lon` (the focus of a search, the point of
/// a reverse), `layer`, `bbox` and `osm_tag`.
fn translate(pairs: &[(String, String)], reverse: bool, max_size: usize) -> std::result::Result<Request, ApiError> {
    let mut out = Request::default();
    let mut layers = Vec::new();
    for (k, v) in pairs {
        let mut put = |to: &str| {
            out.params.insert(to.to_owned(), v.clone());
        };
        match k.as_str() {
            "q" => put("text"),
            "lang" => put("lang"),
            "lat" => put(if reverse { "lat" } else { "focus.lat" }),
            "lon" => put(if reverse { "lon" } else { "focus.lon" }),
            "limit" => out.limit = Some(v.trim().parse().map_err(|_| ApiError::bad_request(format!("invalid `limit`: {v:?}")))?),
            "layer" => layers.extend(v.split(',').map(|l| l.trim().to_owned())),
            "osm_tag" => out.tags.extend(TagFilter::parse(v.trim())),
            "bbox" => {
                let c: Vec<f64> = v.split(',').map(|x| x.trim().parse::<f64>()).collect::<std::result::Result<_, _>>().map_err(|_| ApiError::bad_request(format!("invalid `bbox`: {v:?}")))?;
                let [min_lon, min_lat, max_lon, max_lat] = c[..] else {
                    return Err(ApiError::bad_request("`bbox` is minLon,minLat,maxLon,maxLat"));
                };
                for (key, value) in [("min_lon", min_lon), ("min_lat", min_lat), ("max_lon", max_lon), ("max_lat", max_lat)] {
                    out.params.insert(format!("boundary.rect.{key}"), value.to_string());
                }
            }
            _ => {}
        }
    }
    if !layers.is_empty() {
        let geko: Vec<&str> = layers
            .iter()
            .flat_map(|x| match x.as_str() {
                "house" => vec!["address", "poi"],
                "street" => vec!["street"],
                "city" => vec!["municipality"],
                "locality" | "district" => vec!["locality", "neighbourhood"],
                "county" => vec!["province"],
                "state" => vec!["region"],
                "country" => vec!["country"],
                other => vec![other],
            })
            .collect();
        out.params.insert("layers".into(), geko.join(","));
    }
    let size = match (out.limit, out.tags.is_empty()) {
        (Some(limit), true) => Some(limit),
        // Some results will not pass the tags: ask for as many as can be.
        (_, false) => Some(max_size),
        (None, true) => None,
    };
    if let Some(size) = size {
        out.params.insert("size".into(), size.to_string());
    }
    out.params.insert("admin".into(), "true".into());
    Ok(out)
}

/// Photon's `type` of each geko layer.
fn kind(layer: &str) -> &str {
    match layer {
        "address" => "house",
        "municipality" => "city",
        "locality" | "neighbourhood" | "toponym" => "locality",
        "province" => "county",
        "region" => "state",
        "postcode" => "postcode",
        "poi" => "house",
        other => other,
    }
}

/// Properties in the Photon schema. OSM keys and ids are given when the entity comes from OSM.
fn properties(p: &Map<String, Value>) -> Map<String, Value> {
    let layer = p.get("layer").and_then(Value::as_str).unwrap_or_default();
    let mut out = Map::new();
    out.insert("type".into(), json!(kind(layer)));
    if let Some(n) = p.get("name").filter(|_| layer != "address") {
        out.insert("name".into(), n.clone());
    }
    for (from, to) in [("housenumber", "housenumber"), ("street", "street"), ("postcode", "postcode"), ("municipality", "city")] {
        if let Some(v) = p.get(from) {
            out.insert(to.into(), v.clone());
        }
    }
    for unit in p.get("admin").and_then(Value::as_array).into_iter().flatten() {
        let (Some(kind), Some(name)) = (unit.get("kind").and_then(Value::as_str), unit.get("name")) else { continue };
        let field = match kind {
            "country" => "country",
            "region" => "state",
            "province" => "county",
            "municipality" => "city",
            "locality" => "locality",
            "neighbourhood" => "district",
            _ => continue,
        };
        out.entry(field).or_insert_with(|| name.clone());
    }
    if let Some(c) = p.get("country").and_then(Value::as_str) {
        out.insert("countrycode".into(), json!(c));
    }
    if let Some((key, value)) = p.get("category").and_then(Value::as_str).and_then(|c| c.split_once('=')) {
        out.insert("osm_key".into(), json!(key));
        out.insert("osm_value".into(), json!(value));
    } else {
        // Photon gives every result a key and a value; geko's own kinds are described with
        // the OSM tags of their kind.
        let (key, value) = match layer {
            "address" => ("place", "house"),
            "street" => ("highway", "residential"),
            "municipality" => ("place", "city"),
            "locality" => ("place", "locality"),
            "neighbourhood" => ("place", "suburb"),
            "postcode" => ("place", "postcode"),
            "province" | "region" | "country" => ("boundary", "administrative"),
            _ => ("place", layer),
        };
        out.insert("osm_key".into(), json!(key));
        out.insert("osm_value".into(), json!(value));
    }
    // Ids of OSM entities end in their element: `…-osm-n123` is node 123.
    if let Some(gid) = p.get("gid").and_then(Value::as_str)
        && let Some(tail) = gid.rsplit_once("-osm-").map(|(_, t)| t)
        && let Some((t, id)) = tail.split_at_checked(1)
        && let (Some(osm_type), Ok(id)) = (match t { "n" => Some("N"), "w" => Some("W"), "r" => Some("R"), _ => None }, id.parse::<u64>())
    {
        out.insert("osm_type".into(), json!(osm_type));
        out.insert("osm_id".into(), json!(id));
    }
    out.insert("gid".into(), p.get("gid").cloned().unwrap_or(Value::Null));
    out
}

fn photon(answer: Answer, request: &Request) -> Reply {
    let v: Value = serde_json::from_str(&answer.0).map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut features: Vec<Value> = v.get("features").and_then(Value::as_array).cloned().unwrap_or_default();
    for f in &mut features {
        if let Some(p) = f.get("properties").and_then(Value::as_object).map(properties) {
            f["properties"] = Value::Object(p);
        }
    }
    if !request.tags.is_empty() {
        let tag = |f: &Value, k: &str| f["properties"][k].as_str().unwrap_or_default().to_owned();
        features.retain(|f| passes(&request.tags, &tag(f, "osm_key"), &tag(f, "osm_value")));
    }
    if let Some(limit) = request.limit {
        features.truncate(limit);
    }
    Ok(Answer(json!({ "type": "FeatureCollection", "features": features }).to_string()))
}

async fn search(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let request = translate(&p, false, shared.config.max_size)?;
    // Photon answers as the text is typed: its last word is a prefix.
    geko_server::blocking(move || photon(search_now(&shared, &request.params, true, Instant::now())?, &request)).await
}

async fn reverse(State(shared): State<Arc<Shared>>, Query(p): Params) -> Reply {
    let request = translate(&p, true, shared.config.max_size)?;
    geko_server::blocking(move || photon(reverse_now(&shared, &request.params, Instant::now())?, &request)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn parameters_become_geko_ones() {
        let r = translate(&pairs(&[("q", "gran via"), ("lat", "40.4"), ("lon", "-3.7"), ("limit", "3"), ("layer", "city"), ("layer", "street"), ("bbox", "-3.8,40.3,-3.6,40.5")]), false, 40).unwrap();
        assert_eq!(r.params["text"], "gran via");
        assert_eq!(r.params["focus.lat"], "40.4");
        assert_eq!(r.params["size"], "3");
        assert_eq!(r.params["layers"], "municipality,street");
        assert_eq!(r.params["boundary.rect.min_lon"], "-3.8");
        assert_eq!(r.params["boundary.rect.max_lat"], "40.5");
        let r = translate(&pairs(&[("lat", "40.4"), ("lon", "-3.7")]), true, 40).unwrap();
        assert_eq!(r.params["lat"], "40.4", "a reverse point, not a focus");
        assert!(translate(&pairs(&[("bbox", "1,2,3")]), false, 40).is_err());
    }

    #[test]
    fn tag_filters_follow_photon() {
        let tags: Vec<TagFilter> = ["amenity", "!amenity:bar"].iter().filter_map(|s| TagFilter::parse(s)).collect();
        assert!(passes(&tags, "amenity", "restaurant"));
        assert!(!passes(&tags, "amenity", "bar"));
        assert!(!passes(&tags, "shop", "bakery"));
        let tags: Vec<TagFilter> = [":bakery", "tourism:museum"].iter().filter_map(|s| TagFilter::parse(s)).collect();
        assert!(passes(&tags, "shop", "bakery"));
        assert!(passes(&tags, "tourism", "museum"));
        assert!(!passes(&tags, "tourism", "hotel"));
        let tags: Vec<TagFilter> = ["tourism:!hotel"].iter().filter_map(|s| TagFilter::parse(s)).collect();
        assert!(passes(&tags, "tourism", "museum"));
        assert!(!passes(&tags, "tourism", "hotel"));
        assert!(!passes(&tags, "amenity", "bar"));
        assert_eq!(TagFilter::parse("!"), None);
    }

    #[test]
    fn answers_take_photon_shape_with_tags_and_limit() {
        let feature = |gid: &str, layer: &str, category: Option<&str>| {
            json!({ "type": "Feature", "geometry": { "type": "Point", "coordinates": [2.65, 39.57] }, "properties": {
                "gid": gid, "layer": layer, "name": "X", "category": category, "country": "ES",
                "admin": [{ "kind": "region", "name": "Illes Balears" }, { "kind": "municipality", "name": "Palma" }],
            }})
        };
        let answer = || {
            Answer(json!({ "type": "FeatureCollection", "geocoding": {}, "features": [
                feature("geko:poi:es-osm-n7", "poi", Some("amenity=bar")),
                feature("geko:poi:es-osm-w8", "poi", Some("amenity=restaurant")),
                feature("geko:street:es-1", "street", None),
            ]}).to_string())
        };
        let request = translate(&pairs(&[("osm_tag", "amenity"), ("limit", "1")]), false, 40).unwrap();
        assert_eq!(request.params["size"], "40", "more are asked for when tags filter them");
        let out: Value = serde_json::from_str(&photon(answer(), &request).unwrap().0).unwrap();
        let features = out["features"].as_array().unwrap();
        assert_eq!(features.len(), 1);
        let p = &features[0]["properties"];
        assert_eq!(p["osm_type"], "N");
        assert_eq!(p["osm_id"], 7);
        assert_eq!(p["osm_value"], "bar");
        assert_eq!(p["state"], "Illes Balears");
        assert_eq!(p["city"], "Palma");
        assert!(out.get("geocoding").is_none());
        let out: Value = serde_json::from_str(&photon(answer(), &translate(&[], false, 40).unwrap()).unwrap().0).unwrap();
        assert_eq!(out["features"][2]["properties"]["osm_key"], "highway", "a street has the key of streets");
    }
}
