use super::{artifact_url, tag_from_location};

#[test]
fn extracts_tag_from_location_header() {
    assert_eq!(
        tag_from_location("https://github.com/tiann/KernelSU/releases/tag/v3.2.5").unwrap(),
        "v3.2.5"
    );
    // Trailing slash and query/fragment noise must not leak into the tag.
    assert_eq!(
        tag_from_location("/tiann/KernelSU/releases/tag/v3.2.5/").unwrap(),
        "v3.2.5"
    );
    assert_eq!(
        tag_from_location("https://github.com/tiann/KernelSU/releases/tag/v3.2.5?ref=x#top")
            .unwrap(),
        "v3.2.5"
    );
}

#[test]
fn rejects_locations_without_a_tag() {
    assert!(tag_from_location("https://github.com/tiann/KernelSU/releases").is_err());
    assert!(tag_from_location("/releases/tag/").is_err());
    assert!(tag_from_location("").is_err());
}

#[test]
fn builds_nightly_link_artifact_url() {
    assert_eq!(
        artifact_url("v3.2.5", "x86_64-pc-windows-gnu"),
        "https://nightly.link/tiann/KernelSU/workflows/release/v3.2.5/ksud-x86_64-pc-windows-gnu.zip"
    );
}
