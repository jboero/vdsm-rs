//! Typed Rust bindings generated from `schema/vdsm-api.yml`.
//!
//! The contents of this crate are produced by `build.rs` at compile time
//! from the vendored YAML schema. Do not edit the generated file directly —
//! re-run `cargo build` to regenerate.

#![allow(non_camel_case_types, non_snake_case, dead_code)]
#![allow(clippy::enum_variant_names, clippy::large_enum_variant)]

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_substantial_type_set() {
        assert!(
            TYPE_COUNT > 100,
            "expected >100 types from vdsm-api.yml, got {TYPE_COUNT}"
        );
    }

    #[test]
    fn schema_has_substantial_verb_set() {
        assert!(
            VERB_COUNT > 50,
            "expected >50 verbs from vdsm-api.yml, got {VERB_COUNT}"
        );
    }

    #[test]
    fn day_one_verbs_present() {
        let mut have_get_caps = false;
        let mut have_ping = false;
        let mut have_get_stats = false;
        for v in VERBS {
            if v.eq_ignore_ascii_case("Host.getCapabilities") {
                have_get_caps = true;
            }
            if v.starts_with("Host.ping") {
                have_ping = true;
            }
            if v.eq_ignore_ascii_case("Host.getStats") {
                have_get_stats = true;
            }
        }
        assert!(have_get_caps, "Host.getCapabilities missing from schema");
        assert!(have_ping, "Host.ping* missing from schema");
        assert!(have_get_stats, "Host.getStats missing from schema");
    }

    #[test]
    fn verb_roundtrip() {
        for name in VERBS {
            let v = Verb::from_wire(name).unwrap_or_else(|| panic!("from_wire failed: {name}"));
            assert_eq!(v.as_str(), *name);
        }
    }
}
