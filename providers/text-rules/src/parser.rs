//! Rule-based query parser (PROJECT.md §9.2): turns a query into scored interpretations, with the
//! words and formats of a country pack. The engine checks each one against the index, so
//! ambiguity is resolved there; the parser only proposes plausible readings, most likely first.

use geko_core::Result;
use geko_core::country::CountryCode;
use geko_core::ports::{AddressParser, TextNormalizer};
use geko_core::query::{AddressComponents, Interpretation, InterpretationKind, ParsedQuery};

use crate::coords;
use crate::normalizer::{RulesNormalizer, fold};

pub const MAX_INTERPRETATIONS: usize = 8;

/// The query parser of a country, from its pack.
#[derive(Debug, Clone, Copy)]
pub struct RulesParser {
    normalizer: RulesNormalizer,
}

impl RulesParser {
    pub fn new(normalizer: RulesNormalizer) -> Self {
        Self { normalizer }
    }

    /// The postcode a token is, without the country prefix it may be written with ("L-1930").
    fn postcode_of(&self, t: &Token) -> Option<String> {
        let format = &self.normalizer.pack().postcode;
        let upper = t.raw.to_ascii_uppercase();
        let code = format.country_prefixes.iter().find_map(|p| upper.strip_prefix(p.as_str())).unwrap_or(&t.raw);
        (code.len() == format.digits
            && code.bytes().all(|b| b.is_ascii_digit())
            && (format.prefixes.is_empty() || format.prefixes.iter().any(|p| code.starts_with(p.as_str()))))
        .then(|| code.to_owned())
    }

    fn is_unnumbered(&self, t: &Token) -> bool {
        let lower = t.raw.to_ascii_lowercase();
        self.normalizer.pack().words.unnumbered.contains(&lower)
    }

    fn is_number_marker(&self, t: &Token) -> bool {
        self.normalizer.pack().words.number_markers.contains(&t.folded)
    }

    /// Floor or door marks like "2º", "1ª", "3o" and unit words.
    fn is_unit_detail(&self, t: &Token) -> bool {
        self.normalizer.pack().words.unit_words.contains(&t.folded)
            || (t.raw.chars().any(|c| matches!(c, 'º' | 'ª')) && t.raw.chars().next().is_some_and(|c| c.is_ascii_digit()))
            || t.raw.len() <= 2 && t.raw.chars().all(|c| c.is_ascii_digit())
    }

    fn locality_of(&self, tokens: &[Token]) -> Option<String> {
        let kept: Vec<Token> = tokens.iter().filter(|t| !self.is_unit_detail(t)).cloned().collect();
        let last_segment = kept.last()?.segment;
        let first_of_last = kept.iter().position(|t| t.segment == last_segment).unwrap_or(0);
        // After a comma, the last segment is the locality; without commas, everything left is.
        let locality = if kept.iter().any(|t| t.segment != last_segment) { &kept[first_of_last..] } else { &kept[..] };
        let text = join(locality);
        (!text.is_empty()).then_some(text)
    }
}

#[derive(Debug, Clone)]
struct Token {
    raw: String,
    folded: String,
    segment: usize,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut folded = String::new();
    for (segment, part) in text.split([',', ';']).enumerate() {
        for word in part.split_whitespace() {
            let raw = word.trim_matches(|c: char| matches!(c, '.' | ':' | '(' | ')' | '"' | '\''));
            if raw.is_empty() {
                continue;
            }
            fold(raw, &mut folded);
            out.push(Token { raw: raw.to_owned(), folded: folded.clone(), segment });
        }
    }
    out
}

fn join(tokens: &[Token]) -> String {
    tokens.iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" ")
}

/// "401", "12B", "12bis", "187-189", "12-b". Not a postcode-like 5-digit number.
fn is_house_number(t: &Token) -> bool {
    let s = t.raw.to_ascii_lowercase();
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 4 {
        return false;
    }
    let rest = s[digits..].trim_start_matches('-');
    rest.is_empty()
        || (rest.len() <= 4 && rest.chars().all(|c| c.is_ascii_digit()))
        || (rest.len() <= 3 && rest.chars().all(|c| c.is_ascii_alphabetic()))
}

struct Builder {
    country: CountryCode,
    out: Vec<Interpretation>,
}

impl Builder {
    fn push(&mut self, probability: f32, kind: InterpretationKind) {
        if probability > 0.0 && !self.out.iter().any(|i| i.kind == kind) {
            self.out.push(Interpretation { probability, country: self.country, kind });
        }
    }

    fn address(&mut self, p: f32, street: Option<String>, housenumber: Option<String>, postcode: &Option<String>, locality: Option<String>) {
        self.push(p, InterpretationKind::Address(AddressComponents { street, housenumber, postcode: postcode.clone(), locality }));
    }

    fn finish(mut self, raw: &str) -> ParsedQuery {
        let total: f32 = self.out.iter().map(|i| i.probability).sum();
        for i in &mut self.out {
            i.probability /= total;
        }
        self.out.sort_by(|a, b| b.probability.total_cmp(&a.probability));
        self.out.truncate(MAX_INTERPRETATIONS);
        ParsedQuery { raw: raw.to_owned(), interpretations: self.out }
    }
}

impl AddressParser for RulesParser {
    fn country(&self) -> CountryCode {
        self.normalizer.country()
    }

    fn parse(&self, text: &str) -> Result<ParsedQuery> {
        let raw = text.trim();
        let mut b = Builder { country: self.country(), out: Vec::new() };
        if raw.is_empty() {
            return Ok(b.finish(raw));
        }
        if let Some(c) = coords::parse(raw) {
            b.push(1.0, InterpretationKind::Coordinates(c));
            return Ok(b.finish(raw));
        }

        let n = &self.normalizer;
        let mut tokens = tokenize(raw);
        let postcode = tokens.iter().position(|t| self.postcode_of(t).is_some()).and_then(|i| self.postcode_of(&tokens.remove(i)));
        tokens.retain(|t| !self.is_number_marker(t) || t.raw.chars().any(|c| c.is_ascii_digit()));
        if tokens.is_empty() {
            if let Some(pc) = &postcode {
                b.push(1.0, InterpretationKind::Postcode(pc.clone()));
            }
            return Ok(b.finish(raw));
        }

        let typed = n.query_starts_with_way_type(&join(&tokens));
        // The house number is the last number that has words before it and is not part of a
        // name like "Calle 13 de Septiembre". A number followed by a connector starts a name only
        // when nothing but a street type precedes it: after a name, the connector begins the
        // locality ("calle pintor laureano martinez 32 la Romana", "rue X 4 le Mans").
        let number_at = (1..tokens.len()).rev().find(|&i| {
            let starts_name = || n.is_street_type_alone(&join(&tokens[..i]));
            (is_house_number(&tokens[i]) || self.is_unnumbered(&tokens[i]))
                && !(tokens.get(i + 1).is_some_and(|t| t.segment == tokens[i].segment && n.is_connector(&t.folded)) && starts_name())
        });
        let leading_number = number_at.is_none() && tokens.len() > 1 && is_house_number(&tokens[0]);

        if let Some(k) = number_at {
            let number = (!self.is_unnumbered(&tokens[k])).then(|| tokens[k].raw.clone());
            let street = join(&tokens[..k]);
            let locality = self.locality_of(&tokens[k + 1..]);
            // Numbers that are part of the street name ("Carrer 106", "Camino 2 Cala Mastella").
            let plain_number = tokens[k].raw.bytes().all(|c| c.is_ascii_digit());
            let segment = tokens[k].segment;
            let tail = tokens[k + 1..].iter().take_while(|t| t.segment == segment && !self.is_unit_detail(t)).count();
            let locality_after_comma = tokens.last().is_some_and(|t| t.segment != segment);
            let only_type = n.is_street_type_alone(&street);
            if plain_number && (only_type || (tail > 0 && locality_after_comma)) {
                // The words between the number and the comma cannot be the locality, and a
                // street type alone is no name: the number is part of the street name.
                let named = join(&tokens[..k + 1 + tail]);
                let rest = self.locality_of(&tokens[k + 1 + tail..]);
                b.address(0.8, Some(named.clone()), None, &postcode, rest.clone());
                if rest.is_some() {
                    b.address(0.1, Some(named), None, &postcode, None);
                }
                if !only_type {
                    b.address(0.15, Some(street), number, &postcode, locality);
                } else if tail > 0 {
                    // "Carrer 24 Femenies": the number may also be the house number of the street
                    // named after it.
                    let around: Vec<Token> = tokens[..k].iter().chain(&tokens[k + 1..k + 1 + tail]).cloned().collect();
                    b.address(0.3, Some(join(&around)), number, &postcode, rest);
                }
                return Ok(b.finish(raw));
            }
            b.address(if typed { 0.9 } else { 0.8 }, Some(street.clone()), number.clone(), &postcode, locality.clone());
            if locality.is_some() {
                b.address(0.1, Some(street.clone()), number, &postcode, None);
            }
            if !typed {
                b.push(0.05, InterpretationKind::Place { text: join(&tokens), within: None });
            }
        } else if leading_number {
            // The number first, as in much of Europe and America: "34 Avenue de la Liberté", or
            // alone before a comma, "12, rue de la Gare", where the street is the next part.
            let street_segment = tokens[1].segment;
            let end = tokens.iter().position(|t| t.segment > street_segment).unwrap_or(tokens.len());
            let locality = self.locality_of(&tokens[end..]);
            b.address(0.8, Some(join(&tokens[1..end])), Some(tokens[0].raw.clone()), &postcode, locality);
            b.push(0.1, InterpretationKind::Place { text: join(&tokens), within: None });
        } else if tokens.iter().any(|t| t.segment != tokens[0].segment) {
            let first_segment = tokens[0].segment;
            let end = tokens.iter().position(|t| t.segment != first_segment).unwrap_or(tokens.len());
            let (head, rest) = (join(&tokens[..end]), self.locality_of(&tokens[end..]));
            let (pa, pp) = if typed { (0.7, 0.3) } else { (0.4, 0.6) };
            b.address(pa, Some(head.clone()), None, &postcode, rest.clone());
            b.push(pp, InterpretationKind::Place { text: head, within: rest });
        } else {
            let count = tokens.len();
            let all = join(&tokens);
            if typed {
                b.address(0.5, Some(all.clone()), None, &postcode, None);
                for k in 1..=3.min(count.saturating_sub(2)) {
                    b.address(0.3 / k as f32, Some(join(&tokens[..count - k])), None, &postcode, Some(join(&tokens[count - k..])));
                }
                b.push(0.1, InterpretationKind::Place { text: all, within: None });
            } else {
                b.push(0.5, InterpretationKind::Place { text: all.clone(), within: None });
                b.address(0.2, Some(all), None, &postcode, None);
                for k in 1..=2.min(count.saturating_sub(1)) {
                    let (head, tail) = (join(&tokens[..count - k]), join(&tokens[count - k..]));
                    b.push(0.2 / k as f32, InterpretationKind::Place { text: head.clone(), within: Some(tail.clone()) });
                    b.address(0.1 / k as f32, Some(head), None, &postcode, Some(tail));
                }
            }
            if let Some(pc) = &postcode {
                b.push(0.1, InterpretationKind::Postcode(pc.clone()));
            }
        }
        Ok(b.finish(raw))
    }
}
