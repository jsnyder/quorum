use super::identifiers::{
    harvest_identifiers, load_stoplist, ReviewedFile, Symbol,
};

fn sym(names: &[&str]) -> Vec<Symbol> {
    names.iter().map(|n| Symbol::new(*n)).collect()
}

#[test]
fn returns_refs_when_specific() {
    let refs = sym(&["verify_token", "sign_jwt"]);
    let file = ReviewedFile::new("src/auth.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert_eq!(
        ids,
        vec!["verify_token".to_string(), "sign_jwt".to_string()]
    );
}

#[test]
fn returns_single_ref_augmented_because_too_few() {
    let refs = sym(&["verify_token"]);
    let file = ReviewedFile::new("src/auth/jwt.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert!(ids.contains(&"verify_token".to_string()));
    assert!(ids.contains(&"auth".to_string()));
    assert!(ids.contains(&"jwt".to_string()));
}

#[test]
fn augments_when_all_refs_are_generic() {
    let refs = sym(&["Client", "Handler"]);
    let file = ReviewedFile::new("src/services/payment/processor.rs", "rust")
        .with_neighbors(["PaymentProcessor", "process_charge"]);
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert!(ids.contains(&"payment".to_string()));
    assert!(ids.contains(&"processor".to_string()));
    assert!(ids.contains(&"PaymentProcessor".to_string()));
    assert!(ids.contains(&"process_charge".to_string()));
}

#[test]
fn augments_when_refs_empty() {
    let ids = harvest_identifiers(
        &[],
        &ReviewedFile::new("src/foo/bar.rs", "rust"),
        &load_stoplist("rust"),
    );
    assert!(!ids.is_empty());
    assert!(ids.contains(&"foo".to_string()));
    assert!(ids.contains(&"bar".to_string()));
}

#[test]
fn path_noise_is_filtered() {
    let ids = harvest_identifiers(
        &[],
        &ReviewedFile::new("src/lib.rs", "rust"),
        &load_stoplist("rust"),
    );
    assert!(ids.is_empty() || ids.iter().all(|i| i != "src" && i != "lib"));
}

#[test]
fn camel_case_path_is_split() {
    let ids = harvest_identifiers(
        &[],
        &ReviewedFile::new("src/PaymentProcessor.rs", "rust"),
        &load_stoplist("rust"),
    );
    assert!(ids.contains(&"Payment".to_string()));
    assert!(ids.contains(&"Processor".to_string()));
}

#[test]
fn snake_case_path_is_split() {
    let ids = harvest_identifiers(
        &[],
        &ReviewedFile::new("src/verify_token.rs", "rust"),
        &load_stoplist("rust"),
    );
    assert!(ids.contains(&"verify".to_string()));
    assert!(ids.contains(&"token".to_string()));
}

#[test]
fn duplicate_refs_are_deduped_preserving_order() {
    let refs = sym(&["foo", "bar", "foo", "baz", "bar"]);
    let file = ReviewedFile::new("x.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert_eq!(
        ids,
        vec!["foo".to_string(), "bar".to_string(), "baz".to_string()]
    );
}

#[test]
fn generic_refs_mix_with_specific_keeps_refs_only() {
    let refs = sym(&["Client", "process_payment"]);
    let file = ReviewedFile::new("src/foo.rs", "rust")
        .with_neighbors(["shouldNotAppear"]);
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert_eq!(
        ids,
        vec!["Client".to_string(), "process_payment".to_string()]
    );
}

#[test]
fn stoplist_is_language_scoped() {
    let rust_stoplist = load_stoplist("rust");
    let tf_stoplist = load_stoplist("terraform");
    assert!(!rust_stoplist.is_generic("Module"));
    assert!(tf_stoplist.is_generic("Module"));
}

#[test]
fn output_is_capped() {
    let neighbors: Vec<String> = (0..64).map(|i| format!("Symbol{i}")).collect();
    let file = ReviewedFile::new("x.rs", "rust").with_neighbors(neighbors);
    let ids = harvest_identifiers(&[], &file, &load_stoplist("rust"));
    assert!(ids.len() <= 32);
}

#[test]
fn empty_or_whitespace_refs_filtered() {
    let refs = sym(&["", "   ", "foo", "bar"]);
    let file = ReviewedFile::new("x.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert_eq!(ids, vec!["foo".to_string(), "bar".to_string()]);
}

#[test]
fn stoplist_is_case_insensitive() {
    let stoplist = load_stoplist("rust");
    assert!(stoplist.is_generic("client"));
    assert!(stoplist.is_generic("CLIENT"));
    assert!(stoplist.is_generic("Client"));
}

#[test]
fn whitespace_ref_is_treated_as_empty() {
    // "  foo  ", " foo", "foo" all collapse to "foo"; count becomes 1 so
    // path segments are appended. Expect exactly one "foo" in the output.
    let refs = sym(&["  foo  ", " foo", "foo"]);
    let file = ReviewedFile::new("src/bar.rs", "rust");
    let ids = harvest_identifiers(&refs, &file, &load_stoplist("rust"));
    assert_eq!(ids.iter().filter(|s| s.as_str() == "foo").count(), 1);
    assert!(ids.contains(&"bar".to_string()));
}
