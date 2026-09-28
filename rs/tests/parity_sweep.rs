// The random differential sweep: seeded random documents through the
// canonical TypeScript core (`ts/src/core.js`, run with node) and this
// crate, compared result for result. The conformance fixtures pin chosen
// cases; this finds the ones nobody chose.
//
// Run it by hand (it is ignored in the ordinary suite, because it needs
// node and the ts/ package's engine installed: `cd ts && npm install`):
//
//     cd rs
//     cargo test --test parity_sweep -- --ignored --nocapture
//
// `TABNAS_LSP_SWEEP_N` (default 300) and `TABNAS_LSP_SWEEP_SEED` (default
// 1) choose the sweep; `NODE` names the node binary;
// `TABNAS_LSP_SWEEP_OUT=<file>` writes every case, both runtimes' results
// side by side, as JSON; `TABNAS_LSP_SWEEP_STRICT=1` fails on engine
// divergences too. Progress is a line per 50 documents from each side;
// 300 documents take about ten seconds in a debug build.
//
// The documents come from `tests/parity/ts-core.js`, in six categories
// taken in turn (valid, invalid, multi-error, multi-byte, multi-line and
// comments: the shared grammar keeps the engine's default lexing, so `#`,
// `//` and `/* */` comments, single and backtick quotes and hex numbers
// are all text it reads), under three entries taken in turn (the
// conformance suite's `jsonf`; the same grammar with its own
// `outlineRules` and `semanticTokens` overrides; and the same grammar
// fail-fast, recovery off), with three completion positions each.
//
// Three comparisons are made per document.
//
// 1. END TO END: the two runtimes' diagnostics (every field: code,
//    range, message, source, codeDescription), outline (the whole symbol
//    tree, ranges included), semantic-token data and `failed`, and the
//    completion items at each position (label, kind, detail, insertText,
//    in order), as JSON.
// 2. THE ENGINES: both sides also report the engine's raw view of the
//    parse, before any pipeline code runs, in units neither engine owns
//    (code points): the lex events, the rule events (their indices
//    renumbered, since the engines count from different origins), the
//    errors as each engine serializes them, the recovered value, and the
//    continuations at each position.
// 3. THE PIPELINE ALONE: the TypeScript engine's raw events are fed
//    through THIS crate's pipeline functions (`reconcile` and
//    `semantic_tokens_of`, `outline`, `diagnostics`), which must give the
//    TypeScript results exactly, whether or not the two engines agree on
//    the document.
//
// A difference in (1) where (2) agrees is this port's defect, and so is
// any difference in (3): the test fails on either. A difference in (1)
// where (2) disagrees starts in the engine, which the pipeline cannot and
// must not paper over (AGENTS.md: an engine divergence found here is an
// engine bug there): it is reported, grouped, with the documents that
// reproduce it, for the parser repository, and fails the test only under
// `TABNAS_LSP_SWEEP_STRICT=1`.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value as Json};
use tabnas::{Options, Tabnas, TabnasError};
use tabnas_lsp::analyze::semantic_tokens_of;
use tabnas_lsp::{
    analyze, completion, diagnostics, outline, Doc, Entry, Instances, MakeInstance, Position,
    RuleEvent, RuleEventState, TokenPoint,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
        .to_path_buf()
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// The three entries `ts-core.js` builds (`ENTRIES`), normalized the way
/// the TypeScript registry leaves them. `failfast`'s instance has
/// recovery off (see `make` below).
fn entries() -> HashMap<&'static str, Entry> {
    let mut plain = Entry::new("jsonf");
    plain.language_id = Some("jsonf".into());
    plain.extensions = vec![".jsonf".into()];
    plain.grammar_kind = Some("data".into());

    let mut custom = Entry::new("jsonx");
    custom.language_id = Some("jsonx".into());
    custom.extensions = vec![".jsonx".into()];
    custom.grammar_kind = Some("data".into());
    custom.outline_rules = Some(
        [("pair", "Field"), ("elem", "Item")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    custom.semantic_tokens = Some(
        [("#ST", "property"), ("#VL", "type"), ("#CA", "macro")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    let mut failfast = Entry::new("jsonff");
    failfast.language_id = Some("jsonff".into());
    failfast.extensions = vec![".jsonff".into()];
    failfast.grammar_kind = Some("data".into());
    HashMap::from([("plain", plain), ("custom", custom), ("failfast", failfast)])
}

/// Numbers compare by value: the TypeScript side writes `1000` where
/// serde writes the Rust engine's `1000.0`.
fn normal(value: &Json) -> Json {
    match value {
        Json::Number(n) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| value.clone(), Json::Number),
        Json::Array(items) => Json::Array(items.iter().map(normal).collect()),
        Json::Object(fields) => {
            Json::Object(fields.iter().map(|(k, v)| (k.clone(), normal(v))).collect())
        }
        other => other.clone(),
    }
}

fn same(a: &Json, b: &Json) -> bool {
    normal(a) == normal(b)
}

/// A byte offset as a code-point offset.
fn cp(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    text[..byte].chars().count()
}

/// A token as `ts-core.js` `point` writes it: `[name, si, ri, ci, len,
/// src]`, every offset, column and length in code points.
fn point(text: &str, t: &TokenPoint) -> Json {
    let end = (t.si + t.len).min(text.len());
    let len = if t.si <= end && text.is_char_boundary(t.si) && text.is_char_boundary(end) {
        text[t.si..end].chars().count()
    } else {
        t.src.chars().count()
    };
    json!([t.name, cp(text, t.si), t.ri, t.ci, len, t.src])
}

/// A `point` back as this engine's token: code points to bytes through
/// the document text.
fn token_of(doc: &Doc, p: &Json) -> TokenPoint {
    let text = doc.text.as_str();
    let si = doc.byte_of_scalar(p[1].as_u64().unwrap_or(0) as usize);
    let len: usize = text[si..]
        .chars()
        .take(p[4].as_u64().unwrap_or(0) as usize)
        .map(char::len_utf8)
        .sum();
    TokenPoint {
        name: p[0].as_str().unwrap_or("").to_string(),
        si,
        ri: p[2].as_u64().unwrap_or(0) as usize,
        ci: p[3].as_u64().unwrap_or(0) as usize,
        len,
        src: p[5].as_str().unwrap_or("").to_string(),
    }
}

fn state_name(state: RuleEventState) -> &'static str {
    match state {
        RuleEventState::Open => "o",
        RuleEventState::Close => "c",
    }
}

/// The Rust engine's view of a parse, in the shape `engineView` in
/// `ts-core.js` writes: the raw lex and rule events, the errors and the
/// value, in code points.
fn engine_view(instances: &Instances, inst: &Tabnas, text: &str) -> Json {
    let (recovery, collected) = instances.parse(inst, text);
    let mut errors: Vec<TabnasError> = recovery.errors;
    if let Some(fatal) = recovery.fatal {
        if errors.last() != Some(&fatal) {
            errors.push(fatal);
        }
    }
    let errors: Vec<Json> = errors
        .iter()
        .map(|error| {
            let j = serde_json::to_value(error).expect("an engine error serializes");
            // The Rust engine's `col` and `pos` count scalar values
            // already: code points.
            json!({
                "code": j["code"],
                "message": j["message"],
                "hint": j.get("hint").and_then(Json::as_str).unwrap_or(""),
                "row": j["row"],
                "col": j["col"],
                "pos": j["pos"],
                "len": j["len"],
            })
        })
        .collect();
    let lex: Vec<Json> = collected.lex.iter().map(|t| point(text, t)).collect();
    let rules: Vec<Json> = collected
        .rules
        .iter()
        .map(|e| {
            json!([
                e.i,
                e.name,
                state_name(e.state),
                e.forced,
                e.r,
                e.o0.as_ref().map(|t| point(text, t)),
                e.c0.as_ref().map(|t| point(text, t)),
            ])
        })
        .collect();
    let value = recovery
        .value
        .as_ref()
        .map_or(Json::Null, tabnas::Value::to_json);
    json!({ "value": value, "errors": errors, "lex": lex, "rules": rules })
}

/// This crate's pipeline over the TYPESCRIPT engine's raw events: the
/// diagnostics, outline and semantic-token data the Rust functions derive
/// from exactly the input the canonical pipeline derived its own from.
/// Whatever the two engines do, these must equal the TypeScript results.
fn pipeline_over(ts_engine: &Json, entry: &Entry, doc: &Doc) -> Json {
    let lex: Vec<TokenPoint> = ts_engine["lex"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| token_of(doc, p))
        .collect();
    let rules: Vec<RuleEvent> = ts_engine["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| RuleEvent {
            i: e[0].as_u64().unwrap_or(0) as usize,
            name: e[1].as_str().unwrap_or("").to_string(),
            state: if e[2] == "o" {
                RuleEventState::Open
            } else {
                RuleEventState::Close
            },
            forced: e[3].as_bool().unwrap_or(false),
            r: e[4].as_str().unwrap_or("").to_string(),
            o0: (!e[5].is_null()).then(|| token_of(doc, &e[5])),
            c0: (!e[6].is_null()).then(|| token_of(doc, &e[6])),
        })
        .collect();
    let errors: Vec<TabnasError> = ts_engine["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let n = |k: &str| e[k].as_u64().unwrap_or(0) as usize;
            let mut error = TabnasError::new(
                e["code"].as_str().unwrap_or(""),
                "",
                doc.text.as_str(),
                n("pos"),
                n("row"),
                n("col"),
            );
            error.detail = e["message"].as_str().unwrap_or("").to_string();
            error.hint = e["hint"].as_str().unwrap_or("").to_string();
            error.len = n("len");
            error
        })
        .collect();
    json!({
        "diagnostics": diagnostics(&errors, Some(entry), doc),
        "outline": outline(&rules, Some(entry), doc),
        "data": entry.is_clean().then(|| semantic_tokens_of(&lex, entry, doc).data),
    })
}

/// An engine view with its rule indices renumbered in order of first
/// appearance. The index only pairs a rule's open event with its close,
/// and the two engines count from different origins (TypeScript's first
/// rule is 1, Rust's 0), so the pairing is compared and the origin is
/// not.
fn renumbered(view: &Json) -> Json {
    let mut view = view.clone();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    if let Some(rules) = view["rules"].as_array_mut() {
        for rule in rules {
            if let Some(i) = rule[0].as_u64() {
                let next = seen.len();
                rule[0] = json!(*seen.entry(i).or_insert(next));
            }
        }
    }
    view
}

/// The keys of two JSON objects whose values differ.
fn differing_keys(a: &Json, b: &Json) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = a
        .as_object()
        .into_iter()
        .chain(b.as_object())
        .flat_map(|o| o.keys())
        .collect();
    keys.into_iter()
        .filter(|k| !same(&a[k.as_str()], &b[k.as_str()]))
        .cloned()
        .collect()
}

fn short(value: &Json) -> String {
    let s = value.to_string();
    if s.chars().count() > 600 {
        format!("{}...", s.chars().take(600).collect::<String>())
    } else {
        s
    }
}

#[derive(Default)]
struct Tally {
    documents: usize,
    positions: usize,
    analyze_agree: usize,
    pipeline_agree: usize,
    completion_agree: usize,
    /// (kind, document, detail): mismatches where the engines agree.
    port: Vec<(String, String, String)>,
    /// kind -> (count, first reproductions).
    engine: BTreeMap<String, (usize, Vec<String>)>,
}

impl Tally {
    fn engine(&mut self, kind: String, repro: String) {
        let slot = self.engine.entry(kind).or_default();
        slot.0 += 1;
        if slot.1.len() < 3 {
            slot.1.push(repro);
        }
    }
}

#[test]
#[ignore = "needs node and ts/node_modules; run by hand, see the module docs"]
fn rust_matches_the_typescript_core_on_random_documents() {
    let node = env_or("NODE", "node");
    let n = env_or("TABNAS_LSP_SWEEP_N", "300");
    let seed = env_or("TABNAS_LSP_SWEEP_SEED", "1");
    let script = root().join("rs/tests/parity/ts-core.js");
    let started = Instant::now();
    eprintln!("parity sweep: {n} documents, seed {seed}: the TypeScript core first");
    let output = match Command::new(&node)
        .arg(&script)
        .arg(&n)
        .arg(&seed)
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("cannot run {node} ({error}): the sweep needs node"),
    };
    assert!(
        output.status.success(),
        "{} failed ({}); is ts/node_modules installed (cd ts && npm install)?",
        script.display(),
        output.status
    );
    let ts: Json = serde_json::from_slice(&output.stdout).expect("ts-core.js writes JSON");
    let cases = ts["cases"].as_array().expect("cases").clone();
    let ts_engine = ts["engine"].as_str().unwrap_or("?").to_string();
    eprintln!(
        "parity sweep: engines: TypeScript {ts_engine}, Rust {}",
        tabnas::VERSION
    );
    if ts_engine != tabnas::VERSION {
        eprintln!(
            "parity sweep: WARNING the two engines are different releases, so an engine \
             divergence may be the release gap"
        );
    }

    let spec = std::fs::read_to_string(root().join("test/fixtures/json-grammar.json"))
        .expect("the shared grammar");
    let make: MakeInstance = Arc::new(move |entry: &Entry| {
        let mut options = Options::default();
        options.parse.recover.enabled = entry.language_id() != "jsonff";
        let mut parser = Tabnas::with_options(options);
        parser
            .grammar_json(&spec)
            .map_err(|e| tabnas_lsp::LoadError::new(e.to_string()))?;
        Ok(parser)
    });
    let mut instances = Instances::new(make);
    let entries = entries();
    let mut insts = HashMap::new();
    for (name, entry) in &entries {
        let inst = instances
            .get(entry, None)
            .expect("the shared grammar loads")
            .expect("not quarantined");
        insts.insert(*name, inst);
    }

    let mut tally = Tally::default();
    let mut dump = Vec::new();
    for (k, case) in cases.iter().enumerate() {
        let which = case["entry"].as_str().expect("entry");
        let entry = &entries[which];
        let inst = &insts[which];
        let text = case["text"].as_str().expect("text");
        let lang = entry.language_id().to_string();
        let doc = Doc::new(format!("file:///t.{lang}"), lang, 1, text);
        let repro = serde_json::to_string(text).unwrap_or_default();

        // The LSP results.
        let analysis = analyze(&instances, inst, entry, &doc);
        let rs_lsp = json!({
            "failed": analysis.failed,
            "diagnostics": analysis.diagnostics,
            "outline": analysis.outline,
            "data": analysis.semantic_tokens.as_ref().map(|t| t.data.clone()),
        });
        let positions: Vec<Position> =
            serde_json::from_value(case["positions"].clone()).expect("positions");
        let rs_completions: Vec<Json> = positions
            .iter()
            .map(|&p| json!(completion(Some(&instances), inst, entry, &doc, p)))
            .collect();

        // The engine's view.
        let rs_engine = engine_view(&instances, inst, text);
        let rs_continuations: Vec<Json> = positions
            .iter()
            .map(|&p| {
                let prefix = &text[..doc.offset_at(p)];
                instances.with_parse_lock(|| {
                    catch_unwind(AssertUnwindSafe(|| inst.continuations(prefix)))
                        .map_or(Json::Null, |c| json!(c.tokens))
                })
            })
            .collect();

        tally.documents += 1;
        let engine_diff = differing_keys(&renumbered(&case["engine"]), &renumbered(&rs_engine));
        let ts_lsp = &case["lsp"];

        // The pipeline alone: this crate's functions over the TypeScript
        // engine's own events must give the TypeScript results exactly,
        // whether or not the two engines agree on this document.
        let cross = pipeline_over(&case["engine"], entry, &doc);
        if ts_lsp.get("threw").is_none() {
            let wanted = json!({
                "diagnostics": ts_lsp["diagnostics"],
                "outline": ts_lsp["outline"],
                "data": ts_lsp["data"],
            });
            if same(&wanted, &cross) {
                tally.pipeline_agree += 1;
            } else {
                for key in differing_keys(&wanted, &cross) {
                    tally.port.push((
                        format!("pipeline {key} over the TypeScript engine's events"),
                        repro.clone(),
                        format!(
                            "\n      typescript: {}\n      rust:       {}",
                            short(&wanted[key.as_str()]),
                            short(&cross[key.as_str()])
                        ),
                    ));
                }
            }
        }
        if ts_lsp.get("threw").is_some() {
            tally.engine(
                "the TypeScript core threw".into(),
                format!("{repro}: {}", ts_lsp["threw"]),
            );
        } else if same(ts_lsp, &rs_lsp) {
            tally.analyze_agree += 1;
            if !engine_diff.is_empty() {
                tally.engine(
                    format!(
                        "engine view differs ({}), LSP results agree",
                        engine_diff.join(", ")
                    ),
                    repro.clone(),
                );
            }
        } else {
            let lsp_diff = differing_keys(ts_lsp, &rs_lsp);
            if engine_diff.is_empty() {
                for key in &lsp_diff {
                    tally.port.push((
                        format!("analyze {key}"),
                        repro.clone(),
                        format!(
                            "\n      typescript: {}\n      rust:       {}",
                            short(&ts_lsp[key.as_str()]),
                            short(&rs_lsp[key.as_str()])
                        ),
                    ));
                }
            } else {
                let (ts_view, rs_view) = (renumbered(&case["engine"]), renumbered(&rs_engine));
                let mut detail = String::new();
                for key in &engine_diff {
                    let _ = write!(
                        detail,
                        "\n      {key}: typescript {}\n      {key}: rust       {}",
                        short(&ts_view[key.as_str()]),
                        short(&rs_view[key.as_str()])
                    );
                }
                tally.engine(
                    format!(
                        "engine view differs ({}), so do the LSP {}",
                        engine_diff.join(", "),
                        lsp_diff.join(", ")
                    ),
                    format!("{repro}{detail}"),
                );
            }
        }

        for (i, position) in positions.iter().enumerate() {
            tally.positions += 1;
            let ts_items = &case["completions"][i];
            let ts_cont = &case["continuations"][i];
            if same(ts_items, &rs_completions[i]) {
                tally.completion_agree += 1;
                continue;
            }
            let at = format!("{repro} at {}:{}", position.line, position.character);
            if same(ts_cont, &rs_continuations[i]) {
                tally.port.push((
                    "completion".into(),
                    at,
                    format!(
                        "\n      typescript: {}\n      rust:       {}",
                        short(ts_items),
                        short(&rs_completions[i])
                    ),
                ));
            } else {
                tally.engine(
                    "continuations differ, so do the completion items".into(),
                    format!(
                        "{at}\n      typescript: {}\n      rust:       {}",
                        short(ts_cont),
                        short(&rs_continuations[i])
                    ),
                );
            }
        }

        if std::env::var_os("TABNAS_LSP_SWEEP_OUT").is_some() {
            dump.push(json!({
                "case": case,
                "rust": {
                    "lsp": rs_lsp, "completions": rs_completions,
                    "engine": rs_engine, "continuations": rs_continuations,
                    "pipeline": cross,
                },
            }));
        }
        if (k + 1) % 50 == 0 || k + 1 == cases.len() {
            eprintln!(
                "parity sweep: {} of {} ({}%), {:.1} s",
                k + 1,
                cases.len(),
                100 * (k + 1) / cases.len().max(1),
                started.elapsed().as_secs_f64()
            );
        }
    }

    if let Some(out) = std::env::var_os("TABNAS_LSP_SWEEP_OUT") {
        std::fs::write(&out, serde_json::to_string(&dump).expect("dump"))
            .expect("TABNAS_LSP_SWEEP_OUT is writable");
    }

    let engine_total: usize = tally.engine.values().map(|(n, _)| n).sum();
    eprintln!(
        "\nparity sweep: {} documents, {} completion positions; analyses agree end to end on \
         {}, over the same engine events on {}; completions agree on {}; {} port mismatches; \
         {} engine divergences",
        tally.documents,
        tally.positions,
        tally.analyze_agree,
        tally.pipeline_agree,
        tally.completion_agree,
        tally.port.len(),
        engine_total
    );
    for (kind, (count, repros)) in &tally.engine {
        eprintln!("\n  engine divergence x{count}: {kind}");
        for repro in repros {
            eprintln!("    {repro}");
        }
    }
    for (kind, repro, detail) in &tally.port {
        eprintln!("\n  PORT MISMATCH, {kind}: {repro}{detail}");
    }
    assert!(
        tally.port.is_empty(),
        "{} results differ where the two engines agree: port defects",
        tally.port.len()
    );
    if std::env::var("TABNAS_LSP_SWEEP_STRICT").as_deref() == Ok("1") {
        assert_eq!(engine_total, 0, "engine divergences (strict mode)");
    }
}
