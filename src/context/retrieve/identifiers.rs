use std::collections::HashSet;

/// A symbol reference extracted from hydration context (the reviewed file's
/// direct references or neighboring symbols).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
}

impl Symbol {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// The file under review, with enough context to drive fallback harvesting.
#[derive(Debug, Clone)]
pub struct ReviewedFile {
    pub path: String,
    pub language: String,
    pub neighbors: Vec<String>,
}

impl ReviewedFile {
    pub fn new(path: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            language: language.into(),
            neighbors: Vec::new(),
        }
    }

    pub fn with_neighbors<I, S>(mut self, neighbors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.neighbors = neighbors.into_iter().map(|s| s.into()).collect();
        self
    }
}

/// Per-language stoplist of generic names that aren't distinctive enough to
/// anchor retrieval on their own.
#[derive(Debug, Clone, Default)]
pub struct GenericStoplist(HashSet<String>);

impl GenericStoplist {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(names.into_iter().map(|s| s.into()).collect())
    }
    pub fn is_generic(&self, name: &str) -> bool {
        self.0.contains(name)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

const MAX_IDENTIFIERS: usize = 32;

/// Path segments that are structural noise, not meaningful identifiers.
const PATH_NOISE: &[&str] = &[
    "src", "lib", "tests", "test", "mod", "main",
    "rs", "py", "ts", "js", "tsx", "jsx", "mjs", "cjs",
    "yaml", "yml", "sh", "bash", "zsh", "tf", "tfvars",
];

fn is_path_noise(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    PATH_NOISE.iter().any(|n| *n == lower)
}

/// Split a token on underscores, dashes, and camelCase boundaries.
/// Keeps tokens with len >= 3 that aren't path noise.
fn split_token(token: &str, out: &mut Vec<String>) {
    // First split on _ and -
    for piece in token.split(|c: char| c == '_' || c == '-') {
        if piece.is_empty() {
            continue;
        }
        // Then split camelCase
        let mut current = String::new();
        let chars: Vec<char> = piece.chars().collect();
        for (i, ch) in chars.iter().enumerate() {
            if i > 0 && ch.is_ascii_uppercase() {
                let prev = chars[i - 1];
                if prev.is_ascii_lowercase() || prev.is_ascii_digit() {
                    if !current.is_empty() {
                        push_if_valid(&current, out);
                        current.clear();
                    }
                }
            }
            current.push(*ch);
        }
        if !current.is_empty() {
            push_if_valid(&current, out);
        }
    }
}

fn push_if_valid(tok: &str, out: &mut Vec<String>) {
    if tok.len() >= 3 && !is_path_noise(tok) {
        out.push(tok.to_string());
    }
}

/// Extract path segments from a file path, splitting on `/` and `\`, stripping
/// the filename's extension, splitting on `_`/`-`/camelCase, filtering noise.
fn path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = path.split(|c: char| c == '/' || c == '\\').collect();
    let last_idx = parts.len().saturating_sub(1);
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let stripped: &str = if i == last_idx {
            // Strip extension from the filename
            match part.rfind('.') {
                Some(dot) => &part[..dot],
                None => part,
            }
        } else {
            part
        };
        if stripped.is_empty() {
            continue;
        }
        split_token(stripped, &mut out);
    }
    out
}

/// Harvest a prioritized, deduplicated identifier list for retrieval.
pub fn harvest_identifiers(
    refs: &[Symbol],
    reviewed_file: &ReviewedFile,
    stoplist: &GenericStoplist,
) -> Vec<String> {
    // 1. Dedupe refs preserving order; filter empty/whitespace.
    let mut seen: HashSet<String> = HashSet::new();
    let mut ref_names: Vec<String> = Vec::new();
    for s in refs {
        let name = s.name.as_str();
        if name.trim().is_empty() {
            continue;
        }
        if seen.insert(name.to_string()) {
            ref_names.push(name.to_string());
        }
    }

    // 3. Decide augmentation.
    let all_generic = !ref_names.is_empty()
        && ref_names.iter().all(|n| stoplist.is_generic(n));
    let augment = ref_names.is_empty() || ref_names.len() < 2 || all_generic;

    if !augment {
        return cap(ref_names);
    }

    // 4a. Path segments.
    let segs = path_segments(&reviewed_file.path);
    for seg in segs {
        if seen.insert(seg.clone()) {
            ref_names.push(seg);
        }
    }

    // 4b. Neighbors filtered through stoplist.
    for n in &reviewed_file.neighbors {
        if n.trim().is_empty() || stoplist.is_generic(n) {
            continue;
        }
        if seen.insert(n.clone()) {
            ref_names.push(n.clone());
        }
    }

    cap(ref_names)
}

fn cap(mut v: Vec<String>) -> Vec<String> {
    if v.len() > MAX_IDENTIFIERS {
        v.truncate(MAX_IDENTIFIERS);
    }
    v
}

/// Load the bundled stoplist for a language. Unknown languages return empty.
pub fn load_stoplist(language: &str) -> GenericStoplist {
    let names: &[&str] = match language.to_ascii_lowercase().as_str() {
        "rust" => &[
            "Client", "Handler", "Error", "Builder", "Result", "Option",
            "Default", "Config", "Context", "Service", "Manager", "Engine",
            "State", "Request", "Response",
        ],
        "typescript" | "javascript" | "ts" | "js" | "tsx" | "jsx" => &[
            "Client", "Handler", "Error", "Builder", "Service", "Config",
            "Manager", "Component", "Provider", "Context", "Props", "State",
            "Request", "Response",
        ],
        "python" | "py" => &[
            "Client", "Handler", "Error", "Builder", "Manager", "Service",
            "Config", "Base", "Context",
        ],
        "terraform" | "tf" | "hcl" => &[
            "Resource", "Provider", "Module", "Variable", "Output", "Data",
        ],
        _ => &[],
    };
    GenericStoplist::new(names.iter().copied())
}
