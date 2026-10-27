//! The server's signature check against `intake_vectors.json`, a copy of the site's rules, and that
//! copy against the site's `intake-contract.ts` when the workspace has it next to this repo.
use std::{collections::BTreeMap, path::PathBuf};

use office_documents::{
    Format,
    intake::{Container, HEAD_BYTES, Rejection, check_head, server_rules, site_rules},
};
use serde::Deserialize;

const PHASE_1: [&str; 4] = ["docx", "xlsx", "pptx", "pdf"];

#[derive(Deserialize)]
struct Vectors {
    head_bytes: usize,
    signatures: BTreeMap<String, Signature>,
    site_rules: BTreeMap<String, Vec<String>>,
    server_rules: BTreeMap<String, Vec<String>>,
    cases: Vec<Case>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Signature {
    magic: String,
    within: usize,
}

#[derive(Deserialize)]
struct Case {
    ext: String,
    #[serde(default)]
    pad: usize,
    hex: String,
    site: String,
    server: String,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("intake_vectors.json")).expect("intake_vectors.json")
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn names(containers: &[Container]) -> Vec<String> {
    containers.iter().map(|c| c.name().to_owned()).collect()
}

fn format(ext: &str) -> Format {
    Format::from_extension(ext).expect("phase-1 format")
}

fn head(case: &Case) -> Vec<u8> {
    let mut head = vec![b' '; case.pad];
    head.extend(unhex(&case.hex));
    head.truncate(HEAD_BYTES);
    head
}

/// `matchesSignature` then `signatureFailure` of the site, driven by the vector tables only.
fn site_verdict(v: &Vectors, ext: &str, head: &[u8]) -> &'static str {
    let has = |name: &str| {
        let sig = &v.signatures[name];
        let magic = unhex(&sig.magic);
        (0..=sig.within)
            .take_while(|start| start + magic.len() <= head.len())
            .any(|start| head[start..start + magic.len()] == magic[..])
    };
    if v.site_rules[ext].iter().any(|k| has(k)) {
        "accept"
    } else if ext == "pptx" && has("ole") {
        "UNSUPPORTED_TYPE"
    } else {
        "INVALID_SIGNATURE"
    }
}

fn server_verdict(ext: &str, head: &[u8]) -> &'static str {
    match check_head(format(ext), head) {
        Ok(()) => "accept",
        Err(Rejection::UnsupportedType) => "UNSUPPORTED_TYPE",
        Err(_) => "INVALID_SIGNATURE",
    }
}

#[test]
fn rust_tables_match_the_vector_file() {
    let v = vectors();
    assert_eq!(v.head_bytes, HEAD_BYTES);
    for c in Container::ALL {
        let expected = &v.signatures[c.name()];
        assert_eq!(hex(c.magic()), expected.magic, "{c:?}");
        assert_eq!(c.within(), expected.within, "{c:?}");
    }
    for ext in PHASE_1 {
        assert_eq!(names(site_rules(format(ext))), v.site_rules[ext], "{ext}");
        assert_eq!(
            names(server_rules(format(ext))),
            v.server_rules[ext],
            "{ext}"
        );
        for kind in &v.server_rules[ext] {
            assert!(
                v.site_rules[ext].contains(kind),
                "server accepts more than the site: {ext}"
            );
        }
    }
}

#[test]
fn cases_give_the_recorded_verdicts() {
    let v = vectors();
    for case in &v.cases {
        let head = head(case);
        assert_eq!(
            site_verdict(&v, &case.ext, &head),
            case.site,
            "site {} {}",
            case.ext,
            case.hex
        );
        assert_eq!(
            server_verdict(&case.ext, &head),
            case.server,
            "server {} {}",
            case.ext,
            case.hex
        );
    }
}

fn site_contract() -> Option<String> {
    let path = std::env::var_os("ERPCONS_SITE_INTAKE_CONTRACT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../erpcons-office/src/lib/files/intake-contract.ts")
        });
    std::fs::read_to_string(path).ok()
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text
        .find(start)
        .unwrap_or_else(|| panic!("{start} in intake-contract.ts"));
    let rest = &text[from + start.len()..];
    &rest[..rest.find(end).unwrap_or_else(|| panic!("end of {start}"))]
}

fn quoted_list(list: &str) -> Vec<String> {
    list.trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn vector_file_matches_the_site_contract() {
    let Some(ts) = site_contract() else {
        eprintln!("skipped: erpcons-office/src/lib/files/intake-contract.ts not found");
        return;
    };
    let v = vectors();
    let head_bytes: usize = between(&ts, "headBytes:", ",")
        .trim()
        .parse()
        .expect("headBytes");
    assert_eq!(head_bytes, v.head_bytes);

    for line in between(&ts, "const SIGNATURES = {", "} as const").lines() {
        let Some((name, rest)) = line.split_once(": { magic: [") else {
            continue;
        };
        let (magic, within) = rest.split_once("], within:").expect("within");
        let magic: Vec<u8> = magic
            .split(',')
            .map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16).expect("byte"))
            .collect();
        let within = within.trim().trim_end_matches(['}', ',', ' ']).trim();
        let within = match within.strip_prefix("INTAKE_LIMITS.headBytes - ") {
            Some(n) => head_bytes - n.parse::<usize>().expect("offset"),
            None => within.parse().expect("within"),
        };
        let expected = Signature {
            magic: hex(&magic),
            within,
        };
        assert_eq!(v.signatures.get(name.trim()), Some(&expected), "{name}");
    }

    let rules = between(
        &ts,
        "SIGNATURE_RULES: Readonly<Record<string, readonly Signature[]>> = {",
        "\n}",
    );
    for ext in PHASE_1 {
        let at = rules
            .find(&format!(" {ext}: "))
            .unwrap_or_else(|| panic!("{ext} in SIGNATURE_RULES"));
        let value = rules[at + ext.len() + 3..].trim_start();
        let kinds = if value.starts_with('[') {
            quoted_list(&value[..=value.find(']').expect("]")])
        } else {
            let alias = &value[..value.find(',').expect(",")];
            quoted_list(between(
                &ts,
                &format!("const {alias}: readonly Signature[] = "),
                "\n",
            ))
        };
        assert_eq!(kinds, v.site_rules[ext], "{ext}");
    }
}
