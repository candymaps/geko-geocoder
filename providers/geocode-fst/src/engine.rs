//! Query side of a segment (PROJECT.md §9.5): cascade (exact, prefix, typos), single-pass
//! ranking with early termination, and addresses resolved inside their street's block.

use std::borrow::Cow;
use std::path::Path;

use fst::automaton::Str;
use fst::{Automaton, IntoStreamer, Map, Streamer};
use geko_core::country::CountryCode;
use geko_core::geo::Coord;
use geko_core::model::{LocalId, MatchType, PlaceKind, PlaceRef, PublicId};
use geko_core::ports::{GeocodeEngine, TextNormalizer};
use geko_core::query::{AddressComponents, Hit, InterpretationKind, ParsedQuery, QueryContext};
use geko_core::{GekoError, Result};
use roaring::RoaringBitmap;

use crate::equivalences::{self, Equivalents};
use crate::format::{self, Manifest, NO_MUNICIPALITY, PREFIX_LEN};
use crate::store::{BlobReader, Bytes};

/// Longest word prefixes expand to at most this many words.
const MAX_PREFIX_EXPANSION: usize = 256;
/// A typo is not expanded to words in more than 1 / FUZZY_MAX_SHARE of the entities (2 %), nor
/// below FUZZY_MIN_LIST, so that small segments keep every correction.
const FUZZY_MAX_SHARE: usize = 50;
const FUZZY_MIN_LIST: usize = 10_000;
/// A prefix this short beside other words is checked on names, not intersected (`deferred_prefix`).
const SHORT_PREFIX: usize = 2;
/// Letters from which a word being typed that no word starts as is read with a typo at its start
/// as well (`FstEngine::fuzzy_prefix`).
const TYPED_TYPO_LETTERS: usize = 5;
/// A prefix whose words list more entities than 1 / PREFIX_MAX_SHARE of the index (0.5 %), and at
/// least PREFIX_MIN_POSTINGS, is too costly to expand ("calle", "kal" in Spain): it is checked on
/// names, or served from its precomputed list when typed alone.
const PREFIX_MAX_SHARE: usize = 200;
const PREFIX_MIN_POSTINGS: usize = 5_000;
/// Names checked for a short prefix per search: candidates come most important first, so a very
/// common other word ("la Fu") stops after these instead of walking half the index.
const MAX_NAME_CHECKS: usize = 3_000;
/// Entities visited to resolve a locality text by its words: a text of one common word ("l")
/// must not walk half the index.
const MAX_LOCALITY_SCAN: usize = 20_000;
/// Streets considered for an address before looking up its number.
const MAX_STREET_CANDIDATES: usize = 12;
/// Streets of the query's municipalities (or anywhere, without one) scored for an address.
const MAX_STREETS_SCANNED: usize = 400;
/// Streets visited to find them: a common name ("Calle Real") has thousands of homonyms in a
/// country, and those of a small municipality come last, in importance order. Checking the
/// municipality of a street is one memory read, so this bound only caps the worst case.
const MAX_STREETS_VISITED: usize = 100_000;
/// With a focus and no municipality, streets this close to the focus are candidates even beyond
/// the most important ones: "Calle Mayor 19" next to the user, not in Madrid.
const FOCUS_NEAR_M: f64 = 50_000.0;
/// Localities a "within"/locality part may resolve to.
const MAX_LOCALITIES: usize = 12;
const MAX_TEXT_SCORE: f32 = 1.3;
/// Text bonus while typing for a name that starts with what was typed: about the importance
/// that separates a town from a village.
const NAME_START_BONUS: f32 = 0.1;
const FOCUS_SCALE_KM: f64 = 5.0;
const SAME_ENTITY_M: f64 = 200.0;
/// Words of a query that are searched. Relaxation and typo lookups grow with the square of the
/// word count; no real address or place needs more.
const MAX_QUERY_WORDS: usize = 12;
/// Candidates scanned between two checks of the time budget.
const SCAN_CHECK: usize = 128;

/// Municipalities a locality text refers to, most likely first. The first `direct` are
/// administrative units named exactly as the text; the rest only contain a populated place of
/// that name. Every country has both: a municipality and hamlets elsewhere named like it.
#[derive(Clone)]
struct Within {
    munis: Vec<u32>,
    direct: usize,
}

impl Within {
    fn contains(&self, m: u32) -> bool {
        self.munis.contains(&m)
    }

    /// Whether `m` is a municipality named exactly as the text.
    fn is_direct(&self, m: u32) -> bool {
        self.munis[..self.direct].contains(&m)
    }

    /// Text factor for an entity of municipality `m`: when the text names a municipality, one
    /// reached only through a homonymous populated place is a less likely reading.
    fn factor(&self, m: u32) -> f32 {
        if self.direct > 0 && !self.munis[..self.direct].contains(&m) { INDIRECT_LOCALITY } else { 1.0 }
    }
}

const INDIRECT_LOCALITY: f32 = 0.85;
/// Importance above which a POI is famous: well known abroad (`fusion::ids`, fame up to 0.4 on
/// top of 0.55 for a POI with Wikidata in a large municipality).
const FAMOUS_POI: f32 = 0.6;

/// Whether the time budget of the query is spent (`QueryContext::deadline`).
fn out_of_time(ctx: &QueryContext) -> bool {
    ctx.deadline.is_some_and(|d| std::time::Instant::now() >= d)
}

/// Whether a scan of a secondary reading, `scanned` candidates in, is past `deadline`. The clock
/// is only read every `SCAN_CHECK` candidates.
fn scan_past(deadline: Option<std::time::Instant>, scanned: usize) -> bool {
    scanned % SCAN_CHECK == SCAN_CHECK - 1 && deadline.is_some_and(|d| std::time::Instant::now() >= d)
}

/// Drops a word repeated right after itself ("calle calle calle…" cost up to 190 ms: each copy
/// intersected the same list again) and keeps the first `MAX_QUERY_WORDS`.
fn limit_words(words: &mut Vec<String>) {
    words.dedup();
    words.truncate(MAX_QUERY_WORDS);
}

fn fst_err(e: fst::Error) -> GekoError {
    GekoError::Other(Box::new(e))
}

pub struct FstEngine {
    country: CountryCode,
    normalizer: Box<dyn TextNormalizer>,
    manifest: Manifest,
    equivalents: Equivalents,
    words: Map<Bytes>,
    names: Map<Bytes>,
    prefixes: Map<Bytes>,
    ids: Map<Bytes>,
    postings: Bytes,
    posting_start: Bytes,
    posting_len: Bytes,
    kind: Bytes,
    importance: Bytes,
    lat: Bytes,
    lon: Bytes,
    muni: Bytes,
    ntok: Bytes,
    ltok: Bytes,
    stype: Bytes,
    addr_start: Bytes,
    addr_len: Bytes,
    a_number: Bytes,
    a_suffix: Bytes,
    a_lat: Bytes,
    a_lon: Bytes,
    a_postcode: Bytes,
    a_street: Bytes,
    a_tail: Bytes,
    a_label: Bytes,
    a_meta: Bytes,
    atail: BlobReader,
    alabel: BlobReader,
    json: BlobReader,
    pid: BlobReader,
    name: BlobReader,
}

struct TopK {
    k: usize,
    hits: Vec<Hit>,
    /// Whether a full answer was pushed: not a nearby number, a street alone or a locality alone.
    matched: bool,
    /// Deadline of the reading being run: none for the likeliest one, which always completes.
    deadline: Option<std::time::Instant>,
    /// Localities resolved by earlier readings of the query: its place and address readings
    /// often share the locality text, and resolving one can cost a millisecond.
    localities: Vec<(String, bool, Within)>,
    /// Last resort for a query that found nothing: its readings are searched only translated
    /// into the other languages of the country (`equivalences`).
    translating: bool,
    /// Whether the hits pushed now match the query without one of its words (relaxation).
    relaxing: bool,
    /// Hits whose best score came from a relaxed search.
    relaxed: Vec<LocalId>,
    /// Whether the hit pushed now is in a municipality named exactly as the locality the reading
    /// read in the query.
    locating: bool,
    /// Hits whose best score came from a reading with a resolved locality.
    located: Vec<LocalId>,
    /// Municipality of the hits pushed now, and of each hit kept.
    muni: u32,
    munis: Vec<(LocalId, u32)>,
}

impl TopK {
    fn new(k: usize) -> Self {
        Self { k: k.max(1), hits: Vec::new(), matched: false, deadline: None, localities: Vec::new(), translating: false, relaxing: false, relaxed: Vec::new(), locating: false, located: Vec::new(), muni: NO_MUNICIPALITY, munis: Vec::new() }
    }

    /// Whether the query already has a full answer: fallback searches (typos, relaxation) then
    /// stop. A partial one (a nearby number, the street or the locality alone) does not count.
    fn matched(&self) -> bool {
        self.matched
    }

    fn full(&self) -> bool {
        self.hits.len() >= self.k
    }

    fn min(&self) -> f32 {
        let (best, located) = (self.best_full(), self.located_munis());
        if self.full() { self.hits.iter().map(|h| self.effective(h, best, &located)).fold(f32::INFINITY, f32::min) } else { f32::NEG_INFINITY }
    }

    /// Best score of a hit that matches the whole query in the locality its reading resolved.
    fn best_full(&self) -> Option<f32> {
        self.hits.iter().filter(|h| self.answers_whole(h)).map(|h| h.score).reduce(f32::max)
    }

    fn answers_whole(&self, h: &Hit) -> bool {
        full_match(h) && !self.relaxed.contains(&h.id) && self.located.contains(&h.id)
    }

    fn muni_of(&self, id: LocalId) -> u32 {
        self.munis.iter().find(|(i, _)| *i == id).map_or(NO_MUNICIPALITY, |&(_, m)| m)
    }

    /// Municipalities of the hits that match the whole query in a locality the query names.
    fn located_munis(&self) -> Vec<u32> {
        self.hits.iter().filter(|h| self.answers_whole(h)).map(|h| self.muni_of(h.id)).collect()
    }

    /// Whether a hit found without a word of the query, or through another of its names, lies
    /// outside the municipalities that answer the query whole: those go below them.
    fn partial_elsewhere(&self, h: &Hit, located: &[u32]) -> bool {
        self.relaxed.contains(&h.id) && !located.contains(&self.muni_of(h.id))
    }

    /// The score a hit will rank with: one found without a word of the query goes below the
    /// best that matched it whole in the locality another reading read in it (`run`).
    fn effective(&self, h: &Hit, best_full: Option<f32>, located: &[u32]) -> f32 {
        match best_full {
            Some(best) if self.partial_elsewhere(h, located) => h.score.min(best * 0.99),
            _ => h.score,
        }
    }

    fn push(&mut self, hit: Hit) {
        self.matched |= full_match(&hit);
        let id = hit.id;
        if let Some(existing) = self.hits.iter_mut().find(|h| h.id == hit.id) {
            if hit.score > existing.score {
                *existing = hit;
                self.mark(id);
            }
            return;
        }
        if !self.full() {
            self.hits.push(hit);
        } else {
            // Ranked as it will be at the end: a relaxed hit holds its place only against those
            // it will still be above.
            let whole = !self.relaxing && self.locating && full_match(&hit);
            let best = match self.best_full() {
                Some(b) if whole => Some(b.max(hit.score)),
                Some(b) => Some(b),
                None if whole => Some(hit.score),
                None => None,
            };
            let mut located = self.located_munis();
            if whole {
                located.push(self.muni);
            }
            let incoming = if self.relaxing && !located.contains(&self.muni) { best.map_or(hit.score, |b| hit.score.min(b * 0.99)) } else { hit.score };
            let worst = self
                .hits
                .iter()
                .enumerate()
                .map(|(i, h)| (i, self.effective(h, best, &located)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("full");
            if incoming <= worst.1 {
                return;
            }
            let dropped = self.hits[worst.0].id;
            self.relaxed.retain(|&r| r != dropped);
            self.located.retain(|&r| r != dropped);
            self.munis.retain(|&(r, _)| r != dropped);
            self.hits[worst.0] = hit;
        }
        self.mark(id);
    }

    /// Records whether the score of `id` now comes from a relaxed search, and from a reading
    /// with a resolved locality.
    fn mark(&mut self, id: LocalId) {
        self.relaxed.retain(|&r| r != id);
        if self.relaxing {
            self.relaxed.push(id);
        }
        self.located.retain(|&r| r != id);
        if self.locating {
            self.located.push(id);
        }
        self.munis.retain(|&(r, _)| r != id);
        self.munis.push((id, self.muni));
    }
}

/// Whether a hit answers the whole query: not a nearby number, a street alone or a locality alone.
fn full_match(h: &Hit) -> bool {
    !matches!(h.match_type, MatchType::Approximate | MatchType::StreetOnly | MatchType::LocalityOnly)
}

/// Words within `distance` edits of a word, as an automaton over the word index. Built from
/// tables precomputed once per distance (`levenshtein_automata`): `fst`'s own Levenshtein
/// automaton builds its whole DFA per word, which took about 30 ms for a typo in a long word
/// ("Francsic") and set the tail of autocomplete. Same edits as before: a transposition counts
/// as two.
struct Typos(levenshtein_automata::DFA);

impl Typos {
    fn new(word: &str, distance: u8) -> Self {
        Typos(Self::builder(distance).build_dfa(word))
    }

    /// Words that start with something within `distance` edits of `word`: a word being typed.
    fn starting(word: &str, distance: u8) -> Self {
        Typos(Self::builder(distance).build_prefix_dfa(word))
    }

    fn builder(distance: u8) -> &'static levenshtein_automata::LevenshteinAutomatonBuilder {
        static BUILDERS: std::sync::OnceLock<[levenshtein_automata::LevenshteinAutomatonBuilder; 2]> = std::sync::OnceLock::new();
        let builders = BUILDERS.get_or_init(|| {
            [levenshtein_automata::LevenshteinAutomatonBuilder::new(1, false), levenshtein_automata::LevenshteinAutomatonBuilder::new(2, false)]
        });
        &builders[usize::from(distance.clamp(1, 2) - 1)]
    }
}

impl Automaton for Typos {
    type State = u32;

    fn start(&self) -> u32 {
        self.0.initial_state()
    }

    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), levenshtein_automata::Distance::Exact(_))
    }

    fn can_match(&self, state: &u32) -> bool {
        *state != levenshtein_automata::SINK_STATE
    }

    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

/// Ascending ids present in every list. Each list is sorted ascending.
fn intersect<'a>(lists: &'a [Cow<'a, [u32]>]) -> impl Iterator<Item = u32> + 'a {
    let mut order: Vec<usize> = (0..lists.len()).collect();
    order.sort_by_key(|&i| lists[i].len());
    let driver = order.first().map(|&i| &lists[i][..]).unwrap_or(&[]);
    let mut cursors = vec![0usize; lists.len()];
    driver.iter().copied().filter(move |&id| {
        order[1..].iter().all(|&l| {
            let list = &lists[l];
            let c = &mut cursors[l];
            *c += list[*c..].partition_point(|&x| x < id);
            list.get(*c) == Some(&id)
        })
    })
}

#[derive(Clone, Copy)]
struct Scored {
    text: f32,
    affinity: f32,
}

impl FstEngine {
    pub fn open(dir: &Path, normalizer: Box<dyn TextNormalizer>, preload: bool) -> Result<Self> {
        let manifest = Manifest::read(dir)?;
        let country = CountryCode::new(&manifest.country)?;
        if normalizer.country() != country {
            return Err(GekoError::Config(format!("segment of {country} opened with the text rules of {}", normalizer.country())));
        }
        let hot = dir.join("hot");
        let cold = dir.join("cold");
        let b = |name: &str| Bytes::open(&hot.join(name), preload);
        let map = |path: std::path::PathBuf| -> Result<Map<Bytes>> { Map::new(Bytes::open(&path, preload)?).map_err(fst_err) };
        Ok(Self {
            country,
            normalizer,
            words: map(hot.join("words.fst"))?,
            names: map(hot.join("names.fst"))?,
            prefixes: map(hot.join("prefixes.fst"))?,
            ids: Map::new(Bytes::open(&cold.join("ids.fst"), false)?).map_err(fst_err)?,
            postings: b("postings.bin")?,
            posting_start: b("posting_start.bin")?,
            posting_len: b("posting_len.bin")?,
            kind: b("kind.bin")?,
            importance: b("importance.bin")?,
            lat: b("lat.bin")?,
            lon: b("lon.bin")?,
            muni: b("muni.bin")?,
            ntok: b("ntok.bin")?,
            ltok: b("ltok.bin")?,
            stype: b("stype.bin")?,
            addr_start: b("addr_start.bin")?,
            addr_len: b("addr_len.bin")?,
            a_number: b("a_number.bin")?,
            a_suffix: b("a_suffix.bin")?,
            a_lat: b("a_lat.bin")?,
            a_lon: b("a_lon.bin")?,
            a_postcode: b("a_postcode.bin")?,
            a_street: b("a_street.bin")?,
            a_tail: Bytes::open(&cold.join("a_tail.bin"), false)?,
            a_label: Bytes::open(&cold.join("a_label.bin"), false)?,
            a_meta: Bytes::open(&cold.join("a_meta.bin"), false)?,
            atail: BlobReader::open(&cold, "atail", false)?,
            alabel: BlobReader::open(&cold, "alabel", false)?,
            json: BlobReader::open(&cold, "json", false)?,
            pid: BlobReader::open(&cold, "pid", false)?,
            name: BlobReader::open(&cold, "name", false)?,
            equivalents: std::fs::read_to_string(cold.join(equivalences::FILE)).map(|t| equivalences::parse(&t)).unwrap_or_default(),
            manifest,
        })
    }

    pub fn docs(&self) -> u32 {
        self.manifest.docs
    }

    pub fn addresses(&self) -> u32 {
        self.manifest.addresses
    }

    fn list(&self, idx: u64) -> &[u32] {
        let start = self.posting_start.slice::<u32>()[idx as usize] as usize;
        let len = self.posting_len.slice::<u32>()[idx as usize] as usize;
        &self.postings.slice::<u32>()[start..start + len]
    }

    fn exact(&self, word: &str) -> &[u32] {
        self.words.get(word).map_or(&[], |i| self.list(i))
    }

    fn by_name(&self, name: &str) -> &[u32] {
        self.names.get(name).map_or(&[], |i| self.list(i))
    }

    /// Entities with a name that starts with `text`, sorted; empty when more than
    /// `MAX_PREFIX_EXPANSION` names do, so that no entity gets what the others would miss.
    fn names_starting(&self, text: &str) -> Vec<u32> {
        let mut bitmap = RoaringBitmap::new();
        let mut stream = self.names.search(Str::new(text).starts_with()).into_stream();
        let mut n = 0;
        while let Some((_, idx)) = stream.next() {
            n += 1;
            if n > MAX_PREFIX_EXPANSION {
                return Vec::new();
            }
            bitmap.extend(self.list(idx).iter().copied());
        }
        bitmap.into_iter().collect()
    }

    fn union_stream<A: Automaton>(&self, automaton: A, limit: usize) -> Vec<u32> {
        self.union_stream_within(automaton, limit, usize::MAX)
    }

    /// Like `union_stream`, leaving out words whose posting list is longer than `max_list`.
    fn union_stream_within<A: Automaton>(&self, automaton: A, limit: usize, max_list: usize) -> Vec<u32> {
        let mut bitmap = RoaringBitmap::new();
        let mut stream = self.words.search(automaton).into_stream();
        let mut n = 0;
        while let Some((_, idx)) = stream.next() {
            let list = self.list(idx);
            if list.len() > max_list {
                continue;
            }
            bitmap.extend(list.iter().copied());
            n += 1;
            if n >= limit {
                break;
            }
        }
        bitmap.into_iter().collect()
    }

    /// Entities with a word starting with `word`. The precomputed top results only serve a
    /// prefix typed alone: combined with other words, they could miss the right entity.
    fn prefix(&self, word: &str, alone: bool) -> Cow<'_, [u32]> {
        let precomputed = if word.len() <= PREFIX_LEN { self.prefixes.get(word).map_or(&[][..], |i| self.list(i)) } else { &[] };
        if alone && word.len() <= PREFIX_LEN {
            return Cow::Borrowed(precomputed);
        }
        // Typed alone, a prefix of the commonest words ("calle") is served from the precomputed
        // most important entities of its first letters, filtered by the whole prefix.
        if alone
            && let Some(head) = word.get(..PREFIX_LEN)
            && self.prefix_is_huge(word)
        {
            let top = self.prefixes.get(head).map_or(&[][..], |i| self.list(i));
            return Cow::Owned(top.iter().copied().filter(|&id| self.name_has_prefix(id, word)).collect());
        }
        // The expansion stops after MAX_PREFIX_EXPANSION words in alphabetical order; the
        // precomputed most important entities keep a short prefix ("sa P" → sa Pobla) from
        // losing them.
        let mut ids = self.union_stream(Str::new(word).starts_with(), MAX_PREFIX_EXPANSION);
        if !precomputed.is_empty() {
            ids.extend_from_slice(precomputed);
            ids.sort_unstable();
            ids.dedup();
        }
        Cow::Owned(ids)
    }

    fn fuzzy(&self, word: &str) -> Vec<u32> {
        // Two edits from 5 letters: a transposition ("Detsre" for "Destre") costs two.
        let distance: u8 = if word.chars().count() >= 5 { 2 } else { 1 };
        // A typo read as one of the commonest words of the index ("crarer" as "carrer", in the
        // name of a large share of a country's streets) tells little and costs most: up to
        // 160 ms of a query that took 30 µs. Such words are left out of the expansion.
        let max_list = (self.manifest.docs as usize / FUZZY_MAX_SHARE).max(FUZZY_MIN_LIST);
        self.union_stream_within(Typos::new(word, distance), MAX_PREFIX_EXPANSION, max_list)
    }

    /// Entities with a word that starts like `word` with a typo: one edit up to 7 letters, two
    /// from 8. The commonest words are left out, as in `fuzzy`.
    fn fuzzy_prefix(&self, word: &str) -> Vec<u32> {
        let distance: u8 = if word.chars().count() >= 8 { 2 } else { 1 };
        let max_list = (self.manifest.docs as usize / FUZZY_MAX_SHARE).max(FUZZY_MIN_LIST);
        self.union_stream_within(Typos::starting(word, distance), MAX_PREFIX_EXPANSION, max_list)
    }

    fn tokens(&self, text: &str) -> Vec<String> {
        let mut folded = String::new();
        let mut out = Vec::new();
        self.normalizer.normalize(text, &mut folded);
        self.normalizer.tokenize(&folded, &mut out);
        limit_words(&mut out);
        out
    }

    /// Posting lists for the query words, following the cascade of PROJECT.md §9.5: exact, then
    /// prefix for the word being typed, then typos for words of 4+ letters. Words that match
    /// nothing are dropped. Returns the lists and whether a typo was needed.
    fn word_lists(&self, words: &[String], prefix_last: bool) -> (Vec<Cow<'_, [u32]>>, bool) {
        self.word_lists_with(words, prefix_last, false)
    }

    /// With `typos`, words of 4+ letters also match their misspellings even when they exist as
    /// typed: "borne" is a word somewhere, but "paseo del borne" means "Passeig del Born".
    fn word_lists_with(&self, words: &[String], prefix_last: bool, typos: bool) -> (Vec<Cow<'_, [u32]>>, bool) {
        let mut lists = Vec::new();
        let mut fuzzy = false;
        for (i, w) in words.iter().enumerate() {
            let typing = prefix_last && i == words.len() - 1;
            // A prefix too costly to expand beside other words ("Vejer de l", "Elosegi kal") is not
            // intersected: callers check it on the names of the candidates (`deferred_prefix`).
            if typing && self.deferred_prefix(words, prefix_last).is_some() {
                continue;
            }
            let list: Cow<[u32]> = if typing { self.prefix(w, words.len() == 1) } else { Cow::Borrowed(self.exact(w)) };
            // One of the commonest words, typed as it is, is no typo: expanding it ("santa", "maria")
            // unions lists of hundreds of thousands of entities.
            if typos && w.chars().count() >= 4 && !(prefix_last && i == words.len() - 1) && list.len() <= self.common_bound() {
                let mut union = self.fuzzy(w);
                union.extend_from_slice(&list);
                union.sort_unstable();
                union.dedup();
                fuzzy |= union.len() > list.len();
                if !union.is_empty() {
                    lists.push(Cow::Owned(union));
                }
            } else if !list.is_empty() {
                lists.push(list);
            } else if typing && w.chars().count() >= TYPED_TYPO_LETTERS {
                // No word starts as the one being typed: it has a typo, in a word typed whole
                // ("Oretga") or still being typed ("Aleixn" for "Aleixandre").
                let mut union = self.fuzzy(w);
                union.extend(self.fuzzy_prefix(w));
                union.sort_unstable();
                union.dedup();
                if !union.is_empty() {
                    fuzzy = true;
                    lists.push(Cow::Owned(union));
                }
            } else if w.chars().count() >= 4 {
                let f = self.fuzzy(w);
                if !f.is_empty() {
                    fuzzy = true;
                    lists.push(Cow::Owned(f));
                }
            }
        }
        (lists, fuzzy)
    }

    /// The word being typed, when it is too costly to intersect: beside other words, one or two
    /// letters, or a prefix of the commonest words. Callers check it on candidate names instead.
    fn deferred_prefix<'w>(&self, words: &'w [String], prefix_last: bool) -> Option<&'w str> {
        let last = words.last().filter(|_| prefix_last && words.len() > 1)?;
        (last.chars().count() <= SHORT_PREFIX || self.prefix_is_huge(last)).then_some(last.as_str())
    }

    /// Lists longer than this hold one of the commonest words of the index ("calle", "de").
    fn common_bound(&self) -> usize {
        (self.manifest.docs as usize / PREFIX_MAX_SHARE).max(PREFIX_MIN_POSTINGS)
    }

    /// Whether a search is left with only the commonest words because a word of the query
    /// matched nothing ("Calle de Francsic"): without its discriminating word, scanning them only
    /// returns noise at a great cost, so the search is skipped.
    fn only_common_left(&self, lists: &[Cow<'_, [u32]>], deferred: bool, words: usize) -> bool {
        lists.len() + usize::from(deferred) < words && lists.iter().all(|l| l.len() > self.common_bound())
    }

    /// Whether the words starting with `prefix` list more entities than a prefix may expand to.
    /// Only adds up list lengths, and stops as soon as the bound is passed.
    fn prefix_is_huge(&self, prefix: &str) -> bool {
        let bound = self.common_bound();
        let mut stream = self.words.search(Str::new(prefix).starts_with()).into_stream();
        let mut total = 0;
        while let Some((_, idx)) = stream.next() {
            total += self.list(idx).len();
            if total > bound {
                return true;
            }
        }
        false
    }

    /// Whether the main name of entity `id` has every word of the query (the last one as a
    /// prefix while typing), not only another of its names.
    fn name_covers(&self, id: u32, query: &[String], prefix: bool) -> bool {
        let name = std::str::from_utf8(self.name.get(id as usize)).unwrap_or_default();
        let (mut folded, mut words) = (String::new(), Vec::new());
        self.normalizer.normalize(name, &mut folded);
        self.normalizer.tokenize(&folded, &mut words);
        query.iter().enumerate().all(|(i, q)| {
            let typing = prefix && i == query.len() - 1;
            words.iter().any(|w| w == q || (typing && w.starts_with(q.as_str())))
        })
    }

    /// Whether the name of entity `id` has a word starting with `prefix`.
    fn name_has_prefix(&self, id: u32, prefix: &str) -> bool {
        let name = std::str::from_utf8(self.name.get(id as usize)).unwrap_or_default();
        let (mut folded, mut words) = (String::new(), Vec::new());
        self.normalizer.normalize(name, &mut folded);
        self.normalizer.tokenize(&folded, &mut words);
        words.iter().any(|w| w.starts_with(prefix))
    }

    fn kind_at(&self, id: u32) -> PlaceKind {
        format::kind_of(self.kind.slice::<u8>()[id as usize])
    }

    fn point(&self, id: u32) -> (i32, i32) {
        (self.lon.slice::<i32>()[id as usize], self.lat.slice::<i32>()[id as usize])
    }

    fn importance(&self, id: u32) -> f32 {
        self.importance.slice::<u16>()[id as usize] as f32 / u16::MAX as f32
    }

    fn street_type(&self, id: u32) -> Option<&str> {
        match self.stype.slice::<u16>()[id as usize] {
            0 => None,
            t => self.manifest.street_types.get(t as usize - 1).map(String::as_str),
        }
    }

    fn muni_of(&self, id: u32) -> u32 {
        self.muni.slice::<u32>()[id as usize]
    }

    fn in_area(&self, p: (i32, i32), ctx: &QueryContext) -> bool {
        ctx.boundary.is_none_or(|b| Coord::from_e7(p.1, p.0).is_ok_and(|c| b.contains(c)))
    }

    fn proximity(&self, p: (i32, i32), ctx: &QueryContext) -> f32 {
        match ctx.focus {
            Some(f) => {
                let km = geko_geom::distance_m(p, (f.lon_e7(), f.lat_e7())) / 1000.0;
                (1.0 / (1.0 + km / FOCUS_SCALE_KM)) as f32
            }
            None => 0.0,
        }
    }

    fn score(&self, id: u32, p: (i32, i32), s: Scored, factor: f32, ctx: &QueryContext) -> f32 {
        let w = &ctx.weights;
        factor * (w.text * s.text + w.importance * self.importance(id) + w.distance * self.proximity(p, ctx) + w.kind_affinity * s.affinity)
    }

    fn bound(&self, id: u32, factor: f32, ctx: &QueryContext) -> f32 {
        let w = &ctx.weights;
        let distance = if ctx.focus.is_some() { w.distance } else { 0.0 };
        factor * (w.text * MAX_TEXT_SCORE + w.importance * self.importance(id) + distance + w.kind_affinity)
    }

    /// How famous a POI is, from 0 to 1: its importance past `FAMOUS_POI`.
    fn fame(&self, id: u32) -> f32 {
        ((self.importance(id) - FAMOUS_POI) / (1.0 - FAMOUS_POI)).clamp(0.0, 1.0)
    }

    fn text_score(&self, id: u32, matched: usize, query_words: usize, exact_name: bool, fuzzy: bool) -> f32 {
        let ntok = self.ntok.slice::<u8>()[id as usize].max(1) as f32;
        let coverage = matched as f32 / query_words.max(1) as f32;
        let mut compactness = (query_words as f32 / ntok.max(query_words as f32)).sqrt();
        // A famous place is called by a short form of its long official name: "aeropuerto de
        // barajas" is "Aeropuerto Adolfo Suárez Madrid-Barajas". The longer name costs it less
        // the more famous it is.
        if self.kind_at(id) == PlaceKind::Poi {
            compactness += (1.0 - compactness) * self.fame(id);
        }
        let mut text = coverage * compactness + if exact_name { 0.3 } else { 0.0 };
        if fuzzy {
            text *= 0.8;
        }
        text
    }

    /// Text score while the name is still being typed: a longer name that contains what was
    /// typed is as good a match as an exact one ("Sant Llorenç" may be the start of "Sant
    /// Llorenç des Cardassar"), so neither compactness nor the exact-name bonus decide; a small
    /// bonus only breaks ties, and importance and kind rank the rest. A name that starts with
    /// what was typed is what is being typed more likely than one with it inside: "Oban" is
    /// Obanos before Puebla de Obando.
    fn typed_text_score(matched: usize, query_words: usize, exact_name: bool, starts_name: bool, fuzzy: bool) -> f32 {
        let coverage = matched as f32 / query_words.max(1) as f32;
        let text = coverage + if exact_name { 0.05 } else { 0.0 } + if starts_name { NAME_START_BONUS } else { 0.0 };
        if fuzzy { text * 0.8 } else { text }
    }

    /// Municipalities a locality text refers to (municipalities, localities or neighbourhoods
    /// whose name matches): municipalities named exactly as the text first, then the most
    /// important others. Places named exactly as the text win; only if none is, places whose
    /// name has exactly these words. "soller" is Sóller, not "Son Sóller" in another municipality.
    fn localities(&self, text: &str, prefix: bool) -> Within {
        let mut folded = String::new();
        self.normalizer.normalize(text, &mut folded);
        let named = self.by_name(&folded);
        let direct: Vec<u32> = named.iter().copied().filter(|&id| self.kind_at(id) == PlaceKind::Municipality).take(MAX_LOCALITIES).collect();
        let mut munis = direct.clone();
        for m in self.collect_localities(named.iter().copied()) {
            if munis.len() >= MAX_LOCALITIES {
                break;
            }
            if !munis.contains(&m) {
                munis.push(m);
            }
        }
        // While typing, a hamlet named exactly as the text so far is not the only reading: "Sarri"
        // is one, and also the start of "Sarria". A municipality named so is.
        if !munis.is_empty() && (!prefix || !direct.is_empty()) {
            return Within { direct: direct.len(), munis };
        }
        // While typing, the last word of the locality is a prefix too ("Carrer de Piscis, el Vendr").
        let words = self.tokens(text);
        let deferred = self.deferred_prefix(&words, prefix);
        let (lists, _) = self.word_lists(&words, prefix);
        if lists.is_empty() || lists.len() + usize::from(deferred.is_some()) < words.len() {
            return Within { direct: direct.len(), munis };
        }
        let ltok = self.ltok.slice::<u8>();
        let ids = intersect(&lists)
            .take(MAX_LOCALITY_SCAN)
            .filter(|&id| ltok[id as usize] as usize <= words.len())
            // Its kind first: one read, where checking the name normalizes it. Most entities of a
            // common word are streets and POIs, which `collect_localities` drops anyway.
            .filter(|&id| matches!(self.kind_at(id), PlaceKind::Municipality | PlaceKind::Locality | PlaceKind::Neighbourhood))
            .filter(|&id| deferred.is_none_or(|p| self.name_has_prefix(id, p)));
        for m in self.collect_localities(ids) {
            if munis.len() >= MAX_LOCALITIES {
                break;
            }
            if !munis.contains(&m) {
                munis.push(m);
            }
        }
        Within { direct: direct.len(), munis }
    }

    fn localities_once(&self, text: &str, prefix: bool, top: &mut TopK) -> Within {
        if let Some((_, _, w)) = top.localities.iter().find(|(t, p, _)| t == text && *p == prefix) {
            return w.clone();
        }
        let w = self.localities(text, prefix);
        top.localities.push((text.to_owned(), prefix, w.clone()));
        w
    }

    fn collect_localities(&self, ids: impl Iterator<Item = u32>) -> Vec<u32> {
        let mut out = Vec::new();
        for id in ids {
            let m = match self.kind_at(id) {
                PlaceKind::Municipality => id,
                PlaceKind::Locality | PlaceKind::Neighbourhood => self.muni_of(id),
                _ => continue,
            };
            if m != NO_MUNICIPALITY && !out.contains(&m) {
                out.push(m);
                if out.len() >= MAX_LOCALITIES {
                    break;
                }
            }
        }
        out
    }

    fn affinity_for_place(kind: PlaceKind) -> f32 {
        match kind {
            PlaceKind::Region | PlaceKind::Province | PlaceKind::Municipality => 1.0,
            PlaceKind::Locality => 0.92,
            PlaceKind::Neighbourhood => 0.88,
            PlaceKind::Toponym | PlaceKind::Poi => 0.8,
            PlaceKind::Street => 0.7,
            _ => 0.3,
        }
    }

    fn search_place(&self, text: &str, within: Option<&str>, prefix: bool, factor: f32, ctx: &QueryContext, top: &mut TopK) {
        let mut words = self.tokens(text);
        if words.is_empty() {
            return;
        }
        // A name is found without the words that only link others, as streets are: "aeropuerto
        // de barajas" is "Aeropuerto Adolfo Suárez Madrid-Barajas". Not while typing: there the
        // words typed so far narrow which localities fit ("Villanueva de S").
        let linking: Vec<bool> = words.iter().map(|w| !prefix && self.normalizer.is_connector(w)).collect();
        if linking.contains(&false) {
            let mut i = 0;
            words.retain(|_| {
                i += 1;
                !linking[i - 1]
            });
        }
        let munis = within.map(|w| self.localities_once(w, prefix, top));
        if munis.as_ref().is_some_and(|m| m.munis.is_empty()) {
            return;
        }
        let mut folded = String::new();
        self.normalizer.normalize(text, &mut folded);
        let (mut found, named_here) =
            if top.translating { (0, 0) } else { self.search_words(&words, words.len(), &folded, munis.as_ref(), prefix, factor, ctx, top) };
        let missing_here = !prefix && munis.as_ref().is_some_and(|m| m.direct > 0) && named_here == 0;
        if top.translating || missing_here {
            for translated in equivalences::translations(&words, &self.equivalents) {
                if out_of_time(ctx) {
                    break;
                }
                let words: Vec<String> = translated.split(' ').map(str::to_owned).collect();
                found += self.search_words(&words, words.len(), &translated, munis.as_ref(), prefix, factor, ctx, top).0;
            }
        }
        // Relaxation: a long query with no full match is retried without each word in turn
        // ("basilica sagrada familia" when the name is "Sagrada Família"), scored on coverage.
        // Relaxation rescues a query that found nothing; it is not run to fill a top that already
        // has answers from a likelier reading (PROJECT.md §9.5).
        if found == 0 && words.len() >= 3 && !top.matched() && !top.translating {
            top.relaxing = true;
            for skip in 0..words.len() {
                if out_of_time(ctx) {
                    break;
                }
                let fewer: Vec<String> = words.iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, w)| w.clone()).collect();
                let last_typed = prefix && skip != words.len() - 1;
                self.search_words(&fewer, words.len(), &fewer.join(" "), munis.as_ref(), last_typed, factor, ctx, top);
            }
            top.relaxing = false;
        }
    }

    /// Scores every entity containing all `words`; `query_words` is the length of the full
    /// query, so dropped words lower the coverage. Returns how many entities matched, and how
    /// many of them have exactly this name in a municipality named exactly as the locality (all,
    /// when a word was only found with a typo: a misspelling, not another language).
    #[allow(clippy::too_many_arguments)]
    fn search_words(
        &self,
        words: &[String],
        query_words: usize,
        folded_name: &str,
        munis: Option<&Within>,
        prefix: bool,
        factor: f32,
        ctx: &QueryContext,
        top: &mut TopK,
    ) -> (usize, usize) {
        let (lists, fuzzy) = self.word_lists(words, prefix);
        let short = self.deferred_prefix(words, prefix);
        // Against the whole query: a relaxed search without the discriminating word ("calle de"
        // from "Calle de Francsic") is left with only the commonest words too.
        if lists.is_empty() || self.only_common_left(&lists, short.is_some(), query_words) {
            return (0, 0);
        }
        let exact_names = self.by_name(folded_name);
        let starting = if prefix { self.names_starting(folded_name) } else { Vec::new() };
        let matched = lists.len() + usize::from(short.is_some());
        let mut name_checks = 0;
        let (mut found, mut named_here) = (0, 0);
        // Postcodes have their own reading once complete; a lone number being typed is one too.
        let typing_postcode = prefix && words.len() == 1 && words[0].chars().all(|c| c.is_ascii_digit());
        for (i, id) in intersect(&lists).enumerate() {
            if (top.full() && self.bound(id, factor, ctx) < top.min()) || scan_past(top.deadline, i) {
                break;
            }
            let kind = self.kind_at(id);
            if kind == PlaceKind::Postcode && !typing_postcode {
                continue;
            }
            if let Some(m) = munis
                && !m.contains(self.muni_of(id))
                && !m.contains(id)
            {
                continue;
            }
            let p = self.point(id);
            if !self.in_area(p, ctx) {
                continue;
            }
            if let Some(typed) = short {
                name_checks += 1;
                if name_checks > MAX_NAME_CHECKS {
                    break;
                }
                if !self.name_has_prefix(id, typed) {
                    continue;
                }
            }
            found += 1;
            let muni = if kind == PlaceKind::Municipality { id } else { self.muni_of(id) };
            let exact = exact_names.binary_search(&id).is_ok();
            named_here += usize::from(exact && munis.is_some_and(|m| m.is_direct(muni)));
            let mut text = if prefix {
                Self::typed_text_score(matched, query_words, exact, starting.binary_search(&id).is_ok(), fuzzy)
            } else {
                self.text_score(id, matched, query_words, exact, fuzzy)
            };
            if let Some(m) = munis {
                text *= m.factor(muni);
            }
            let affinity = if kind == PlaceKind::Poi && !prefix {
                // A famous place answers a bare name as well as a municipality does. While typing,
                // a municipality is still the likelier start ("Cons" is Consdorf).
                let fame = self.fame(id);
                Self::affinity_for_place(kind) + (1.0 - Self::affinity_for_place(kind)) * fame
            } else {
                Self::affinity_for_place(kind)
            };
            let score = self.score(id, p, Scored { text, affinity }, factor, ctx);
            let hit = Hit { country: self.country, id: LocalId(id), score, match_type: MatchType::Exact };
            top.muni = muni;
            top.locating = munis.is_some_and(|m| m.is_direct(muni));
            // Found through another of its names only: like a relaxed hit, it goes below a whole
            // match in the locality another reading read in the query. "carrefour express
            // barcelona" is not the shop in Madrid that a source calls "Carrefour Express Ciudad
            // de Barcelona, 142", after its street.
            if munis.is_none() && !fuzzy && words.len() > 1 && (!top.full() || score > top.min()) && !self.name_covers(id, words, prefix) {
                let relaxing = std::mem::replace(&mut top.relaxing, true);
                top.push(hit);
                top.relaxing = relaxing;
            } else {
                top.push(hit);
            }
        }
        top.locating = false;
        top.muni = NO_MUNICIPALITY;
        (found, if fuzzy { found } else { named_here })
    }

    fn block(&self, street: u32) -> std::ops::Range<usize> {
        let start = self.addr_start.slice::<u32>()[street as usize] as usize;
        start..start + self.addr_len.slice::<u32>()[street as usize] as usize
    }

    fn suffix_of(&self, a: usize) -> Option<&str> {
        match self.a_suffix.slice::<u16>()[a] {
            0 => None,
            s => self.manifest.suffixes.get(s as usize - 1).map(String::as_str),
        }
    }

    /// A postcode as stored in `a_postcode.bin`: 1 + its index in the manifest.
    fn postcode_code(&self, postcode: &str) -> Option<u16> {
        self.manifest.postcodes.iter().position(|p| p == postcode).map(|i| i as u16 + 1)
    }

    fn block_has_postcode(&self, block: std::ops::Range<usize>, code: u16) -> bool {
        self.a_postcode.slice::<u16>()[block].contains(&code)
    }

    /// The address of `number` in a street block, or the nearest number on the same side. A
    /// number can repeat in a street (two numberings, or two postcodes): the query's postcode,
    /// as `postcode` (1 + index, as in `a_postcode.bin`), breaks the tie.
    fn find_number(&self, block: std::ops::Range<usize>, number: u32, suffix: Option<&str>, postcode: Option<u16>) -> Option<(usize, MatchType)> {
        let numbers = &self.a_number.slice::<u32>()[block.clone()];
        let numbered = numbers.partition_point(|&n| n != 0);
        let numbers = &numbers[..numbered];
        let lo = numbers.partition_point(|&n| n < number);
        let hi = numbers.partition_point(|&n| n <= number);
        if lo < hi {
            let codes = self.a_postcode.slice::<u16>();
            let same_pc = |i: usize| postcode.is_some_and(|p| codes[block.start + i] == p);
            let pick = (lo..hi)
                .max_by_key(|&i| {
                    let s = self.suffix_of(block.start + i);
                    (s == suffix, s.is_none(), same_pc(i), std::cmp::Reverse(i))
                })
                .unwrap_or(lo);
            return Some((block.start + pick, MatchType::Exact));
        }
        numbers
            .iter()
            .enumerate()
            .filter(|(_, n)| *n % 2 == number % 2 && n.abs_diff(number) <= 10)
            .min_by_key(|(_, n)| n.abs_diff(number))
            .map(|(i, _)| (block.start + i, MatchType::Approximate))
    }

    fn search_address(&self, c: &AddressComponents, prefix: bool, factor: f32, ctx: &QueryContext, top: &mut TopK) {
        let Some(street) = c.street.as_deref() else {
            if let Some(locality) = c.locality.as_deref() {
                self.search_place(locality, None, prefix, factor, ctx, top);
            }
            return;
        };
        let parts = self.normalizer.query_street_parts(street);
        let mut words = Vec::new();
        self.normalizer.tokenize(&parts.core, &mut words);
        limit_words(&mut words);
        if words.is_empty() {
            return;
        }
        let munis = c.locality.as_deref().map(|l| self.localities_once(l, prefix, top)).filter(|m| !m.munis.is_empty());
        let exact_names = self.by_name(&parts.core);
        let prefix_words = prefix && c.housenumber.is_none();
        let (lists, fuzzy) = self.word_lists(&words, prefix_words);
        let short = self.deferred_prefix(&words, prefix_words);
        let mut streets = if top.translating {
            Vec::new()
        } else {
            self.streets_for(&words, &lists, fuzzy, short, &parts, c, munis.as_ref(), exact_names, factor, ctx, top.deadline)
        };
        // A complete locality names a municipality with no street of this exact name: the name
        // may be written there in another language of the country ("plaza mayor, palma" for
        // "Plaça Major"). While typing, a name is seldom exact yet, and a word only found with a
        // typo is a misspelling, not another language.
        let missing_here = !prefix
            && !fuzzy
            && munis.as_ref().is_some_and(|m| {
                m.direct > 0 && !streets.iter().any(|&(_, id)| m.is_direct(self.muni_of(id)) && exact_names.binary_search(&id).is_ok())
            });
        if top.translating || missing_here {
            for translated in equivalences::translations(&words, &self.equivalents) {
                if out_of_time(ctx) {
                    break;
                }
                let words: Vec<String> = translated.split(' ').map(str::to_owned).collect();
                let (lists, fuzzy) = self.word_lists(&words, prefix_words);
                let short = self.deferred_prefix(&words, prefix_words);
                let names = self.by_name(&translated);
                streets.extend(self.streets_for(&words, &lists, fuzzy, short, &parts, c, munis.as_ref(), names, factor, ctx, top.deadline));
            }
        }
        if streets.is_empty() && !fuzzy && !top.matched() && !top.translating && !out_of_time(ctx) {
            let (lists, fuzzy) = self.word_lists_with(&words, prefix_words, true);
            streets = self.streets_for(&words, &lists, fuzzy, short, &parts, c, munis.as_ref(), exact_names, factor, ctx, top.deadline);
        }
        streets.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut seen = Vec::new();
        streets.retain(|&(_, id)| {
            let new = !seen.contains(&id);
            seen.push(id);
            new
        });
        streets.truncate(MAX_STREET_CANDIDATES);
        self.answer_address(streets, munis.as_ref(), c, factor, ctx, top);
    }

    /// Streets matching the name words, scored, within the locality if one was resolved.
    #[allow(clippy::too_many_arguments)]
    fn streets_for(
        &self,
        words: &[String],
        lists: &[Cow<'_, [u32]>],
        fuzzy: bool,
        short: Option<&str>,
        parts: &geko_core::model::StreetParts,
        c: &AddressComponents,
        munis: Option<&Within>,
        exact_names: &[u32],
        factor: f32,
        ctx: &QueryContext,
        deadline: Option<std::time::Instant>,
    ) -> Vec<(f32, u32)> {
        let mut streets: Vec<(f32, u32)> = Vec::new();
        if lists.is_empty() || self.only_common_left(lists, short.is_some(), words.len()) {
            return streets;
        }
        let mut scanned = 0;
        let mut near_focus = 0;
        let mut name_checks = 0;
        // Past the most important streets, the scan goes on near the focus, or near the postcode
        // of the query: of the thousands of "Calle de la Cruz", the one of 05140 is seldom among
        // the most important of the country.
        let focus = ctx
            .focus
            .filter(|_| munis.is_none())
            .map(|f| (f.lon_e7(), f.lat_e7()))
            .or_else(|| c.postcode.as_deref().filter(|_| munis.is_none()).and_then(|pc| self.postcode_point(pc)));
        // Looked up once: the manifest lists every postcode of the country.
        let postcode = c.postcode.as_deref().and_then(|pc| self.postcode_code(pc));
        // A street that has the number asked for is a likelier reading than a homonym that has
        // not: a road crosses a hundred municipalities, each with its own stretch and kilometres,
        // and only one has kilometre 1050.
        let number = c.housenumber.as_deref().and_then(|n| self.normalizer.parse_housenumber(n));
        let has_number = |id: u32| {
            number.as_ref().is_some_and(|n| {
                matches!(self.find_number(self.block(id), n.number, n.suffix.as_deref(), postcode), Some((_, MatchType::Exact)))
            })
        };
        let mut with_number = 0;
        for (i, id) in intersect(lists).filter(|&id| self.kind_at(id) == PlaceKind::Street).take(MAX_STREETS_VISITED).enumerate() {
            if scan_past(deadline, i) {
                break;
            }
            if let Some(m) = munis
                && !m.contains(self.muni_of(id))
            {
                continue;
            }
            let p = self.point(id);
            if !self.in_area(p, ctx) {
                continue;
            }
            if let Some(typed) = short {
                name_checks += 1;
                if name_checks > MAX_NAME_CHECKS {
                    break;
                }
                if !self.name_has_prefix(id, typed) {
                    continue;
                }
            }
            // Only streets that pass the filters count: with Spain loaded, the 400 first "Calle
            // Real" never reached the one of Calzada del Coto.
            scanned += 1;
            let numbered = has_number(id);
            if scanned > MAX_STREETS_SCANNED {
                // Past the most important ones, only streets near the focus, if there is one, or
                // else those that have the number asked for.
                if let Some(f) = focus {
                    if geko_geom::distance_m(p, f) > FOCUS_NEAR_M {
                        continue;
                    }
                    near_focus += 1;
                    if near_focus > MAX_STREETS_SCANNED {
                        break;
                    }
                } else if number.is_none() {
                    break;
                } else if !numbered {
                    continue;
                } else {
                    with_number += 1;
                    if with_number > MAX_STREET_CANDIDATES {
                        break;
                    }
                }
            }
            // "parc guell" is not "Passatge de Güell": a known, incompatible street type
            // forfeits the exact-name bonus and weighs less.
            let doc_type = self.street_type(id);
            let same_type = self.normalizer.street_types_compatible(parts.type_code, doc_type);
            let exact = same_type && exact_names.binary_search(&id).is_ok();
            let mut text = self.text_score(id, lists.len() + usize::from(short.is_some()), words.len(), exact, fuzzy);
            if !same_type {
                text *= 0.6;
            } else if parts.type_code.is_some() && doc_type.is_some() && parts.type_code != doc_type {
                // Compatible but different ("rambla" asked, "carrer" found): a slight penalty.
                text *= 0.9;
            }
            if c.locality.is_some() && munis.is_none() {
                text *= 0.7;
            }
            if let Some(m) = munis {
                text *= m.factor(self.muni_of(id));
            }
            if let Some(pc) = postcode
                && self.block_has_postcode(self.block(id), pc)
            {
                text += 0.2;
            }
            if numbered {
                text += 0.1;
            }
            streets.push((self.score(id, p, Scored { text, affinity: 1.0 }, factor, ctx), id));
        }
        streets
    }

    /// Resolves the house number in the best streets, or falls back to the locality.
    fn answer_address(
        &self,
        streets: Vec<(f32, u32)>,
        munis: Option<&Within>,
        c: &AddressComponents,
        factor: f32,
        ctx: &QueryContext,
        top: &mut TopK,
    ) {
        // The locality exists but has no such street: answer with the locality itself rather
        // than with a homonymous street elsewhere.
        if streets.is_empty()
            && let Some(&m) = munis.and_then(|m| m.munis.first())
        {
            let p = self.point(m);
            let score = self.score(m, p, Scored { text: 0.9, affinity: 1.0 }, factor, ctx);
            top.push(Hit { country: self.country, id: LocalId(m), score, match_type: MatchType::LocalityOnly });
            return;
        }

        let number = c.housenumber.as_deref().and_then(|n| self.normalizer.parse_housenumber(n));
        let postcode = c.postcode.as_deref().and_then(|pc| self.postcode_code(pc));
        let docs = self.manifest.docs;
        for (street_score, id) in streets {
            top.muni = self.muni_of(id);
            top.locating = munis.is_some_and(|m| m.is_direct(top.muni));
            let found = number.as_ref().and_then(|n| self.find_number(self.block(id), n.number, n.suffix.as_deref(), postcode));
            match (found, &number) {
                (Some((a, match_type)), _) => {
                    let bonus = if match_type == MatchType::Exact { 0.4 } else { 0.15 };
                    let score = street_score + factor * ctx.weights.text * bonus;
                    top.push(Hit { country: self.country, id: LocalId(docs + a as u32), score, match_type });
                }
                (None, Some(_)) => {
                    top.push(Hit { country: self.country, id: LocalId(id), score: street_score - 0.05, match_type: MatchType::StreetOnly });
                }
                (None, None) => top.push(Hit { country: self.country, id: LocalId(id), score: street_score, match_type: MatchType::Exact }),
            }
        }
        top.locating = false;
        top.muni = NO_MUNICIPALITY;
    }

    /// Where a postcode is: the centre of its addresses.
    fn postcode_point(&self, postcode: &str) -> Option<(i32, i32)> {
        self.by_name(postcode).iter().find(|&&id| self.kind_at(id) == PlaceKind::Postcode).map(|&id| self.point(id))
    }

    fn search_postcode(&self, postcode: &str, factor: f32, ctx: &QueryContext, top: &mut TopK) {
        for &id in self.by_name(postcode) {
            if self.kind_at(id) == PlaceKind::Postcode {
                let p = self.point(id);
                let score = self.score(id, p, Scored { text: 1.0, affinity: 1.0 }, factor, ctx);
                top.push(Hit { country: self.country, id: LocalId(id), score, match_type: MatchType::Exact });
            }
        }
    }

    fn run(&self, q: &ParsedQuery, ctx: &QueryContext, prefix: bool, out: &mut Vec<Hit>) -> Result<()> {
        out.clear();
        if !ctx.countries.is_empty() && !ctx.countries.contains(&self.country) {
            return Ok(());
        }
        let mut top = TopK::new(ctx.size);
        self.readings(q, ctx, prefix, &mut top);
        if top.hits.is_empty() && !self.equivalents.is_empty() && !out_of_time(ctx) {
            top.translating = true;
            self.readings(q, ctx, prefix, &mut top);
        }
        let whole: Vec<bool> = top.hits.iter().map(|h| top.answers_whole(h)).collect();
        let located = top.located_munis();
        let partial: Vec<LocalId> = top.hits.iter().filter(|h| top.partial_elsewhere(h, &located)).map(|h| h.id).collect();
        let mut hits = top.hits;
        // The locality alone is the answer of a reading that found no such street there; another
        // reading that found the whole query is the likelier one ("…bacarrisas 78, pila": the
        // hamlet named "Pila" against the address in Pilas). So is a name found without one of
        // the words, against the whole query in another reading that read that word as the
        // locality: "manolo bakes barcelona" is not every Manolo Bakes but the one in Barcelona.
        if let Some(best) = hits.iter().filter(|h| full_match(h)).map(|h| h.score).reduce(f32::max) {
            for h in hits.iter_mut().filter(|h| h.match_type == MatchType::LocalityOnly && h.score >= best) {
                h.score = best * 0.99;
            }
        }
        if let Some(best) = hits.iter().zip(&whole).filter(|(_, w)| **w).map(|(h, _)| h.score).reduce(f32::max) {
            for h in hits.iter_mut().filter(|h| partial.contains(&h.id) && h.score >= best) {
                h.score = best * 0.99;
            }
        }
        hits.retain(|h| h.score >= ctx.min_score);
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        for h in hits {
            if !out.iter().any(|kept: &Hit| self.same_entity(kept.id.0, h.id.0)) {
                out.push(h);
            }
        }
        Ok(())
    }

    /// Searches the readings of the query, likeliest first.
    fn readings(&self, q: &ParsedQuery, ctx: &QueryContext, prefix: bool, top: &mut TopK) {
        for (n, interpretation) in q.interpretations.iter().enumerate() {
            if interpretation.country != self.country {
                continue;
            }
            // The likeliest reading always completes; the others only within the time budget.
            if n > 0 && out_of_time(ctx) {
                break;
            }
            let factor = 0.5 + 0.5 * interpretation.probability;
            top.deadline = if n == 0 { None } else { ctx.deadline };
            if top.full() && self.bound(0, factor, ctx) + 0.5 < top.min() {
                break;
            }
            let started = ctx.debug.then(std::time::Instant::now);
            match &interpretation.kind {
                InterpretationKind::Place { text, within } => self.search_place(text, within.as_deref(), prefix, factor, ctx, top),
                InterpretationKind::Address(c) => self.search_address(c, prefix, factor, ctx, top),
                InterpretationKind::Postcode(pc) => self.search_postcode(pc, factor, ctx, top),
                InterpretationKind::Coordinates(_) => {}
            }
            if let Some(started) = started {
                eprintln!("{:>8} us  p={:.2}  {:?}", started.elapsed().as_micros(), interpretation.probability, interpretation.kind);
            }
        }
    }

    /// Safety net (PROJECT.md §6.5.6): two results with the same kind, name and municipality
    /// a few metres apart are one entity that fusion failed to merge.
    fn same_entity(&self, a: u32, b: u32) -> bool {
        let docs = self.manifest.docs;
        if a >= docs || b >= docs || self.kind_at(a) != self.kind_at(b) || self.muni_of(a) != self.muni_of(b) {
            return false;
        }
        self.name.get(a as usize) == self.name.get(b as usize)
            && geko_geom::distance_m(self.point(a), self.point(b)) < SAME_ENTITY_M
    }
}

impl GeocodeEngine for FstEngine {
    fn country(&self) -> CountryCode {
        self.country
    }

    fn search(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()> {
        self.run(q, ctx, false, out)
    }

    fn autocomplete(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()> {
        self.run(q, ctx, true, out)
    }

    fn place(&self, id: LocalId) -> Option<PlaceRef<'_>> {
        let i = id.0;
        let docs = self.manifest.docs;
        if i < docs {
            let p = self.point(i);
            return Some(PlaceRef {
                country: self.country,
                id,
                public_id: Cow::Borrowed(self.pid.str(i as usize)),
                kind: self.kind_at(i),
                name: Cow::Borrowed(self.name.str(i as usize)),
                point: Coord::from_e7(p.1, p.0).ok()?,
                json: Cow::Borrowed(self.json.get(i as usize)),
            });
        }
        if i >= docs + self.manifest.addresses {
            return None;
        }
        let a = (i - docs) as usize;
        let street = self.a_street.slice::<u32>()[a] as usize;
        let street_id = self.pid.str(street);
        let public_id = format::address_public_id(street_id, self.atail.str(self.a_tail.slice::<u32>()[a] as usize));
        let view = format::AddressView {
            public_id: &public_id,
            street_id,
            street_name: self.name.str(street),
            label: self.alabel.str(self.a_label.slice::<u32>()[a] as usize),
            postcode: match self.a_postcode.slice::<u16>()[a] {
                0 => None,
                code => self.manifest.postcodes.get(code as usize - 1).map(String::as_str),
            },
            meta: self.manifest.address_meta.get(self.a_meta.slice::<u16>()[a] as usize)?,
        };
        let (name, json) = (view.name(), view.fragment());
        Some(PlaceRef {
            country: self.country,
            id,
            kind: PlaceKind::Address,
            point: Coord::from_e7(self.a_lat.slice::<i32>()[a], self.a_lon.slice::<i32>()[a]).ok()?,
            public_id: Cow::Owned(public_id),
            name: Cow::Owned(name),
            json: Cow::Owned(json),
        })
    }

    fn resolve(&self, id: &PublicId) -> Option<LocalId> {
        if let Some(v) = self.ids.get(id.as_str()) {
            return Some(LocalId(v as u32));
        }
        // Address ids are not in `ids.fst`: they are their street's id plus a tail.
        let (street_id, tail) = format::split_address_id(id.as_str())?;
        let street = self.ids.get(&street_id)? as u32;
        let tails = self.a_tail.slice::<u32>();
        self.block(street).find(|&a| self.atail.str(tails[a] as usize) == tail).map(|a| LocalId(self.manifest.docs + a as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_words_count_once() {
        let words = |text: &str| {
            let mut w: Vec<String> = text.split(' ').map(str::to_owned).collect();
            limit_words(&mut w);
            w
        };
        assert_eq!(words("calle calle calle calle"), ["calle"]);
        assert_eq!(words("sant joan sant llorenc"), ["sant", "joan", "sant", "llorenc"], "only consecutive repeats");
        assert_eq!(words(&"a b ".repeat(10)).len(), MAX_QUERY_WORDS);
    }

    #[test]
    fn intersection_of_sorted_lists() {
        let lists: Vec<Cow<[u32]>> = vec![Cow::Owned(vec![1, 3, 5, 7, 9]), Cow::Owned(vec![3, 4, 5, 9]), Cow::Owned(vec![0, 3, 9, 11])];
        assert_eq!(intersect(&lists).collect::<Vec<_>>(), [3, 9]);
        let single: Vec<Cow<[u32]>> = vec![Cow::Owned(vec![2, 4])];
        assert_eq!(intersect(&single).collect::<Vec<_>>(), [2, 4]);
    }

    #[test]
    fn top_k_keeps_best_and_dedups() {
        let es = CountryCode::new("ES").unwrap();
        let hit = |id, score| Hit { country: es, id: LocalId(id), score, match_type: MatchType::Exact };
        let mut top = TopK::new(2);
        top.push(hit(1, 0.5));
        top.push(hit(2, 0.9));
        top.push(hit(1, 0.7));
        assert_eq!(top.min(), 0.7);
        top.push(hit(3, 0.6));
        assert_eq!(top.min(), 0.7);
        top.push(hit(4, 0.8));
        let mut ids: Vec<u32> = top.hits.iter().map(|h| h.id.0).collect();
        ids.sort();
        assert_eq!(ids, [2, 4]);
    }
}
