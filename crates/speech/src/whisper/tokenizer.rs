//! Whisper's tokenizer from the model's `tokenizer.json` (byte-level BPE: each vocabulary entry
//! is a string of "printable" stand-in characters, one per byte, using the GPT-2 byte↔unicode
//! table; special tokens are listed separately as added tokens).
//!
//! Decoding turns produced tokens into text. Encoding ([`Tokenizer::encode`]) is only needed for
//! the initial prompt (previous-text conditioning): text is split like GPT-2's pre-tokenizer and
//! each piece is merged by the BPE merge ranks.

use std::collections::HashMap;

use crate::SpeechError;

pub struct Tokenizer {
    /// Bytes of each regular token, by id.
    bytes: Vec<Vec<u8>>,
    /// Special (added) tokens by content and by id.
    special: HashMap<String, u32>,
    special_by_id: HashMap<u32, String>,
    /// Regular tokens by their stand-in string (encoding).
    ids: HashMap<String, u32>,
    /// BPE merge ranks (lower merges first).
    ranks: HashMap<(String, String), usize>,
    /// Byte → stand-in character.
    byte_char: Vec<char>,
    pub eot: u32,
    pub sot: u32,
    pub transcribe: u32,
    pub translate: Option<u32>,
    pub no_timestamps: u32,
    pub no_speech: Option<u32>,
    /// `<|startofprev|>`: starts the previous-text prompt.
    pub start_of_prev: Option<u32>,
    /// `<|0.00|>`; every later id is a timestamp in 0.02 s steps.
    pub timestamp_begin: u32,
    /// Language tokens (`"en"` → id), multilingual models only.
    pub languages: Vec<(String, u32)>,
}

/// GPT-2's byte → printable character table: `(byte, character)` for all 256 bytes.
fn byte_unicode() -> Vec<(u8, char)> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.iter().zip(&cs).filter_map(|(&b, &c)| Some((u8::try_from(b).ok()?, char::from_u32(c)?))).collect()
}

/// GPT-2's byte → printable character table, inverted.
fn unicode_to_byte() -> HashMap<char, u8> {
    byte_unicode().into_iter().map(|(b, c)| (c, b)).collect()
}

/// Longest text encoded (characters); the prompt is cut to [`crate::MAX_PROMPT_TOKENS`] anyway.
const MAX_ENCODE_CHARS: usize = 4096;
/// Longest pre-tokenized piece merged (bytes); the rest of a longer piece is dropped.
const MAX_PIECE_BYTES: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Letter,
    Number,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphabetic() {
        Class::Letter
    } else if c.is_numeric() {
        Class::Number
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// GPT-2's pre-tokenizer: contractions (`'s 't 're 've 'm 'll 'd`), ` ?letters`, ` ?digits`,
/// ` ?other symbols`, and runs of whitespace (a run before a word leaves its last space to it).
pub fn pre_tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().take(MAX_ENCODE_CHARS).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let rest = chars.get(i..).unwrap_or_default();
        if rest.first() == Some(&'\'') {
            let contraction = ["s", "t", "re", "ve", "m", "ll", "d"].iter().find(|c| rest.get(1..=c.len()).is_some_and(|x| x.iter().copied().eq(c.chars())));
            if let Some(c) = contraction {
                out.push(format!("'{c}"));
                i += 1 + c.len();
                continue;
            }
        }
        let c = rest.first().copied().unwrap_or(' ');
        if class(c) == Class::Space {
            let run = rest.iter().take_while(|c| class(**c) == Class::Space).count();
            // a space right before a word goes with the word (`\s+(?!\S)`)
            let gives_last = rest.get(run).is_some() && rest.get(run - 1) == Some(&' ');
            let take = if gives_last { run - 1 } else { run };
            if take > 0 {
                out.push(rest.iter().take(take).collect());
                i += take;
                continue;
            }
        }
        // ` ?` + a run of one class
        let (lead, body) = if c == ' ' { (1, rest.get(1..).unwrap_or_default()) } else { (0, rest) };
        let Some(&first) = body.first() else {
            out.push(c.to_string());
            i += 1;
            continue;
        };
        let k = class(first);
        let n = body.iter().take_while(|c| class(**c) == k).count().max(1);
        out.push(rest.iter().take(lead + n).collect());
        i += lead + n;
    }
    out
}

impl Tokenizer {
    /// Token ids of `text` (byte-level BPE with the model's merges). Text the vocabulary can't
    /// cover falls back to single-byte tokens; the result never contains special tokens.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for piece in pre_tokenize(text) {
            let mapped: Vec<String> = piece.bytes().take(MAX_PIECE_BYTES).filter_map(|b| self.byte_char.get(b as usize)).map(|c| c.to_string()).collect();
            for sym in self.bpe(mapped) {
                match self.ids.get(&sym) {
                    Some(&id) => out.push(id),
                    None => out.extend(sym.chars().filter_map(|c| self.ids.get(&c.to_string()).copied())),
                }
            }
        }
        out.retain(|id| !self.special_by_id.contains_key(id));
        out
    }

    /// Merge adjacent symbols by rank until no merge applies.
    fn bpe(&self, mut syms: Vec<String>) -> Vec<String> {
        while syms.len() > 1 {
            let best = syms.windows(2).filter_map(|w| self.ranks.get(&(w[0].clone(), w[1].clone())).copied()).min();
            let Some(rank) = best else { break };
            let mut next = Vec::with_capacity(syms.len());
            let mut i = 0;
            while let Some(a) = syms.get(i) {
                if let Some(b) = syms.get(i + 1)
                    && self.ranks.get(&(a.clone(), b.clone())) == Some(&rank)
                {
                    next.push(format!("{a}{b}"));
                    i += 2;
                } else {
                    next.push(a.clone());
                    i += 1;
                }
            }
            syms = next;
        }
        syms
    }
}

impl Tokenizer {
    pub fn from_json(json: &str) -> Result<Self, SpeechError> {
        let bad = |m: &str| SpeechError::Model(format!("tokenizer.json: {m}"));
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?;
        let vocab = v["model"]["vocab"].as_object().ok_or_else(|| bad("no model.vocab"))?;
        let table = unicode_to_byte();
        let max = vocab.values().filter_map(|x| x.as_u64()).max().unwrap_or(0) as usize;
        if max > 1_000_000 {
            return Err(bad("vocabulary too large"));
        }
        let mut bytes = vec![Vec::new(); max + 1];
        let mut ids = HashMap::new();
        for (tok, id) in vocab {
            let id = id.as_u64().ok_or_else(|| bad("vocab id"))?;
            if let Some(slot) = bytes.get_mut(id as usize) {
                *slot = tok.chars().filter_map(|c| table.get(&c).copied()).collect();
            }
            ids.insert(tok.clone(), id as u32);
        }
        // merges: "a b" strings (older files) or ["a", "b"] pairs
        let mut ranks = HashMap::new();
        for (r, m) in v["model"]["merges"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
            let pair = match m {
                serde_json::Value::String(s) => s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())),
                serde_json::Value::Array(a) => match (a.first().and_then(|x| x.as_str()), a.get(1).and_then(|x| x.as_str())) {
                    (Some(a), Some(b)) => Some((a.to_string(), b.to_string())),
                    _ => None,
                },
                _ => None,
            };
            if let Some(p) = pair {
                ranks.entry(p).or_insert(r);
            }
        }
        let mut byte_char = vec![' '; 256];
        for (b, c) in byte_unicode() {
            if let Some(slot) = byte_char.get_mut(b as usize) {
                *slot = c;
            }
        }
        let mut special = HashMap::new();
        let mut special_by_id = HashMap::new();
        for a in v["added_tokens"].as_array().ok_or_else(|| bad("no added_tokens"))? {
            let (Some(id), Some(c)) = (a["id"].as_u64(), a["content"].as_str()) else { continue };
            special.insert(c.to_string(), id as u32);
            special_by_id.insert(id as u32, c.to_string());
        }
        let get = |k: &str| special.get(k).copied();
        let need = |k: &str| get(k).ok_or_else(|| bad(&format!("no {k}")));
        let sot = need("<|startoftranscript|>")?;
        let translate = get("<|translate|>");
        let mut languages: Vec<(String, u32)> = special
            .iter()
            .filter(|(k, id)| **id > sot && translate.is_none_or(|t| **id < t) && k.starts_with("<|") && k.ends_with("|>"))
            .map(|(k, id)| (k[2..k.len() - 2].to_string(), *id))
            .collect();
        languages.sort_by_key(|x| x.1);
        Ok(Self {
            eot: need("<|endoftext|>")?,
            sot,
            transcribe: need("<|transcribe|>")?,
            translate,
            no_timestamps: need("<|notimestamps|>")?,
            no_speech: get("<|nospeech|>").or_else(|| get("<|nocaptions|>")),
            start_of_prev: get("<|startofprev|>"),
            timestamp_begin: need("<|0.00|>")?,
            languages,
            bytes,
            special,
            special_by_id,
            ids,
            ranks,
            byte_char,
        })
    }

    pub fn multilingual(&self) -> bool {
        !self.languages.is_empty()
    }

    pub fn language_token(&self, code: &str) -> Option<u32> {
        self.languages.iter().find(|(c, _)| c == code).map(|x| x.1)
    }

    pub fn is_timestamp(&self, id: u32) -> bool {
        id >= self.timestamp_begin
    }

    /// Bytes of a regular token (empty for special tokens).
    pub fn token_bytes(&self, id: u32) -> &[u8] {
        if self.special_by_id.contains_key(&id) {
            return &[];
        }
        self.bytes.get(id as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let b: Vec<u8> = ids.iter().flat_map(|&i| self.token_bytes(i).iter().copied()).collect();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Ids of all special tokens that must never be sampled as text (everything special except
    /// end-of-text and the timestamps).
    pub fn non_text_specials(&self) -> Vec<u32> {
        self.special.values().copied().filter(|&id| id != self.eot && id < self.timestamp_begin).collect()
    }
}

/// Group text tokens into words: a token whose text starts with a space starts a new word;
/// punctuation-only tokens join the word before them (opening quotes/brackets the word after).
/// Returns (word text, token index range).
pub fn group_words(tok: &Tokenizer, ids: &[u32]) -> Vec<(String, std::ops::Range<usize>)> {
    let mut groups: Vec<(Vec<u8>, std::ops::Range<usize>)> = Vec::new();
    let mut pending_open: Option<(Vec<u8>, usize)> = None;
    for (i, &id) in ids.iter().enumerate() {
        let b = tok.token_bytes(id);
        if b.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(b);
        let trimmed = text.trim();
        let starts_space = b[0] == b' ';
        let punct = !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_punctuation() || "“”‘’«»¿¡…。，！？：、".contains(c));
        let opening = punct && trimmed.chars().all(|c| "\"'“‘¿¡([{«-".contains(c)) && starts_space;
        if opening {
            let start = pending_open.as_ref().map(|p| p.1).unwrap_or(i);
            let mut acc = pending_open.take().map(|p| p.0).unwrap_or_default();
            acc.extend_from_slice(b);
            pending_open = Some((acc, start));
            continue;
        }
        if let Some((mut pre, start)) = pending_open.take() {
            pre.extend_from_slice(b);
            groups.push((pre, start..i + 1));
            continue;
        }
        match groups.last_mut() {
            Some(g) if !starts_space => {
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            Some(g) if punct => {
                // " ," style punctuation with a space still belongs to the previous word
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            _ => groups.push((b.to_vec(), i..i + 1)),
        }
    }
    if let Some((pre, start)) = pending_open {
        groups.push((pre, start..ids.len()));
    }
    groups.into_iter().map(|(b, r)| (String::from_utf8_lossy(&b).trim().to_string(), r)).filter(|(t, _)| !t.is_empty()).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tiny tokenizer: single-byte tokens for a few characters, merges that build "Umm", " um"
    /// and " so", and the special tokens Whisper needs (ids from 100).
    pub(crate) fn tiny() -> Tokenizer {
        let g = |c: char| if c == ' ' { '\u{120}' } else { c };
        let mut vocab = serde_json::Map::new();
        let mut next = 0u32;
        for c in [' ', 'U', 'u', 'm', ',', 's', 'o', 'h'] {
            vocab.insert(g(c).to_string(), next.into());
            next += 1;
        }
        for t in ["\u{120}u", "\u{120}um", "Um", "Umm", "\u{120}s", "\u{120}so"] {
            vocab.insert(t.to_string(), next.into());
            next += 1;
        }
        let merges = vec!["\u{120} u", "\u{120}u m", "U m", "Um m", "\u{120} s", "\u{120}s o"];
        let specials = ["<|endoftext|>", "<|startoftranscript|>", "<|en|>", "<|transcribe|>", "<|startofprev|>", "<|notimestamps|>", "<|0.00|>"];
        let added: Vec<serde_json::Value> = specials.iter().enumerate().map(|(i, c)| serde_json::json!({"id": 100 + i, "content": c})).collect();
        let json = serde_json::json!({"model": {"vocab": vocab, "merges": merges}, "added_tokens": added});
        Tokenizer::from_json(&json.to_string()).unwrap()
    }

    #[test]
    fn pre_tokenizer_splits_like_gpt2() {
        assert_eq!(pre_tokenize("Umm, so, uh"), ["Umm", ",", " so", ",", " uh"]);
        assert_eq!(pre_tokenize("I'm  ok 42!"), ["I", "'m", " ", " ok", " 42", "!"]);
        assert_eq!(pre_tokenize("here's the thing."), ["here", "'s", " the", " thing", "."]);
        assert_eq!(pre_tokenize("a\u{2026} b"), ["a", "\u{2026}", " b"]);
        assert!(pre_tokenize("").is_empty());
        assert_eq!(pre_tokenize("x  ").concat(), "x  ");
        assert_eq!(pre_tokenize(" ' ").concat(), " ' ");
    }

    #[test]
    fn encode_applies_merges_and_round_trips() {
        let t = tiny();
        let ids = t.encode("Umm, so, um");
        assert_eq!(t.decode(&ids), "Umm, so, um");
        // "Umm" "," " so" "," " um": five tokens thanks to the merges
        assert_eq!(ids.len(), 5, "{ids:?}");
        assert_eq!(t.start_of_prev, Some(104));
        // nothing the vocabulary lacks becomes a special token
        assert!(t.encode("xyz \u{1F600}").iter().all(|&i| i < 100));
        assert!(t.encode(&"um ".repeat(100_000)).len() <= MAX_ENCODE_CHARS);
    }
}
