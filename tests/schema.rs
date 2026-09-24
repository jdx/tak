//! `docs/public/schema/tak.json` must describe what `tak.toml` accepts.
//!
//! Editors validate against the schema and the parser is what actually runs,
//! so the two drifting apart is a quiet failure: a key the schema rejects but
//! tak honours, or the reverse. This keeps one config that uses every key and
//! checks both directions against it.

use std::collections::BTreeSet;

/// Every key tak.toml accepts, at least once.
const EVERYTHING: &str = r#"
[env]
deny = ["GITHUB_TOKEN"]
allow = []

[gate]
pct = 1.5

[report]
credit = false

[runner]
class = "local-test"

[defaults]
runs = "auto"
warmup = 1
budget = "30s"
min_runs = 5
max_runs = 50
prepare = ["true"]
setup = ["true"]
version_cmd = ["true", "--version"]
dir = "."
env = { A = "1" }
vars = { v = "x" }

[subject.shared]
cmd = ["true"]
when = "true"
counters = false
runs = 3
warmup = 0
budget = "1s"
min_runs = 1
max_runs = 2
prepare = "true"
setup = "true"
version_cmd = "true --version"
dir = "."
env = { B = "2" }
vars = { w = "y" }

[bench.single]
cmd = "true"
when = 'os != ""'
runs = 2
warmup = 0
budget = "5s"
min_runs = 1
max_runs = 3
prepare = ["true"]
setup = "true"
version_cmd = ["true"]
dir = "."
env = { C = "3" }
vars = { u = "z" }

[bench.multi]
subjects = ["shared"]
[bench.multi.subject.local]
cmd = ["true"]
counters = true
"#;

fn schema() -> serde_json::Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/public/schema/tak.json"
    ))
    .unwrap();
    serde_json::from_str(&text).expect("schema is valid JSON")
}

/// The property names of the schema object at `pointer`.
fn props(schema: &serde_json::Value, pointer: &str) -> BTreeSet<String> {
    schema
        .pointer(pointer)
        .and_then(|v| v.get("properties"))
        .and_then(|p| p.as_object())
        .unwrap_or_else(|| panic!("no properties at {pointer}"))
        .keys()
        .cloned()
        .collect()
}

fn keys(table: &toml::Table) -> BTreeSet<String> {
    table.keys().cloned().collect()
}

#[test]
fn the_everything_config_parses() {
    tak_cli::config::Config::parse(EVERYTHING).expect("every documented key is accepted");
}

#[test]
fn the_schema_and_the_parser_agree_on_every_key() {
    let s = schema();
    let t: toml::Table = EVERYTHING.parse().unwrap();
    let table = |v: &toml::Value| v.as_table().unwrap().clone();

    assert_eq!(keys(&t), props(&s, ""), "top level");
    for section in ["env", "gate", "report", "runner"] {
        assert_eq!(
            keys(&table(&t[section])),
            props(&s, &format!("/properties/{section}")),
            "[{section}]"
        );
    }
    assert_eq!(
        keys(&table(&t["defaults"])),
        props(&s, "/definitions/layer"),
        "[defaults]"
    );
    assert_eq!(
        keys(&table(&t["subject"]["shared"])),
        props(&s, "/definitions/subject"),
        "[subject.NAME]"
    );

    // A benchmark: the single-command form and the multi-subject form
    // between them use every key.
    let mut bench = keys(&table(&t["bench"]["single"]));
    bench.extend(keys(&table(&t["bench"]["multi"])));
    assert_eq!(bench, props(&s, "/definitions/bench"), "[bench.NAME]");
}
