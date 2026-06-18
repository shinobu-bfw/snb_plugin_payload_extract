use super::*;

#[test]
fn exact_names_still_match() {
    let supported = vec!["boot".to_string(), "init_boot".to_string()];
    assert!(unsupported_partitions("boot", &supported).is_empty());
    assert!(unsupported_partitions("init_boot,boot", &supported).is_empty());
    assert_eq!(
        unsupported_partitions("system", &supported),
        vec!["system".to_string()]
    );
}

#[test]
fn empty_whitelist_allows_all() {
    assert!(unsupported_partitions("anything,goes", &[]).is_empty());
}

#[test]
fn star_prefix_matches_family() {
    let supported = vec!["xbl*".to_string(), "abl*".to_string()];
    assert!(unsupported_partitions("xbl_a,xbl_config_b,abl_a", &supported).is_empty());
    assert_eq!(
        unsupported_partitions("boot", &supported),
        vec!["boot".to_string()]
    );
}

#[test]
fn question_mark_matches_single_char() {
    let supported = vec!["boot_?".to_string()];
    assert!(unsupported_partitions("boot_a", &supported).is_empty());
    assert_eq!(
        unsupported_partitions("boot_ab", &supported),
        vec!["boot_ab".to_string()]
    );
}

#[test]
fn matches_pattern_covers_wildcards() {
    // Bare `*` allows everything.
    assert!(matches_pattern("*", "vendor_boot"));
    // Prefix / suffix / contains.
    assert!(matches_pattern("xbl*", "xbl"));
    assert!(matches_pattern("xbl*", "xbl_config_a"));
    assert!(matches_pattern("*boot", "vendor_boot"));
    assert!(matches_pattern("*boot*", "init_boot_a"));
    assert!(!matches_pattern("xbl*", "abl_a"));
    // `?` is exactly one character.
    assert!(matches_pattern("a?c", "abc"));
    assert!(!matches_pattern("a?c", "ac"));
    // No wildcards => exact, case-sensitive.
    assert!(matches_pattern("boot", "boot"));
    assert!(!matches_pattern("boot", "boota"));
    assert!(!matches_pattern("boot", "Boot"));
}
