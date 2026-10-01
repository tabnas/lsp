// The dynamic-add lanes (design §6) and the grammar firewall (design
// §10), case for case with ts/test/loaders.test.js and one test per
// refusal. The L2 lane is exercised with the shared strict-JSON
// GrammarSpec fixture, the same pure-data grammar the TypeScript and Go
// ports load, which is what makes it the standing proof of the lane.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tabnas::grammar::BUILTIN_SCHEMA_VERSION;
use tabnas::Tabnas;
use tabnas_lsp::loaders::{
    compile_grammar_text, entry_from_spec_json, firewall_spec, install_spec, instance_options,
    read_capped, resolve_sandboxed, watched_file, Loader, ALT_FUNC_KEYS, FORBIDDEN_KEYS,
    MAX_GRAMMAR_ALTS, MAX_GRAMMAR_BYTES, MAX_GRAMMAR_DEPTH, MAX_GRAMMAR_RULES,
};
use tabnas_lsp::{Entry, EntrySource, Issue, Load, LoadError, SpecSource};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
}

fn json_grammar() -> Value {
    let text = fs::read_to_string(repo_root().join("test/fixtures/json-grammar.json"))
        .expect("test/fixtures/json-grammar.json is readable");
    serde_json::from_str(&text).expect("the shared grammar is JSON")
}

/// A fresh, empty folder under the system temp directory.
fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tabnas-lsp-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// An entry from its JSON fields, as a registry or manifest holds it.
fn entry(fields: Value) -> Entry {
    serde_json::from_value(fields).expect("an entry")
}

fn has(issues: &[Issue], path: &str, needle: &str) -> bool {
    issues
        .iter()
        .any(|issue| issue.path == path && issue.message.contains(needle))
}

fn refused(result: Result<Tabnas, LoadError>) -> LoadError {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------
// The firewall: one test per refusal.

#[test]
fn the_firewall_accepts_the_shared_strict_json_grammar() {
    assert_eq!(firewall_spec(&json_grammar()), []);
}

#[test]
fn the_firewall_refuses_a_grammar_that_is_not_an_object() {
    for bad in [json!([]), json!("rule"), json!(null), json!(1)] {
        let issues = firewall_spec(&bad);
        assert_eq!(issues.len(), 1, "{bad}");
        assert_eq!(issues[0].path, "$");
        assert_eq!(
            issues[0].message,
            "grammar must be a JSON object (the serialized GrammarSpec form)"
        );
    }
}

#[test]
fn the_firewall_refuses_prototype_pollution_keys_anywhere_in_the_tree() {
    for key in FORBIDDEN_KEYS {
        let src = format!(
            r##"{{"rule":{{"val":{{"open":[{{"s":"#NR","u":{{"{key}":{{"x":1}}}}}}]}}}}}}"##
        );
        let spec: Value = serde_json::from_str(&src).unwrap();
        let issues = firewall_spec(&spec);
        let path = format!("$.rule.val.open[0].u.{key}");
        assert!(
            has(&issues, &path, &format!("forbidden key '{key}'")),
            "{issues:?}"
        );
    }
    // A poisoned tree is reported alone: nothing below touches it, so the
    // ref bag beside it goes unmentioned.
    let spec = json!({"ref": {}, "rule": {"__proto__": {"open": []}}});
    let issues = firewall_spec(&spec);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].path, "$.rule.__proto__");
    assert!(issues[0].message.ends_with(
        "refused to prevent prototype pollution (a serialized grammar is data, never a route \
         to Object.prototype)"
    ));
}

#[test]
fn the_firewall_refuses_pathological_nesting_instead_of_recursing_off_the_stack() {
    // The root is depth 0 and `options` depth 1, so wrapping its inner
    // object MAX_GRAMMAR_DEPTH - 1 times puts the deepest container at
    // depth MAX_GRAMMAR_DEPTH, which passes; one level more is refused
    // (the TypeScript firewall answers the same two specs the same way).
    let nest = |levels: usize| {
        let mut deep = json!({"x": 1});
        for _ in 0..levels {
            deep = json!({ "d": deep });
        }
        json!({ "options": deep })
    };
    assert_eq!(firewall_spec(&nest(MAX_GRAMMAR_DEPTH - 1)), []);
    let issues = firewall_spec(&nest(MAX_GRAMMAR_DEPTH));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0]
        .message
        .starts_with("grammar nesting deeper than 100 levels: refused"));
    // Far deeper, and in arrays too: one finding, no overflow. (Deeper
    // still is unreachable from text: serde_json refuses to parse past
    // 128 levels, so only a host's own Value could be; this one is kept
    // shallow enough for its recursive drop.)
    let mut deep = json!(1);
    for _ in 0..500 {
        deep = json!([deep]);
    }
    let issues = firewall_spec(&json!({ "rule": deep }));
    assert_eq!(issues.len(), 1);
    assert!(issues[0].message.contains("nesting deeper"));
}

#[test]
fn the_firewall_refuses_a_ref_bag_live_functions_are_not_json() {
    let issues = firewall_spec(&json!({"ref": {}, "rule": {}}));
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "$.ref");
    assert_eq!(
        issues[0].message,
        "'ref' is not part of the serialized grammar form: live functions are not JSON. Name \
         $-suffixed engine builtins instead."
    );
}

#[test]
fn the_firewall_refuses_plugins_inside_grammar_options() {
    let issues = firewall_spec(&json!({"options": {"plugins": []}}));
    assert!(
        has(&issues, "$.options.plugins", "a plugin is live code"),
        "{issues:?}"
    );
    // Only in `options`: a rule named plugins is a rule.
    assert_eq!(
        firewall_spec(&json!({"rule": {"plugins": {"open": []}}})),
        []
    );
}

#[test]
fn the_firewall_gates_the_builtin_schema_version() {
    let issues = firewall_spec(&json!({"v": 999, "rule": {}}));
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "$.v");
    assert_eq!(
        issues[0].message,
        format!(
            "grammar declares builtin schema version 999; this engine supports up to \
             {BUILTIN_SCHEMA_VERSION}"
        )
    );
    let next = BUILTIN_SCHEMA_VERSION + 1;
    assert_eq!(firewall_spec(&json!({ "v": next })).len(), 1);
    assert_eq!(firewall_spec(&json!({ "v": BUILTIN_SCHEMA_VERSION })), []);
    // Anything but a number is version 1, as `typeof gs.v` reads it.
    assert_eq!(firewall_spec(&json!({"v": "999"})), []);
}

#[test]
fn the_firewall_passes_regexes_and_escapes_in_options_and_refuses_bare_refs() {
    let ok = json!({"options": {
        "x": "@@literal", "y": "@SKIP", "z": "@/ab+/i", "w": "@~/ab+/", "b": "@node$",
        "n": "not a ref", "s": "@ spaced", "d": "@1digit", "e": "@"
    }});
    assert_eq!(firewall_spec(&ok), []);
    let bad = json!({"options": {"x": "@custom", "deep": {"list": ["@a.b-c", "@evil$"]}}});
    let issues = firewall_spec(&bad);
    assert_eq!(issues.len(), 3, "{issues:?}");
    assert!(has(
        &issues,
        "$.options.x",
        "unknown function reference '@custom': a serialized grammar may only name \
         $-suffixed engine builtins"
    ));
    assert!(has(&issues, "$.options.deep.list[0]", "'@a.b-c'"));
    assert!(has(&issues, "$.options.deep.list[1]", "'@evil$'"));
}

#[test]
fn the_options_scan_stops_collecting_past_a_hundred_findings() {
    let many: Vec<Value> = (0..500).map(|i| json!(format!("@r{i}"))).collect();
    let issues = firewall_spec(&json!({"options": { "x": many }}));
    // `100 < out.length` stops the scan once it holds 101.
    assert_eq!(issues.len(), 101);
}

#[test]
fn the_firewall_refuses_non_builtin_function_refs_in_alt_positions() {
    for key in ALT_FUNC_KEYS {
        let spec = json!({"rule": {"val": {"open": [{"s": "#NR", key: "@evil"}]}}});
        let issues = firewall_spec(&spec);
        assert!(
            has(
                &issues,
                &format!("$.rule.val.open[0].{key}"),
                "unknown function reference '@evil'"
            ),
            "{key}: {issues:?}"
        );
    }
    // In an alt position every @-string is a reference, even one the
    // options scan would take for data.
    let spec = json!({"rule": {"val": {"close": {"alts": [{"a": ["@node$", "@@x", "plain"]}]}}}});
    let issues = firewall_spec(&spec);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].path, "$.rule.val.close[0].a[1]");
    // Builtins pass in every position, a condition included.
    let spec = json!({"rule": {"val": {"open": [
        {"a": "@object$", "c": "@probePhase1$"},
        {"a": ["@push$", "@value$"]},
    ]}}});
    assert_eq!(firewall_spec(&spec), []);
}

#[test]
fn the_firewall_caps_the_rule_count() {
    let rules = |count: usize| {
        let mut rule = serde_json::Map::new();
        for i in 0..count {
            rule.insert(format!("r{i}"), json!({"open": [{"s": "#NR"}]}));
        }
        json!({ "rule": rule })
    };
    assert_eq!(firewall_spec(&rules(MAX_GRAMMAR_RULES)), []);
    let issues = firewall_spec(&rules(MAX_GRAMMAR_RULES + 1));
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "$.rule");
    assert_eq!(
        issues[0].message,
        "grammar defines 5001 rules, more than 5000"
    );
}

#[test]
fn the_firewall_caps_total_alternates_so_one_rule_cannot_smuggle_an_alts_bomb() {
    let alts = |count: usize| vec![json!({"s": "#NR"}); count];
    assert_eq!(
        firewall_spec(&json!({"rule": {"top": {"open": alts(MAX_GRAMMAR_ALTS)}}})),
        []
    );
    let issues = firewall_spec(&json!({"rule": {"top": {"open": alts(MAX_GRAMMAR_ALTS + 1)}}}));
    assert_eq!(issues.len(), 1);
    assert_eq!(
        issues[0].message,
        "grammar defines more than 10000 alternates in total"
    );
    // Counted across rules and states, the `{alts}` wrapper included.
    let spec = json!({"rule": {
        "a": {"open": alts(5000), "close": {"alts": alts(4000)}},
        "b": {"close": alts(1001)},
    }});
    assert!(has(&firewall_spec(&spec), "$.rule", "alternates in total"));
}

// ---------------------------------------------------------------------
// L2: serialized GrammarSpec.

#[test]
fn l2_loads_the_shared_grammar_from_an_inline_object() {
    let loader = Loader::new();
    let entry = entry(json!({
        "name": "jsonspec", "languageId": "jsonspec", "grammarKind": "data",
        "load": {"spec": json_grammar()},
    }));
    let parser = loader
        .make_instance(&entry)
        .expect("the shared grammar loads");
    let out = parser.parse_recover(r#"{"a":[1,2]}"#);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(out.value.unwrap().to_string(), r#"{"a":[1,2]}"#);
}

#[test]
fn l2_loads_a_spec_from_a_sandboxed_file_path() {
    let loader = Loader::new();
    let entry = entry(json!({
        "name": "jsonspec", "languageId": "jsonspec",
        "load": {"spec": "fixtures/json-grammar.json"},
        "_source": "workspace",
        "_dir": repo_root().join("test"),
    }));
    let parser = loader.make_instance(&entry).expect("the file loads");
    let out = parser.parse_recover("[true,null]");
    assert!(out.errors.is_empty());
    assert_eq!(out.value.unwrap().to_string(), "[true,null]");
}

#[test]
fn l2_recovery_is_on_so_a_broken_document_yields_errors() {
    let loader = Loader::new();
    let entry = entry(json!({
        "name": "jsonspec", "languageId": "jsonspec", "load": {"spec": json_grammar()},
    }));
    let parser = loader.make_instance(&entry).unwrap();
    assert!(parser.options.parse.recover.enabled);
    let out = parser.parse_recover(r#"{"a":true blah,"b":2}"#);
    assert!(!out.errors.is_empty(), "recovery reports the error");
}

#[test]
fn l2_an_entry_configuring_parse_options_keeps_recovery_on() {
    // A shallow options merge dropped recover.enabled the moment an entry
    // set ANY parse option. The nested merge keeps the default under the
    // entry's own settings, and the entry's explicit recover still wins.
    let loader = Loader::new();
    let with = |options: Value| {
        entry(json!({
            "name": "jsonspec", "languageId": "jsonspec",
            "load": {"spec": json_grammar()}, "options": options,
        }))
    };
    let parser = loader.make_instance(&with(json!({"parse": {}}))).unwrap();
    assert!(parser.options.parse.recover.enabled);
    let out = parser.parse_recover(r#"{"a":true blah,"b":2}"#);
    assert!(!out.errors.is_empty(), "recovery lost under options.parse");
    let parser = loader
        .make_instance(&with(json!({"parse": {"recover": {"enabled": false}}})))
        .unwrap();
    assert!(!parser.options.parse.recover.enabled);
}

#[test]
fn the_entry_sync_groups_apply_unless_its_options_declare_their_own() {
    let mut entry = entry(json!({"name": "s", "syncGroups": ["end", "comma"]}));
    assert_eq!(
        instance_options(&entry),
        json!({"parse": {"recover": {"enabled": true, "syncGroups": ["end", "comma"]}}})
    );
    entry.options = Some(json!({"lex": {"x": 1}, "parse": {"recover": {"syncGroups": null}}}));
    assert_eq!(
        instance_options(&entry),
        json!({"lex": {"x": 1}, "parse": {"recover": {"enabled": true, "syncGroups": null}}})
    );
    entry.load = Some(Load::Spec(SpecSource::Inline(json_grammar())));
    entry.options = None;
    let parser = Loader::new().make_instance(&entry).unwrap();
    assert_eq!(parser.options.parse.recover.sync_groups, ["end", "comma"]);
}

#[test]
fn l2_a_poisoned_spec_is_refused_before_any_engine_load() {
    let spec: Value = serde_json::from_str(r#"{"rule":{"__proto__":{"open":[]}}}"#).unwrap();
    let entry = entry(json!({"name": "bad", "languageId": "bad", "load": {"spec": spec}}));
    let error = refused(Loader::new().make_instance(&entry));
    assert_eq!(error.message, "grammar for bad failed the firewall");
    assert_eq!(error.issues.len(), 1);
    assert!(error
        .to_string()
        .starts_with("grammar for bad failed the firewall\n  $.rule.__proto__: forbidden key"));
}

#[test]
fn l2_entry_options_are_held_to_the_firewall_too() {
    // Entry options go through the engine's serialized-options path,
    // which resolves @-references, so they pass the same scan.
    let entry = entry(json!({
        "name": "o", "languageId": "o", "load": {"spec": json_grammar()},
        "options": {"value": {"def": {"x": {"val": "@custom"}}}},
    }));
    let error = refused(Loader::new().make_instance(&entry));
    assert_eq!(error.message, "options for o failed the firewall");
    assert!(has(&error.issues, "$.options.value.def.x.val", "'@custom'"));
}

#[test]
fn l2_a_spec_the_engine_rejects_is_a_load_error() {
    let entry = entry(json!({
        "name": "e", "languageId": "e", "load": {"spec": {"rule": {"top": "not a rule"}}},
    }));
    let error = refused(Loader::new().make_instance(&entry));
    assert!(error.message.starts_with("Grammar:"), "{error}");
    assert!(error.issues.is_empty());
}

#[test]
fn l2_a_spec_file_nested_past_the_json_reader_gets_the_firewalls_refusal() {
    // serde_json stops at 128 levels, above MAX_GRAMMAR_DEPTH; JSON.parse
    // reads any depth and the firewall refuses it, so the refusal is the
    // firewall's here too.
    let dir = temp_dir("deepfile");
    let depth = 130;
    let deep = format!(
        "{{\"rule\": {{}}, \"options\": {}{}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    fs::write(dir.join("deep.json"), deep).unwrap();
    let entry = entry(json!({
        "name": "d", "languageId": "d", "load": {"spec": "deep.json"},
        "_source": "workspace", "_dir": dir,
    }));
    let error = refused(Loader::new().make_instance(&entry));
    assert_eq!(
        error.message, "grammar for d failed the firewall",
        "{error}"
    );
    assert!(
        has(
            &error.issues,
            "$",
            &format!("grammar nesting deeper than {MAX_GRAMMAR_DEPTH} levels")
        ),
        "{error}"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_alt_scan_stops_collecting_past_a_hundred_findings_too() {
    // Each unknown $-name costs an engine probe, so the scan of alt
    // positions stops where the options scan does; a spec with more
    // unknown references than that is refused either way.
    let names: Vec<Value> = (0..20_000).map(|i| json!(format!("@r{i}$"))).collect();
    let spec = json!({"rule": {"val": {"open": [{"s": "#NR", "a": names}]}}});
    let started = std::time::Instant::now();
    let issues = firewall_spec(&spec);
    assert_eq!(issues.len(), 101, "{}", issues.len());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // The same names again are remembered, not probed.
    let started = std::time::Instant::now();
    assert_eq!(firewall_spec(&spec).len(), 101);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn load_dispatches_spec_then_grammar_then_module_as_the_canonical_loader_does() {
    let load = |value: Value| entry(json!({"name": "n", "languageId": "n", "load": value})).load();
    assert_eq!(
        load(json!({"spec": "g.json", "module": "x"})),
        Load::Spec(SpecSource::File("g.json".into()))
    );
    assert_eq!(
        load(json!({"spec": {"rule": {}}, "grammar": "g.abnf"})),
        Load::Spec(SpecSource::Inline(json!({"rule": {}})))
    );
    assert_eq!(
        load(json!({"spec": null, "grammar": "g.abnf", "module": "x"})),
        Load::Grammar("g.abnf".into())
    );
    assert_eq!(
        load(json!({"grammar": null, "module": "m", "extra": 1})),
        Load::Module("m".into())
    );
    assert_eq!(load(json!({})), Load::Module(String::new()));
    assert_eq!(load(json!({"module": null})), Load::Module(String::new()));
    // An entry with no load at all is its own module.
    assert_eq!(
        entry(json!({"name": "n", "languageId": "n"})).load(),
        Load::Module("n".into())
    );
    // A load that is not an object, or a lane that is not a string, is
    // refused with the field named.
    for value in [
        json!("g.json"),
        json!([]),
        json!({"grammar": 1}),
        json!({"module": []}),
    ] {
        let error =
            serde_json::from_value::<Entry>(json!({"name": "n", "languageId": "n", "load": value}))
                .unwrap_err();
        assert!(
            error.to_string().contains("invalid value"),
            "{value}: {error}"
        );
    }
}

#[test]
fn l2_a_spec_file_that_is_not_json_is_refused() {
    let dir = temp_dir("notjson");
    fs::write(dir.join("g.json"), "{ nope").unwrap();
    let entry = entry(json!({
        "name": "j", "languageId": "j", "load": {"spec": "g.json"},
        "_source": "workspace", "_dir": dir,
    }));
    let error = refused(Loader::new().make_instance(&entry));
    assert!(
        error
            .message
            .starts_with("grammar file g.json is not valid JSON:"),
        "{error}"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_explicit_null_spec_falls_through_to_the_module() {
    // `null != load.spec` is false for null, so the canonical dispatch
    // loads the entry's module.
    let entry = entry(json!({"name": "@tabnas/nothing", "load": {"spec": null}}));
    let error = refused(Loader::new().make_instance(&entry));
    assert!(
        error
            .message
            .starts_with("grammar module @tabnas/nothing is not linked"),
        "{error}"
    );
}

// ---------------------------------------------------------------------
// The sandbox and the read cap: one test per refusal.

#[test]
fn sandbox_a_grammar_path_needs_a_base_directory() {
    let error = resolve_sandboxed("g.json", None).unwrap_err();
    assert_eq!(
        error.message,
        "grammar file paths need a base directory: g.json"
    );
    // Through the loader: an entry with a file and no folder.
    let entry = entry(json!({"name": "n", "load": {"spec": "g.json"}}));
    let error = refused(Loader::new().make_instance(&entry));
    assert!(error.message.contains("need a base directory"));
}

#[test]
fn sandbox_an_unreadable_workspace_folder_is_refused() {
    let missing = temp_dir("gone").join("not-there");
    let error = resolve_sandboxed("g.json", Some(&missing)).unwrap_err();
    assert!(
        error.message.starts_with(&format!(
            "workspace folder not readable: {}: ",
            missing.display()
        )),
        "{error}"
    );
}

#[test]
fn sandbox_a_spec_path_may_not_escape_its_folder() {
    let root = temp_dir("sbx");
    let ws = root.join("project");
    fs::create_dir_all(ws.join("sub")).unwrap();
    fs::create_dir_all(root.join("project-evil")).unwrap();
    fs::write(root.join("outside.json"), "{}").unwrap();
    fs::write(root.join("project-evil").join("x.json"), "{}").unwrap();
    fs::write(ws.join("sub").join("x.json"), "{}").unwrap();

    let escapes = |file: &str| {
        let error = resolve_sandboxed(file, Some(&ws)).unwrap_err();
        assert!(
            error.message.starts_with(&format!(
                "grammar file escapes its workspace folder: {file} (from "
            )),
            "{error}"
        );
    };
    escapes("../outside.json");
    escapes(&root.join("outside.json").to_string_lossy());
    // A sibling folder sharing the prefix is not inside.
    escapes("../project-evil/x.json");
    // Refused lexically even when the target does not exist.
    escapes("../../no/such/file.json");
    assert_eq!(
        resolve_sandboxed("sub/x.json", Some(&ws)).unwrap(),
        fs::canonicalize(ws.join("sub").join("x.json")).unwrap()
    );
    assert_eq!(
        resolve_sandboxed("./sub/../sub/x.json", Some(&ws)).unwrap(),
        fs::canonicalize(ws.join("sub").join("x.json")).unwrap()
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn sandbox_a_symlink_out_of_the_folder_is_refused() {
    // The check runs on REAL paths: a lexical prefix test accepts a
    // `grammar.json` that links outside the workspace.
    let root = temp_dir("sbl");
    let ws = root.join("project");
    fs::create_dir_all(&ws).unwrap();
    fs::write(root.join("secret.json"), "{}").unwrap();
    std::os::unix::fs::symlink(root.join("secret.json"), ws.join("grammar.json")).unwrap();
    let error = resolve_sandboxed("grammar.json", Some(&ws)).unwrap_err();
    assert!(
        error.message.starts_with(
            "grammar file escapes its workspace folder (via symlink): grammar.json -> "
        ),
        "{error}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sandbox_a_missing_grammar_file_is_refused() {
    let ws = temp_dir("missing");
    let error = resolve_sandboxed("nope.json", Some(&ws)).unwrap_err();
    assert!(
        error
            .message
            .starts_with("grammar file not readable: nope.json: "),
        "{error}"
    );
    let _ = fs::remove_dir_all(ws);
}

#[test]
fn the_read_cap_refuses_an_oversized_grammar_file() {
    let ws = temp_dir("big");
    let file = ws.join("big.json");
    fs::write(&file, vec![b' '; MAX_GRAMMAR_BYTES as usize + 1]).unwrap();
    let entry = entry(json!({
        "name": "big", "languageId": "big", "load": {"spec": "big.json"},
        "_source": "workspace", "_dir": ws,
    }));
    let error = read_capped(&file, &entry).unwrap_err();
    assert_eq!(
        error.message,
        "grammar file for big is 1000001 bytes, larger than 1000000"
    );
    // The loader applies it before the JSON parse.
    let error = refused(Loader::new().make_instance(&entry));
    assert!(error.message.contains("larger than 1000000"), "{error}");
    // At the cap exactly, the file is read.
    fs::write(&file, vec![b' '; MAX_GRAMMAR_BYTES as usize]).unwrap();
    assert_eq!(read_capped(&file, &entry).unwrap().len(), 1_000_000);
    let _ = fs::remove_dir_all(ws);
}

// ---------------------------------------------------------------------
// L1: linked modules and the workspace trust gate.

/// A linked grammar that installs one recognisable rule.
fn plain_module(rule: &'static str) -> tabnas_lsp::loaders::Factory {
    Arc::new(move || {
        let mut parser = Tabnas::new();
        parser
            .grammar_json(&format!(
                r#"{{"rule":{{"{rule}":{{"open":[{{"s":[]}}]}}}}}}"#
            ))
            .expect("a one-rule grammar");
        parser
    })
}

fn workspace_module_entry() -> Entry {
    entry(json!({
        "name": "wsmod", "languageId": "wsmod",
        "load": {"module": "some-module"},
        "_source": "workspace", "_dir": "/tmp/ws",
    }))
}

#[test]
fn a_workspace_module_load_is_refused_without_the_trust_flag() {
    let mut loader = Loader::new();
    loader.link("some-module", plain_module("plained"));
    let error = refused(loader.make_instance(&workspace_module_entry()));
    assert_eq!(
        error.message,
        "workspace entry wsmod loads module some-module, which runs code from the workspace. \
         Refused: set trustWorkspaceModules in initializationOptions to allow it."
    );
}

#[test]
fn the_trust_flag_is_read_late_so_initialize_can_grant_it() {
    let mut loader = Loader::new();
    loader.link("some-module", plain_module("plained"));
    // The server holds a clone, turned into its MakeInstance, before it
    // learns the setting.
    let make = loader.clone().into_make_instance();
    let entry = workspace_module_entry();
    assert!(make(&entry).is_err());
    loader.set_trust_workspace_modules(true); // what initialize does
    let parser = make(&entry).expect("trusted now");
    assert!(parser.rule_names().contains(&"plained".to_string()));
    assert!(parser.options.parse.recover.enabled);
    // The handle is the same setting.
    loader.trust_handle().store(false, Ordering::SeqCst);
    assert!(make(&entry).is_err());
}

#[test]
fn bundled_and_user_module_entries_need_no_trust_flag() {
    let mut loader = Loader::new();
    loader.link("@tabnas/fleetmod", plain_module("fleeted"));
    let bundled = entry(json!({"name": "@tabnas/fleetmod"}));
    assert!(loader.make_instance(&bundled).is_ok());
    let mut user = bundled.clone();
    user.source = EntrySource::User;
    assert!(loader.make_instance(&user).is_ok());
}

#[test]
fn a_module_that_is_not_linked_is_refused_by_name() {
    let entry = entry(json!({"name": "@tabnas/elsewhere"}));
    let error = refused(Loader::new().make_instance(&entry));
    assert_eq!(
        error.message,
        "grammar module @tabnas/elsewhere is not linked into this server: a Rust server serves \
         the grammars built into it (the `fleet` feature, or a host's Loader::link)"
    );
}

#[test]
fn the_tabnas_scope_fallback_works_for_short_names() {
    let mut loader = Loader::new();
    loader.link("@tabnas/shorty", plain_module("shortied"));
    let parser = loader
        .make_instance(&entry(json!({"name": "shorty"})))
        .unwrap();
    assert!(parser.rule_names().contains(&"shortied".to_string()));
    // Never for a scoped name.
    assert!(loader
        .make_instance(&entry(json!({"name": "@other/shorty"})))
        .is_err());
}

#[test]
fn a_module_entry_loads_the_module_its_load_names() {
    let mut loader = Loader::new();
    loader.link("@tabnas/named", plain_module("by_name"));
    loader.link("@tabnas/loaded", plain_module("by_load"));
    let entry = entry(json!({"name": "@tabnas/named", "load": {"module": "@tabnas/loaded"}}));
    let parser = loader.make_instance(&entry).unwrap();
    assert!(parser.rule_names().contains(&"by_load".to_string()));
}

#[test]
fn every_declared_layer_must_be_linked_and_the_last_one_builds() {
    let mut loader = Loader::new();
    loader.link("@tabnas/top", plain_module("topped"));
    // A grammar entry's base is a layer; it must be linked.
    let layered = entry(json!({"name": "@tabnas/top", "base": "@tabnas/base"}));
    let error = refused(loader.make_instance(&layered));
    assert!(error
        .message
        .starts_with("grammar module @tabnas/base is not linked"));
    loader.link("@tabnas/base", plain_module("based"));
    let parser = loader.make_instance(&layered).unwrap();
    // A linked grammar is complete: the last layer's factory builds it.
    assert!(parser.rule_names().contains(&"topped".to_string()));
    // A compiler's base is its library dependency and is not a layer.
    let mut compiler = layered.clone();
    compiler.base = Some("@tabnas/unlinked".into());
    compiler.plugin_kind = Some("compiler".into());
    assert!(loader.make_instance(&compiler).is_ok());
    // An explicit stack replaces base and name.
    let stacked = entry(json!({"name": "@tabnas/nope", "stack": ["@tabnas/base"]}));
    let parser = loader.make_instance(&stacked).unwrap();
    assert!(parser.rule_names().contains(&"based".to_string()));
}

#[test]
fn a_spec_layering_on_a_base_loads_against_that_base() {
    // Composition-aware: the spec installs over its declared stack, not
    // a bare engine.
    let mut loader = Loader::new();
    loader.link("@tabnas/base", plain_module("based"));
    let entry = entry(json!({
        "name": "over", "languageId": "over", "base": "@tabnas/base",
        "load": {"spec": {"rule": {"added": {"open": [{"s": []}]}}}},
    }));
    let parser = loader.make_instance(&entry).unwrap();
    let names = parser.rule_names();
    assert!(names.contains(&"based".to_string()), "{names:?}");
    assert!(names.contains(&"added".to_string()), "{names:?}");
    // And a spec over a base that is not linked is refused by name.
    let error = refused(Loader::new().make_instance(&entry));
    assert!(error.message.contains("@tabnas/base is not linked"));
}

// ---------------------------------------------------------------------
// L3: BNF-dialect grammar text.

fn grammar_entry(file: &str, dir: &Path) -> Entry {
    entry(json!({
        "name": "mydsl", "languageId": "mydsl", "load": {"grammar": file},
        "_source": "workspace", "_dir": dir,
    }))
}

#[test]
fn l3_an_unknown_grammar_extension_is_refused() {
    let dir = temp_dir("l3x");
    fs::write(dir.join("my.pegjs"), "x").unwrap();
    let error = refused(Loader::new().make_instance(&grammar_entry("my.pegjs", &dir)));
    assert!(
        error
            .message
            .starts_with("unknown grammar dialect .pegjs for "),
        "{error}"
    );
    assert!(error
        .message
        .ends_with("expected one of: .abnf, .ebnf, .gbnf"));
    let _ = fs::remove_dir_all(dir);
}

#[cfg(not(feature = "dialects"))]
#[test]
fn l3_without_the_dialects_feature_a_grammar_is_refused_as_not_compiled_in() {
    let dir = temp_dir("l3n");
    fs::write(dir.join("my.gbnf"), "root ::= \"a\"").unwrap();
    let error = refused(Loader::new().make_instance(&grammar_entry("my.gbnf", &dir)));
    assert!(
        error.message.starts_with(
            "grammar dialect package not compiled in: @tabnas/gbnf (needed to compile "
        ),
        "{error}"
    );
    assert!(error
        .message
        .ends_with("build tabnas-lsp with the `dialects` feature"));
    let _ = fs::remove_dir_all(dir);
}

#[cfg(feature = "dialects")]
mod dialects {
    use super::*;
    use std::time::{Duration, Instant};
    use tabnas_lsp::loaders::MAX_REPETITION_BOUND;

    #[test]
    fn l3_compiles_each_dialect_through_its_own_crate() {
        let dir = temp_dir("l3d");
        for (file, src, doc) in [
            ("my.abnf", "top = 1*DIGIT\n", "123"),
            ("my.ebnf", "top ::= \"a\" \"b\"*\n", "abbb"),
            ("my.gbnf", "root ::= \"x\" [0-9]+\n", "x42"),
        ] {
            fs::write(dir.join(file), src).unwrap();
            let parser = Loader::new()
                .make_instance(&grammar_entry(file, &dir))
                .unwrap_or_else(|error| panic!("{file}: {error}"));
            assert!(parser.options.parse.recover.enabled, "{file}");
            let out = parser.parse_recover(doc);
            assert!(out.errors.is_empty(), "{file}: {:?}", out.errors);
            let out = parser.parse_recover("?");
            assert!(
                !out.errors.is_empty(),
                "{file} rejects what it does not describe"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn l3_output_is_pure_data_that_passes_the_firewall() {
        // `builtins: true`, then the pure lowering: no ref bag, a schema
        // version stamped.
        let spec = compile_grammar_text(Path::new("g.abnf"), "top = \"a\" / \"b\"\n").unwrap();
        assert!(spec.get("ref").is_none(), "{spec}");
        assert!(spec.get("v").is_some_and(Value::is_u64), "{spec}");
        assert_eq!(firewall_spec(&spec), []);
    }

    #[test]
    fn l3_dispatches_per_dialect_and_never_tries_them_all() {
        // ABNF text in an .ebnf file is compiled by the EBNF crate alone,
        // which refuses it.
        let abnf = "top = 1*DIGIT\n";
        assert!(compile_grammar_text(Path::new("g.abnf"), abnf).is_ok());
        assert!(compile_grammar_text(Path::new("g.ebnf"), abnf).is_err());
    }

    #[test]
    fn l3_a_grammar_that_does_not_compile_is_a_load_error() {
        let dir = temp_dir("l3e");
        fs::write(dir.join("bad.abnf"), "= = =\n").unwrap();
        let error = refused(Loader::new().make_instance(&grammar_entry("bad.abnf", &dir)));
        assert!(!error.message.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn l3_a_count_past_the_repetition_cap_is_refused_before_the_compile() {
        let dir = temp_dir("l3r");
        let count = MAX_REPETITION_BOUND + 1;
        fs::write(dir.join("wide.abnf"), format!("top = 0*{count}\"x\"\n")).unwrap();
        let started = Instant::now();
        let error = refused(Loader::new().make_instance(&grammar_entry("wide.abnf", &dir)));
        assert!(
            error
                .message
                .contains(&format!("{count} times, more than {MAX_REPETITION_BOUND}")),
            "{error}"
        );
        // Refused by the scan, not by a compile: at the cap the compile
        // takes seconds in a debug build.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn l3_a_compile_past_its_budget_refuses_the_entry() {
        let dir = temp_dir("l3b");
        fs::write(
            dir.join("slow.abnf"),
            format!("top = 0*{MAX_REPETITION_BOUND}\"x\"\n"),
        )
        .unwrap();
        let loader = Loader::new().with_compile_budget(Some(Duration::from_millis(1)));
        let error = refused(loader.make_instance(&grammar_entry("slow.abnf", &dir)));
        assert!(
            error.message.contains("did not compile within 1 ms"),
            "{error}"
        );
        // Without a budget the same grammar compiles.
        let loader = Loader::new().with_compile_budget(None);
        assert!(loader
            .make_instance(&grammar_entry("slow.abnf", &dir))
            .is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn l3_the_compiled_spec_is_firewalled_too() {
        // More productions than the rule cap: the compiled spec reaches
        // the same firewall an L2 spec does.
        let mut src = String::from("top = r0\n");
        for i in 0..=MAX_GRAMMAR_RULES {
            src.push_str(&format!("r{i} = \"a\"\n"));
        }
        let dir = temp_dir("l3f");
        fs::write(dir.join("many.abnf"), src).unwrap();
        let error = refused(Loader::new().make_instance(&grammar_entry("many.abnf", &dir)));
        assert_eq!(
            error.message, "grammar for mydsl failed the firewall",
            "{error}"
        );
        assert!(has(&error.issues, "$.rule", "more than 5000"));
        let _ = fs::remove_dir_all(dir);
    }
}

// ---------------------------------------------------------------------
// The single-language lane, and hot reload's entry points.

#[test]
fn entry_from_spec_json_is_the_l2_lane_on_its_own() {
    let spec = serde_json::to_string(&json_grammar()).unwrap();
    let (entry, make) = entry_from_spec_json("jsonspec", &[".jsonspec"], &spec);
    assert_eq!(entry.language_id(), "jsonspec");
    assert_eq!(entry.extensions, [".jsonspec"]);
    assert_eq!(entry.plugin_kind(), "grammar");
    assert!(entry.is_clean() && entry.is_enabled());
    assert!(matches!(entry.load(), Load::Spec(SpecSource::Inline(_))));
    let parser = make(&entry).unwrap();
    assert!(parser.options.parse.recover.enabled);
    assert_eq!(parser.parse("[1]").unwrap().to_string(), "[1]");
    // The same entry through the general loader builds the same grammar.
    assert!(Loader::new().make_instance(&entry).is_ok());

    // Unparseable JSON fails every make, naming the language.
    let (_, make) = entry_from_spec_json("broken", &[], "{ nope");
    let error = refused(make(&Entry::new("broken")));
    assert!(error.message.starts_with("grammar for broken: "), "{error}");
    // A poisoned spec is refused at every make.
    let (entry, make) = entry_from_spec_json("poison", &[], r#"{"ref": {}}"#);
    let error = refused(make(&entry));
    assert_eq!(error.message, "grammar for poison failed the firewall");
}

#[test]
fn install_spec_firewalls_before_the_engine_sees_the_spec() {
    let entry = Entry::new("direct");
    let options = instance_options(&entry);
    let error = refused(install_spec(
        json!({"rule": {"x": {"open": [{"a": "@evil"}]}}}),
        &entry,
        &options,
        &Loader::new(),
    ));
    assert_eq!(error.message, "grammar for direct failed the firewall");
    assert!(install_spec(json_grammar(), &entry, &options, &Loader::new()).is_ok());
}

#[test]
fn a_workspace_grammar_file_is_what_hot_reload_watches() {
    let spec = entry(json!({
        "name": "a", "load": {"spec": "./g/../g.json"},
        "_source": "workspace", "_dir": "/ws/a",
    }));
    assert_eq!(watched_file(&spec), Some(PathBuf::from("/ws/a/g.json")));
    let grammar = entry(json!({
        "name": "b", "load": {"grammar": "gr/my.abnf"},
        "_source": "workspace", "_dir": "/ws/b",
    }));
    assert_eq!(
        watched_file(&grammar),
        Some(PathBuf::from("/ws/b/gr/my.abnf"))
    );
    // Nothing to watch: an inline spec, a module, a non-workspace entry,
    // a workspace entry with no folder.
    for fields in [
        json!({"name": "c", "load": {"spec": {}}, "_source": "workspace", "_dir": "/ws"}),
        json!({"name": "d", "_source": "workspace", "_dir": "/ws"}),
        json!({"name": "e", "load": {"spec": "g.json"}, "_dir": "/ws"}),
        json!({"name": "f", "load": {"spec": "g.json"}, "_source": "workspace"}),
    ] {
        assert_eq!(watched_file(&entry(fields.clone())), None, "{fields}");
    }
}

#[test]
fn a_rebuild_after_a_file_change_reads_the_file_afresh() {
    // Hot reload is invalidate-and-make-again: the loader caches nothing.
    let ws = temp_dir("reload");
    fs::write(ws.join("g.json"), r#"{"rule":{"one":{"open":[{"s":[]}]}}}"#).unwrap();
    let entry = entry(json!({
        "name": "r", "languageId": "r", "load": {"spec": "g.json"},
        "_source": "workspace", "_dir": ws,
    }));
    let loader = Loader::new();
    let before = loader.make_instance(&entry).unwrap();
    assert!(before.rule_names().contains(&"one".to_string()));
    fs::write(ws.join("g.json"), r#"{"rule":{"two":{"open":[{"s":[]}]}}}"#).unwrap();
    let after = loader.make_instance(&entry).unwrap();
    let names = after.rule_names();
    assert!(names.contains(&"two".to_string()) && !names.contains(&"one".to_string()));
    let _ = fs::remove_dir_all(ws);
}

// ---------------------------------------------------------------------
// L1 in Rust: the fleet grammars linked in.

#[cfg(feature = "fleet")]
#[test]
fn the_fleet_builds_every_linked_bundled_entry_with_recovery_on() {
    use tabnas_lsp::loaders::{fleet, FLEET_LANGUAGE_IDS};
    use tabnas_lsp::Registry;

    let loader = fleet();
    assert_eq!(loader.linked_names().len(), FLEET_LANGUAGE_IDS.len());
    for id in FLEET_LANGUAGE_IDS {
        let entry = Registry::bundled()
            .entry(id)
            .unwrap_or_else(|| panic!("{id} is in the registry"));
        let parser = loader
            .make_instance(entry)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        assert!(parser.options.parse.recover.enabled, "{id}");
        if let Some(groups) = &entry.sync_groups {
            assert_eq!(&parser.options.parse.recover.sync_groups, groups, "{id}");
        }
    }
    let json = loader
        .make_instance(Registry::bundled().entry("json").unwrap())
        .unwrap();
    assert_eq!(json.parse("[1,2]").unwrap().to_string(), "[1,2]");
}

#[cfg(feature = "fleet")]
#[test]
fn a_fleet_instance_parses_what_its_crate_parses() {
    // The entry's options go on after the crate's make(); they switch
    // recovery on and must leave the grammar's meaning alone.
    use tabnas_lsp::loaders::fleet;
    use tabnas_lsp::Registry;

    let loader = fleet();
    for (id, doc) in [
        ("csv", "a,b\n1,2\n"),
        (
            "feed",
            "<rss version=\"2.0\"><channel><title>t</title></channel></rss>",
        ),
        ("ini", "[a]\nb = c\n"),
        ("json", "{\"a\":[1,true,null]}"),
        ("json5", "{a: 1, b: [2,],}"),
        ("jsonc", "{\"a\": 1 // note\n}"),
        ("jsonic", "a:1, b:x"),
        ("jsonl", "{\"a\":1}\n[2]\n"),
        ("toml", "[a]\nx = 1\n"),
        ("xml", "<a><b>c</b></a>"),
        ("yaml", "a: 1\nb: [x, y]\n"),
        ("zon", ".{ .a = 1 }"),
    ] {
        let entry = Registry::bundled().entry(id).unwrap();
        let direct = loader.linked(id).unwrap()();
        let expected = direct
            .parse(doc)
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let out = loader.make_instance(entry).unwrap().parse_recover(doc);
        assert!(out.errors.is_empty(), "{id}: {:?}", out.errors);
        assert_eq!(
            out.value.map(|v| v.to_string()),
            Some(expected.to_string()),
            "{id}"
        );
    }
}

#[cfg(feature = "fleet")]
#[test]
fn the_fleet_toml_grammar_is_coloured_in_every_string_form() {
    // tabnas-toml reports each string token where the string ENDS, so
    // the token lexed next shadowed it and no TOML string was coloured,
    // in aless's view or in an editor. Every form now is, where it is: a
    // value, an array item, an inline table's value, a header's quoted
    // part, and a quoted key too, since the token cannot tell a key from
    // a value. A bare key stays a variable.
    use tabnas_lsp::loaders::fleet;
    use tabnas_lsp::{analyze, highlight, Doc, Instances, Registry, TokenType};

    let text = concat!(
        "basic = \"b \\\"q\\\"\"\n",
        "literal = 'C:\\path'\n",
        "multi = \"\"\"\nline\"\"\"\n",
        "raw = '''\nline'''\n",
        "arr = [\"a\", 'b']\n",
        "inline = { k = \"v\" }\n",
        "\"quoted\" = 1\n",
        "'lit' = 2\n",
        "[t.\"h\"]\n",
        "x = \"y\"\n",
    );
    let strings = [
        "\"b \\\"q\\\"\"",
        "'C:\\path'",
        "\"\"\"",
        "line\"\"\"",
        "'''",
        "line'''",
        "\"a\"",
        "'b'",
        "\"v\"",
        "\"quoted\"",
        "'lit'",
        "\"h\"",
        "\"y\"",
    ];
    let entry = Registry::bundled().entry("toml").expect("toml is bundled");
    let loader = fleet();

    let result = highlight(
        loader.make_instance(entry).unwrap(),
        text,
        entry.overrides(),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(!result.partial);
    let coloured = |kind: TokenType| -> Vec<&str> {
        result
            .spans
            .iter()
            .filter(|span| span.kind == kind)
            .map(|span| &text[span.start..span.end])
            .collect()
    };
    assert_eq!(coloured(TokenType::String), strings);
    assert_eq!(
        coloured(TokenType::Variable),
        ["basic", "literal", "multi", "raw", "arr", "inline", "k", "t", "x"]
    );

    // The server's path agrees, row and column, in UTF-16 units.
    let mut instances = Instances::new(loader.into_make_instance());
    let inst = instances
        .get(entry, None)
        .unwrap()
        .expect("not quarantined");
    let doc = Doc::new("file:///t.toml", "toml", 1, text);
    let analysis = analyze(&instances, &inst, entry, &doc);
    let tokens = analysis
        .semantic_tokens
        .expect("toml's lex stream is clean");
    assert_eq!(tokens.tokens, result.tokens);
    let first = &tokens.tokens[2];
    assert_eq!(
        (first.row, first.col, first.len, first.kind),
        (0, 8, 9, TokenType::String)
    );
}

#[test]
fn compile_grammar_text_refuses_an_unknown_dialect_before_anything_else() {
    let error = compile_grammar_text(Path::new("g.txt"), "").unwrap_err();
    assert!(error
        .message
        .starts_with("unknown grammar dialect .txt for g.txt"));
}
