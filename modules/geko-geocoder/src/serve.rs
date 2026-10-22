//! What the geocoder serves (PROJECT.md §10): loads the country segments of a bundle and answers
//! search, autocomplete, reverse and place lookups as GeoJSON. Providers are opened through a
//! function given by `sources`, so the answers know no concrete technology.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use geko_core::country::CountryCode;
use geko_core::geo::{BBox, Coord};
use geko_core::model::{AdminLevelRef, MatchType, PlaceKind, PublicId};
use geko_core::ports::{AddressParser, GeocodeEngine, ReverseIndex};
use geko_core::query::{AddressComponents, Hit, Interpretation, InterpretationKind, KindFilter, ParsedQuery, QueryContext, RankingWeights};
use geko_core::{GekoError, Result};
use geko_core::segment::Meta;
use geko_module_sdk::module::{BundleView, Request, Response};

use crate::cache;

/// Longest accepted `text`, in characters: a long text only costs time, never finds more.
const MAX_TEXT_CHARS: usize = 256;

pub struct Segment {
    pub country: CountryCode,
    pub engine: Box<dyn GeocodeEngine>,
    pub reverse: Box<dyn ReverseIndex>,
    pub parser: Box<dyn AddressParser>,
}

/// Opens one country segment of a bundle (its directory and manifest entry).
pub type SegmentOpener = Arc<dyn Fn(&Path, &geko_core::segment::Segment) -> Result<Segment> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct GeocoderConfig {
    /// Countries this node serves; empty means every country of the bundle.
    pub countries: Vec<CountryCode>,
    pub default_size: usize,
    pub max_size: usize,
    pub weights: RankingWeights,
    /// Time budgets of a search and of an autocomplete request (`QueryContext::deadline`),
    /// counted from when the engine starts working on it; `None` means no budget.
    pub search_budget: Option<std::time::Duration>,
    pub autocomplete_budget: Option<std::time::Duration>,
    /// Requests that take longer are logged with their query; `None` logs none.
    pub slow_query: Option<std::time::Duration>,
    /// Results with the same name closer than this, in metres, are merged in the answer
    /// (PROJECT.md §6.5.6); 0 merges only the same id.
    pub same_entity_m: f64,
    /// Most requests in one `/v1/batch`.
    pub batch_max: usize,
    /// Search and autocomplete answers kept for repeated queries; 0 keeps none.
    pub cache_entries: usize,
}

impl Default for GeocoderConfig {
    fn default() -> Self {
        Self {
            countries: Vec::new(),
            default_size: 10,
            max_size: 40,
            weights: RankingWeights::default(),
            search_budget: Some(std::time::Duration::from_millis(15)),
            autocomplete_budget: Some(std::time::Duration::from_millis(5)),
            slow_query: Some(std::time::Duration::from_millis(50)),
            same_entity_m: 100.0,
            batch_max: 100,
            cache_entries: 0,
        }
    }
}

struct Loaded {
    version: String,
    /// The attributions of the sources of the bundle, sent with every answer.
    attribution: String,
    segments: Vec<Segment>,
}

struct Shared {
    config: GeocoderConfig,
    loaded: ArcSwapOption<Loaded>,
    /// Results merged by the safety net: each one is a duplicate that fusion let through.
    merged: AtomicU64,
    cache: cache::Cache,
}

/// The method of making the fused database, offered under ODbL 4.6 to whoever gets results.
const FUSION_METHOD: &str = include_str!("../../../docs/FUSION_METHOD.md");
/// The reference of the API of the geocoder, which the host lists at `/docs`.
pub const OPENAPI: &str = include_str!("../openapi.yaml");
const GEOJSON: &str = "application/geo+json; charset=utf-8";

pub struct Server {
    opener: SegmentOpener,
    shared: Shared,
}

impl Server {
    pub fn new(opener: SegmentOpener, config: GeocoderConfig) -> Self {
        Self { opener, shared: Shared { cache: cache::Cache::new(config.cache_entries), config, loaded: ArcSwapOption::empty(), merged: AtomicU64::new(0) } }
    }

    /// Answers a request to one of the routes of `crate::routes`.
    pub fn handle(&self, request: Request) -> Response {
        // From when the host received it: the wait for a free core is part of its time.
        let now = Instant::now();
        let started = now.checked_sub(Duration::from_micros(request.waited_us)).unwrap_or(now);
        let shared = &self.shared;
        let p: HashMap<String, String> = request.query.into_iter().collect();
        let answer = match request.route.as_str() {
            "openapi.yaml" => return Response::new(200, "application/yaml", OPENAPI),
            "v1/attribution/method" => return Response::new(200, "text/markdown; charset=utf-8", FUSION_METHOD),
            "v1/search" => search_now(shared, &p, false, started),
            "v1/autocomplete" => search_now(shared, &p, true, started),
            "v1/search/structured" => structured_now(shared, &p, started),
            "v1/batch" => batch(shared, &request.body),
            "v1/reverse" => reverse_now(shared, &p, started),
            "v1/place/{id}" => match request.params.iter().find(|(k, _)| k == "id") {
                Some((_, id)) => place(shared, id, started),
                None => Err(ApiError::bad_request("an id is required")),
            },
            other => Err(ApiError::not_found(format!("no route {other}"))),
        };
        match answer {
            Ok(Answer(body)) => Response::new(200, GEOJSON, body),
            Err(e) => Response::error(e.status, &e.message),
        }
    }

    pub fn load(&self, view: &BundleView) -> Result<()> {
        let wanted = &self.shared.config.countries;
        let mut segments = Vec::new();
        for s in &Meta::of(&view.module)?.segments {
            let country = CountryCode::new(&s.country)?;
            if !wanted.is_empty() && !wanted.contains(&country) {
                continue;
            }
            segments.push((self.opener)(&view.root.join(&s.dir), s)?);
        }
        if segments.is_empty() {
            return Err(GekoError::Data(format!("bundle {} has no segment for the served countries", view.manifest.version)));
        }
        let attribution = view.manifest.licenses.iter().map(|l| l.attribution.as_str()).collect::<Vec<_>>().join("; ");
        self.shared.loaded.store(Some(Arc::new(Loaded { version: view.manifest.version.clone(), attribution, segments })));
        self.shared.cache.clear();
        Ok(())
    }

    pub fn shutdown(&self) {
        self.shared.loaded.store(None);
    }

    pub fn metrics(&self, out: &mut String) {
        out.push_str("# HELP geko_merged_duplicates_total Duplicated results merged in answers (PROJECT.md 6.5.6): each is a fusion miss.\n# TYPE geko_merged_duplicates_total counter\n");
        let _ = writeln!(out, "geko_merged_duplicates_total {}", self.shared.merged.load(Ordering::Relaxed));
        if self.shared.cache.enabled() {
            out.push_str("# HELP geko_cache_lookups_total Search and autocomplete answers looked up in the cache, by result.\n# TYPE geko_cache_lookups_total counter\n");
            let _ = writeln!(out, "geko_cache_lookups_total{{result=\"hit\"}} {}", self.shared.cache.hits.load(Ordering::Relaxed));
            let _ = writeln!(out, "geko_cache_lookups_total{{result=\"miss\"}} {}", self.shared.cache.misses.load(Ordering::Relaxed));
        }
    }
}

/// The cache key of a search: the route, the text as the engine compares it (case and runs of
/// spaces do not matter), the focus to about a kilometre, and every other parameter.
fn cache_key(p: &HashMap<String, String>, prefix: bool, text: &str) -> String {
    let mut key = String::from(if prefix { "a\u{1}" } else { "s\u{1}" });
    key.extend(text.split_whitespace().flat_map(|w| [w.to_lowercase(), " ".into()]));
    let mut rest: Vec<(&String, &String)> = p.iter().filter(|(k, _)| !matches!(k.as_str(), "text" | "api_key")).collect();
    rest.sort();
    for (k, v) in rest {
        let v = match k.as_str() {
            "focus.lat" | "focus.lon" | "focus.point.lat" | "focus.point.lon" => v.parse::<f64>().map_or(v.clone(), |x| format!("{x:.2}")),
            _ => v.clone(),
        };
        let _ = write!(key, "\u{1}{k}={v}");
    }
    key
}

/// What a request that fails answers: its status and `{"error": message}`.
#[derive(Debug)]
struct ApiError {
    status: u16,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self { status: 400, message: message.into() }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self { status: 404, message: message.into() }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self { status: 503, message: message.into() }
    }
}

fn loaded(shared: &Shared) -> std::result::Result<Arc<Loaded>, ApiError> {
    shared.loaded.load_full().ok_or_else(|| ApiError::unavailable("no geocoder bundle loaded"))
}

fn number<T: std::str::FromStr>(p: &HashMap<String, String>, key: &str) -> std::result::Result<Option<T>, ApiError> {
    p.get(key).map(|v| v.trim().parse().map_err(|_| ApiError::bad_request(format!("invalid `{key}`: {v:?}")))).transpose()
}

/// A coordinate under its geko name or, for clients written for Pelias, under Pelias's name.
fn coordinate(p: &HashMap<String, String>, key: &str, pelias: &str) -> std::result::Result<Option<f64>, ApiError> {
    match number(p, key)? {
        Some(v) => Ok(Some(v)),
        None => number(p, pelias),
    }
}

fn context(shared: &Shared, p: &HashMap<String, String>) -> std::result::Result<QueryContext, ApiError> {
    let c = &shared.config;
    let size = number::<usize>(p, "size")?.unwrap_or(c.default_size).clamp(1, c.max_size);
    let focus = match (coordinate(p, "focus.lat", "focus.point.lat")?, coordinate(p, "focus.lon", "focus.point.lon")?) {
        (Some(lat), Some(lon)) => Some(Coord::from_degrees(lat, lon).map_err(|e| ApiError::bad_request(e.to_string()))?),
        (None, None) => None,
        _ => return Err(ApiError::bad_request("focus needs both focus.lat and focus.lon")),
    };
    let rect = ["boundary.rect.min_lat", "boundary.rect.min_lon", "boundary.rect.max_lat", "boundary.rect.max_lon"]
        .map(|k| number::<f64>(p, k));
    let boundary = match rect {
        [Ok(Some(a)), Ok(Some(b)), Ok(Some(c)), Ok(Some(d))] => {
            let corner = |lat, lon| Coord::from_degrees(lat, lon).map_err(|e| ApiError::bad_request(e.to_string()));
            Some(BBox::new(corner(a, b)?, corner(c, d)?).map_err(|e| ApiError::bad_request(e.to_string()))?)
        }
        _ => None,
    };
    let countries = match p.get("boundary.country") {
        Some(list) => list
            .split(',')
            .map(|c| CountryCode::new(c.trim()).map_err(|e| ApiError::bad_request(e.to_string())))
            .collect::<std::result::Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    Ok(QueryContext {
        focus,
        boundary,
        countries,
        lang: p.get("lang").cloned(),
        size,
        weights: c.weights,
        debug: p.get("debug").is_some_and(|v| v == "true" || v == "1"),
        min_score: f32::NEG_INFINITY,
        deadline: None,
    })
}

fn match_factor(m: MatchType) -> f32 {
    match m {
        MatchType::Exact | MatchType::Coordinates | MatchType::Cadastral | MatchType::Kilometric => 1.0,
        MatchType::Interpolated => 0.9,
        MatchType::Approximate => 0.8,
        MatchType::StreetOnly | MatchType::LocalityOnly => 0.6,
    }
}

/// How the `confidence` of each result is computed.
#[derive(Clone, Copy)]
enum Confidence {
    /// A text search: the probability that the result is the one asked for (`text_confidence`).
    Text { autocomplete: bool },
    /// Reverse and lookup by id: the score relative to the best result.
    Relative,
}

/// Logistic model of the probability that a text search result is the one asked for, from what
/// any query has: its score, its lead over the next result, how far it is behind the first, its
/// position and its match type (PROJECT.md §10.2). Fitted on the Spain reference queries and
/// keystrokes with the default ranking weights (`tools/confidence/`): calibration error 0.065
/// on a held-out set, where the score relative to the best had 0.48. A result marked 0.9 is right
/// about nine times in ten.
///
/// The first result, the one apps show and act on, has its own model, fitted on the first
/// results of Spain and Luxembourg weighing the same (BACKLOG 16b): calibration error of the first
/// result 0.143 → 0.057 in Spain and 0.167 → 0.082 in Luxembourg on held-out sets.
fn text_confidence(score: f32, next: Option<f32>, top: f32, first: bool, autocomplete: bool, m: MatchType) -> f32 {
    let gap = next.map_or(0.3, |n| (score - n).min(0.5));
    if first {
        let match_weight = match m {
            MatchType::Approximate | MatchType::Interpolated => -6.668,
            MatchType::StreetOnly => -4.710,
            MatchType::LocalityOnly => -7.021,
            MatchType::Exact | MatchType::Coordinates | MatchType::Cadastral | MatchType::Kilometric => 0.0,
        };
        let z = -1.476 + 0.915 * score + 11.069 * gap + if autocomplete { -1.779 } else { 0.0 } + match_weight;
        return 1.0 / (1.0 + (-z).exp());
    }
    let behind = (top - score).min(0.5);
    let match_weight = match m {
        MatchType::Approximate | MatchType::Interpolated => -1.343,
        MatchType::StreetOnly => -1.808,
        MatchType::LocalityOnly => -2.730,
        MatchType::Exact | MatchType::Coordinates | MatchType::Cadastral | MatchType::Kilometric => 0.0,
    };
    let z = -3.183 + 0.395 * score + 5.616 * gap + 0.247 * behind
        + if first { 2.441 } else { 0.0 }
        + if autocomplete { -1.292 } else { 0.0 }
        + match_weight;
    1.0 / (1.0 + (-z).exp())
}

struct Found<'a> {
    segment: &'a Segment,
    hit: Hit,
    distance_m: Option<f64>,
}

/// Where the `"names":{…}` member of a properties fragment is, with one of its commas. Its
/// values are strings, so the first `}` outside a string closes it.
fn names_member(json: &[u8]) -> Option<std::ops::Range<usize>> {
    const KEY: &[u8] = br#""names":{"#;
    let start = json.windows(KEY.len()).position(|w| w == KEY)?;
    let (mut in_string, mut escaped) = (false, false);
    let mut end = None;
    for (i, &b) in json.iter().enumerate().skip(start + KEY.len()) {
        match (in_string, escaped, b) {
            (true, true, _) => escaped = false,
            (true, false, b'\\') => escaped = true,
            (_, false, b'"') => in_string = !in_string,
            (false, _, b'}') => {
                end = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let end = end?;
    Some(if json.get(end) == Some(&b',') { start..end + 1 } else if start > 0 && json[start - 1] == b',' { start - 1..end } else { start..end })
}

fn properties(json: &[u8]) -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_slice(&[b"{".as_slice(), json, b"}"].concat()).unwrap_or_default()
}

/// The name of entity `id` in `lang`, when its data has one.
fn name_in(segment: &Segment, id: &str, lang: &str) -> Option<(String, String)> {
    let place = segment.engine.place(segment.engine.resolve(&PublicId::parse(id).ok()?)?)?;
    let new = properties(&place.json).get("names")?.get(lang)?.as_str()?.to_owned();
    Some((place.name.into_owned(), new))
}

/// The properties of a result with its name, street and municipality in `lang` where the data
/// has them (BACKLOG 15), and without the names in every language.
fn localized(segment: &Segment, json: &[u8], lang: &str) -> Vec<u8> {
    let mut props = properties(json);
    let names = props.remove("names");
    let mut label = props.get("label").and_then(|l| l.as_str()).unwrap_or_default().to_owned();
    let swap = |props: &mut serde_json::Map<String, serde_json::Value>, key: &str, old: &str, new: &str| {
        if let Some(v) = props.get_mut(key).filter(|v| v.as_str() == Some(old)) {
            *v = new.into();
        }
    };
    if let Some(new) = names.as_ref().and_then(|n| n.get(lang)).and_then(|v| v.as_str())
        && let Some(old) = props.get("name").and_then(|v| v.as_str()).map(str::to_owned)
    {
        if let Some(rest) = label.strip_prefix(old.as_str()) {
            label = format!("{new}{rest}");
        }
        swap(&mut props, "name", &old, new);
    }
    for (id_key, key) in [("street_id", "street"), ("municipality_id", "municipality")] {
        let Some((old, new)) = props.get(id_key).and_then(|v| v.as_str()).and_then(|id| name_in(segment, id, lang)) else { continue };
        if let Some(at) = label.rfind(old.as_str()) {
            label.replace_range(at..at + old.len(), &new);
        }
        if key == "street"
            && let Some(name) = props.get("name").and_then(|v| v.as_str()).and_then(|n| n.strip_prefix(old.as_str())).map(|rest| format!("{new}{rest}"))
        {
            props.insert("name".into(), name.into());
        }
        swap(&mut props, key, &old, &new);
    }
    props.insert("label".into(), label.into());
    let text = serde_json::Value::Object(props).to_string();
    text.as_bytes()[1..text.len() - 1].to_vec()
}

/// What a response shows besides its results.
#[derive(Clone, Copy)]
struct Output<'a> {
    debug: bool,
    lang: Option<&'a str>,
    /// Each result with the administrative units that contain it (`admin`), as a reverse
    /// answer gives them for its point.
    admin: bool,
}

impl<'a> Output<'a> {
    fn of(ctx: &'a QueryContext, p: &HashMap<String, String>) -> Self {
        Output { debug: ctx.debug, lang: ctx.lang.as_deref(), admin: p.get("admin").is_some_and(|v| v == "true" || v == "1") }
    }
}

/// Administrative units, from the country down, as JSON.
fn levels_json(levels: &[AdminLevelRef]) -> serde_json::Value {
    levels.iter().map(|l| serde_json::json!({ "kind": l.kind.as_str(), "name": l.name, "code": (!l.code.is_empty()).then_some(l.code) })).collect()
}

/// The administrative units that contain a point.
fn admin_json(segment: &Segment, point: Coord) -> serde_json::Value {
    segment.reverse.admin_at(point).map_or_else(|| serde_json::json!([]), |chain| levels_json(&chain.levels))
}

fn feature(out: &mut String, f: &Found, confidence: f32, output: Output) {
    let (debug, lang) = (output.debug, output.lang);
    let Some(place) = f.segment.engine.place(f.hit.id) else { return };
    let json: std::borrow::Cow<[u8]> = match (lang, names_member(&place.json)) {
        (Some(lang), _) => localized(f.segment, &place.json, lang).into(),
        (None, Some(range)) => [&place.json[..range.start], &place.json[range.end..]].concat().into(),
        (None, None) => std::borrow::Cow::Borrowed(&place.json[..]),
    };
    if !out.ends_with('[') {
        out.push(',');
    }
    let _ = write!(
        out,
        r#"{{"type":"Feature","geometry":{{"type":"Point","coordinates":[{:.7},{:.7}]}},"properties":{{"#,
        place.point.lon(),
        place.point.lat()
    );
    out.push_str(std::str::from_utf8(&json).unwrap_or(r#""gid":"""#));
    let _ = write!(out, r#","country":"{}","confidence":{:.3},"match_type":"{}""#, place.country, confidence, f.hit.match_type.as_str());
    if let Some(d) = f.distance_m {
        let _ = write!(out, r#","distance_m":{d:.1}"#);
    }
    if output.admin {
        let _ = write!(out, r#","admin":{}"#, admin_json(f.segment, place.point));
    }
    if debug {
        let _ = write!(out, r#","score":{:.4},"local_id":{}"#, f.hit.score, f.hit.id.0);
    }
    out.push_str("}}");
}

/// A GeoJSON answer, kept as text: a batch embeds several in one body.
struct Answer(String);

fn respond(
    loaded: &Loaded,
    query: serde_json::Value,
    found: &[Found],
    extra: Option<(&str, serde_json::Value)>,
    started: Instant,
    output: Output<'_>,
    confidence: Confidence,
) -> Answer {
    let top = found.iter().map(|f| f.hit.score).fold(f32::NEG_INFINITY, f32::max);
    let mut geocoding = serde_json::json!({
        "engine": concat!("geko/", env!("CARGO_PKG_VERSION")),
        "bundle": loaded.version,
        "attribution": loaded.attribution,
        "query": query,
        "timing_us": started.elapsed().as_micros() as u64,
    });
    if let Some((k, v)) = extra {
        geocoding[k] = v;
    }
    let mut body = String::with_capacity(512 + found.len() * 600);
    let _ = write!(body, r#"{{"type":"FeatureCollection","geocoding":{geocoding},"features":["#);
    for (i, f) in found.iter().enumerate() {
        let c = match confidence {
            Confidence::Text { autocomplete } => {
                text_confidence(f.hit.score, found.get(i + 1).map(|n| n.hit.score), top, i == 0, autocomplete, f.hit.match_type)
            }
            Confidence::Relative if top > 0.0 => (f.hit.score / top).clamp(0.0, 1.0) * match_factor(f.hit.match_type),
            Confidence::Relative => 0.0,
        };
        feature(&mut body, f, c, output);
    }
    body.push_str("]}");
    Answer(body)
}

/// The safety net of PROJECT.md §6.5.6: drops each result with the id of a better one, or with
/// its name closer than `same_entity_m`, and keeps at most `size`. Returns how many it dropped
/// as duplicates. Results past `size` are never read: they would only fault in cold pages.
fn merge_duplicates(found: &mut Vec<Found>, same_entity_m: f64, size: usize) -> usize {
    let mut kept: Vec<(String, String, Coord)> = Vec::with_capacity(size);
    let mut dropped = 0;
    found.retain(|f| {
        if kept.len() == size {
            return false;
        }
        let Some(place) = f.segment.engine.place(f.hit.id) else { return true };
        let name = place.name.trim().to_lowercase();
        let duplicate = kept.iter().any(|(id, n, point)| {
            *id == place.public_id || (*n == name && !name.is_empty() && point.distance_m(place.point) < same_entity_m)
        });
        if duplicate {
            dropped += 1;
        } else {
            kept.push((place.public_id.into_owned(), name, place.point));
        }
        !duplicate
    });
    dropped
}

/// Searches every served segment with the query that `parse` makes for it, best results first.
fn geocode<'a>(
    loaded: &'a Loaded,
    parse: &dyn Fn(&Segment) -> Result<ParsedQuery>,
    ctx: &mut QueryContext,
    prefix: bool,
    same_entity_m: f64,
    merged: &AtomicU64,
) -> Result<Vec<Found<'a>>> {
    let mut found: Vec<Found> = Vec::new();
    let mut hits = Vec::new();
    for segment in &loaded.segments {
        if !ctx.countries.is_empty() && !ctx.countries.contains(&segment.country) {
            continue;
        }
        let parsed = parse(segment)?;
        if let Some(InterpretationKind::Coordinates(c)) = parsed.interpretations.first().map(|i| &i.kind) {
            segment.reverse.nearest(*c, KindFilter::ALL, ctx.size, &mut hits)?;
            found.extend(hits.drain(..).map(|hit| Found { segment, hit, distance_m: distance(segment, hit, *c) }));
            continue;
        }
        if prefix {
            segment.engine.autocomplete(&parsed, ctx, &mut hits)?;
        } else {
            segment.engine.search(&parsed, ctx, &mut hits)?;
        }
        found.extend(hits.drain(..).map(|hit| Found { segment, hit, distance_m: ctx.focus.and_then(|f| distance(segment, hit, f)) }));
        found.sort_by(|a, b| b.hit.score.total_cmp(&a.hit.score));
        let dropped = merge_duplicates(&mut found, same_entity_m, ctx.size);
        if dropped > 0 {
            merged.fetch_add(dropped as u64, Ordering::Relaxed);
        }
        if found.len() == ctx.size {
            ctx.min_score = found.last().map_or(f32::NEG_INFINITY, |f| f.hit.score);
        }
    }
    Ok(found)
}

fn distance(segment: &Segment, hit: Hit, from: Coord) -> Option<f64> {
    segment.engine.place(hit.id).map(|p| p.point.distance_m(from))
}

fn search_now(shared: &Shared, p: &HashMap<String, String>, prefix: bool, started: Instant) -> std::result::Result<Answer, ApiError> {
    let loaded = loaded(shared)?;
    let text = p.get("text").map(|t| t.trim()).filter(|t| !t.is_empty()).ok_or_else(|| ApiError::bad_request("`text` is required"))?;
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(ApiError::bad_request(format!("`text` is longer than {MAX_TEXT_CHARS} characters")));
    }
    let mut ctx = context(shared, p)?;
    let key = (shared.cache.enabled() && !ctx.debug).then(|| cache_key(p, prefix, text));
    if let Some(cached) = key.as_deref().and_then(|k| shared.cache.get(k)) {
        return Ok(Answer(cached.to_string()));
    }
    // From when work starts, not from when the request arrived: waiting for a free core under
    // load must not cut the search short.
    let budget = if prefix { shared.config.autocomplete_budget } else { shared.config.search_budget };
    ctx.deadline = budget.map(|b| Instant::now() + b);
    let found = geocode(&loaded, &|s: &Segment| s.parser.parse(text), &mut ctx, prefix, shared.config.same_entity_m, &shared.merged)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if let Some(slow) = shared.config.slow_query
        && started.elapsed() >= slow
    {
        let route = if prefix { "autocomplete" } else { "search" };
        tracing::warn!(route, text, ms = started.elapsed().as_millis() as u64, "slow query");
    }
    // An answer the time budget cut short is not kept: under load it would outlive the load.
    let complete = ctx.deadline.is_none_or(|d| Instant::now() < d);
    let query = serde_json::json!({ "text": text, "size": ctx.size });
    let answer = respond(&loaded, query, &found, None, started, Output::of(&ctx, p), Confidence::Text { autocomplete: prefix });
    if let Some(key) = key.filter(|_| complete) {
        shared.cache.put(key, answer.0.as_str().into());
    }
    Ok(answer)
}

/// The query of a structured search for one segment: the fields as they come, with `address`
/// ("Calle Mayor 5") split into street and number by the segment's parser. A street makes it an
/// address; otherwise a locality is a place, and a postcode alone a postcode.
fn structured_query(segment: &Segment, f: &StructuredFields) -> Result<ParsedQuery> {
    let mut c = AddressComponents { postcode: f.postalcode.clone(), locality: f.locality.clone(), ..Default::default() };
    if let Some(address) = &f.address {
        let parsed = segment.parser.parse(address)?;
        if let Some(a) = parsed.interpretations.iter().find_map(|i| match &i.kind {
            InterpretationKind::Address(a) => Some(a),
            _ => None,
        }) {
            c.street = a.street.clone();
            c.housenumber = a.housenumber.clone();
        } else {
            c.street = Some(address.clone());
        }
    }
    if f.street.is_some() {
        c.street = f.street.clone();
    }
    if f.housenumber.is_some() {
        c.housenumber = f.housenumber.clone();
    }
    let kind = match (&c.street, &c.locality, &c.postcode) {
        (Some(_), _, _) => InterpretationKind::Address(c),
        (None, Some(l), _) => InterpretationKind::Place { text: l.clone(), within: None },
        (None, None, Some(pc)) => InterpretationKind::Postcode(pc.clone()),
        (None, None, None) => return Err(GekoError::InvalidInput("one of address, street, locality or postalcode is required".into())),
    };
    let raw = [&f.address, &f.street, &f.housenumber, &f.postalcode, &f.locality].into_iter().flatten().cloned().collect::<Vec<_>>().join(", ");
    Ok(ParsedQuery { raw, interpretations: vec![Interpretation { probability: 1.0, country: segment.country, kind }] })
}

struct StructuredFields {
    address: Option<String>,
    street: Option<String>,
    housenumber: Option<String>,
    postalcode: Option<String>,
    locality: Option<String>,
}

fn structured_now(shared: &Shared, p: &HashMap<String, String>, started: Instant) -> std::result::Result<Answer, ApiError> {
    let loaded = loaded(shared)?;
    let field = |name: &str| -> std::result::Result<Option<String>, ApiError> {
        let v = p.get(name).map(|t| t.trim()).filter(|t| !t.is_empty());
        if v.is_some_and(|t| t.chars().count() > MAX_TEXT_CHARS) {
            return Err(ApiError::bad_request(format!("`{name}` is longer than {MAX_TEXT_CHARS} characters")));
        }
        Ok(v.map(str::to_owned))
    };
    let fields = StructuredFields {
        address: field("address")?,
        street: field("street")?,
        housenumber: field("housenumber")?,
        postalcode: field("postalcode")?,
        locality: field("locality")?,
    };
    if [&fields.address, &fields.street, &fields.postalcode, &fields.locality].iter().all(|f| f.is_none()) {
        return Err(ApiError::bad_request("one of `address`, `street`, `locality` or `postalcode` is required"));
    }
    let mut ctx = context(shared, p)?;
    if let Some(country) = field("country")? {
        ctx.countries = vec![CountryCode::new(&country).map_err(|e| ApiError::bad_request(e.to_string()))?];
    }
    ctx.deadline = shared.config.search_budget.map(|b| Instant::now() + b);
    let found = geocode(&loaded, &|s: &Segment| structured_query(s, &fields), &mut ctx, false, shared.config.same_entity_m, &shared.merged)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let query = serde_json::json!({
        "address": fields.address, "street": fields.street, "housenumber": fields.housenumber,
        "postalcode": fields.postalcode, "locality": fields.locality, "size": ctx.size,
    });
    Ok(respond(&loaded, query, &found, None, started, Output::of(&ctx, p), Confidence::Text { autocomplete: false }))
}

#[derive(serde::Deserialize)]
struct BatchRequest {
    requests: Vec<BatchItem>,
}

#[derive(serde::Deserialize)]
struct BatchItem {
    /// `search`, `autocomplete`, `structured` or `reverse`.
    route: String,
    #[serde(default)]
    params: HashMap<String, String>,
}

/// Several requests in one: answered in order, one after another on a single slot of the
/// search pool (the route is on the pool), so a batch never takes every core from the requests
/// of other clients.
fn batch(shared: &Shared, body: &[u8]) -> std::result::Result<Answer, ApiError> {
    let body: BatchRequest = serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid batch: {e}")))?;
    if body.requests.len() > shared.config.batch_max {
        return Err(ApiError::bad_request(format!("a batch holds at most {} requests", shared.config.batch_max)));
    }
    let mut out = String::from(r#"{"results":["#);
    for (i, item) in body.requests.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let started = Instant::now();
        let answer = match item.route.as_str() {
            "search" => search_now(shared, &item.params, false, started),
            "autocomplete" => search_now(shared, &item.params, true, started),
            "structured" => structured_now(shared, &item.params, started),
            "reverse" => reverse_now(shared, &item.params, started),
            other => Err(ApiError::bad_request(format!("unknown route `{other}`"))),
        };
        match answer {
            Ok(a) => out.push_str(&a.0),
            Err(e) => out.push_str(&serde_json::json!({ "error": e.message, "status": e.status }).to_string()),
        }
    }
    out.push_str("]}");
    Ok(Answer(out))
}

fn layers(list: Option<&String>) -> std::result::Result<KindFilter, ApiError> {
    let Some(list) = list else { return Ok(KindFilter::ALL) };
    let mut kinds = Vec::new();
    for layer in list.split(',').map(str::trim) {
        match layer {
            "address" => kinds.push(PlaceKind::Address),
            "street" => kinds.push(PlaceKind::Street),
            "poi" | "venue" => kinds.push(PlaceKind::Poi),
            "postcode" | "postalcode" => kinds.push(PlaceKind::Postcode),
            "municipality" => kinds.push(PlaceKind::Municipality),
            "neighbourhood" => kinds.push(PlaceKind::Neighbourhood),
            "province" => kinds.push(PlaceKind::Province),
            "region" => kinds.push(PlaceKind::Region),
            "locality" | "place" => kinds.extend([PlaceKind::Locality, PlaceKind::Neighbourhood, PlaceKind::Toponym]),
            other => return Err(ApiError::bad_request(format!("unknown layer `{other}`"))),
        }
    }
    Ok(KindFilter::only(&kinds))
}

fn reverse_now(shared: &Shared, p: &HashMap<String, String>, started: Instant) -> std::result::Result<Answer, ApiError> {
    let loaded = loaded(shared)?;
    let (Some(lat), Some(lon)) = (coordinate(p, "lat", "point.lat")?, coordinate(p, "lon", "point.lon")?) else {
        return Err(ApiError::bad_request("`lat` and `lon` are required"));
    };
    let point = Coord::from_degrees(lat, lon).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let ctx = context(shared, p)?;
    let filter = layers(p.get("layers"))?;
    let mut best: Option<(&Segment, Vec<Hit>, serde_json::Value)> = None;
    for segment in &loaded.segments {
        let Some(chain) = segment.reverse.admin_at(point) else { continue };
        let admin = levels_json(&chain.levels);
        let mut hits = Vec::new();
        segment.reverse.nearest(point, filter, ctx.size, &mut hits).map_err(|e| ApiError::bad_request(e.to_string()))?;
        best = Some((segment, hits, admin));
        break;
    }
    let (found, admin) = match &best {
        Some((segment, hits, admin)) => {
            (hits.iter().map(|&hit| Found { segment, hit, distance_m: distance(segment, hit, point) }).collect::<Vec<_>>(), admin.clone())
        }
        None => (Vec::new(), serde_json::Value::Array(Vec::new())),
    };
    let query = serde_json::json!({ "lat": lat, "lon": lon, "size": ctx.size });
    Ok(respond(&loaded, query, &found, Some(("admin", admin)), started, Output::of(&ctx, p), Confidence::Relative))
}

fn place(shared: &Shared, id: &str, started: Instant) -> std::result::Result<Answer, ApiError> {
    let loaded = loaded(shared)?;
    let public = PublicId::parse(id).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let segment = loaded
        .segments
        .iter()
        .find(|s| s.country == public.country())
        .ok_or_else(|| ApiError::not_found(format!("country {} is not served here", public.country())))?;
    let local = segment.engine.resolve(&public).ok_or_else(|| ApiError::not_found(format!("{id} not found")))?;
    let hit = Hit { country: segment.country, id: local, score: 1.0, match_type: MatchType::Exact };
    let query = serde_json::json!({ "id": id });
    Ok(respond(&loaded, query, &[Found { segment, hit, distance_m: None }], None, started, Output { debug: false, lang: None, admin: false }, Confidence::Relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_member_is_cut_with_one_comma() {
        let cut = |json: &str| {
            let r = names_member(json.as_bytes()).unwrap();
            format!("{}{}", &json[..r.start], &json[r.end..])
        };
        assert_eq!(cut(r#""a":1,"names":{"ca":"x}y","es":"z\"}"},"b":2"#), r#""a":1,"b":2"#);
        assert_eq!(cut(r#""a":1,"names":{"ca":"x"}"#), r#""a":1"#);
        assert_eq!(cut(r#""names":{"ca":"x"},"b":2"#), r#""b":2"#);
        assert!(names_member(br#""a":1"#).is_none());
    }
}
