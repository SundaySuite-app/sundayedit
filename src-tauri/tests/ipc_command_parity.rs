//! Seam guard: the frontend's IPC calls ↔ the real Rust `#[tauri::command]`
//! signatures.
//!
//! The IPC boundary is a classic seam: the renderer sends a command NAME and a
//! bag of camelCase argument KEYS; Tauri deserializes those keys into the
//! snake_case parameters of a Rust function. Neither side type-checks the
//! other. Rename a Rust parameter (or a command) and **both** test suites stay
//! green — `ipc.contract.test.ts` only pins what ipc.ts SENDS against
//! hand-written expectations living in the same repo, TS-on-TS — while the app
//! fails at runtime with "invalid args" the moment a user clicks the button.
//!
//! This is the third member of the family that closes such seams from the Rust
//! side by parsing the actual TypeScript source:
//!
//!   - `effects_registry_parity.rs`   — effects registry ↔ ffmpeg effects
//!   - `compose_xfade_vocabulary.rs`  — transition picker ↔ ffmpeg `xfade` enum
//!   - `ipc_command_parity.rs`        — this file: the renderer ↔ `generate_handler!`
//!
//! **Scope: the whole of `src/`, not just `ipc.ts`.** `ipc.ts` is the front
//! door (`call<T>("cmd", {…})`), but it is not the only one: `composeEngine.ts`
//! and `timeline/timelineOpsExtra.ts` reach for `invoke()` directly. A guard
//! that only read ipc.ts would leave exactly those calls — the newest, least
//! settled ones — unguarded, which is the failure mode this file exists to
//! prevent. So both shapes are collected from every non-test file under `src/`.
//!
//! What is asserted, per call site found:
//!
//!   1. `cmd` is registered in `tauri::generate_handler!` in `src/lib.rs`.
//!      (An unregistered command is a guaranteed runtime "command not found".)
//!   2. Every argument key is the camelCase of a REAL parameter of that
//!      command's Rust function.
//!   3. Every REQUIRED Rust parameter (i.e. not `Option<…>`, and not a
//!      runtime-injected `tauri::Window` / `tauri::State` / `AppHandle`) is
//!      actually sent. `Option<…>` may be omitted — Tauri deserializes an
//!      absent key as `Value::Null` → `None`.
//!
//! And in the other direction: every command in `generate_handler!` is either
//! reachable from the renderer or sits on the explicit `NO_WRAPPER` allow-list
//! below — plus every `invoke(…)` in `src/` must name its command with a
//! string LITERAL, so nothing can quietly slip past the parser.
//!
//! Deliberately NOT asserted: argument TYPES. The wire types come from ts-rs
//! (`src/lib/bindings`), which is generated from the same Rust structs, so the
//! payload shapes cannot drift the way hand-written names can. Names are the
//! unguarded half; names are what this file guards.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;

/// Commands registered in `generate_handler!` that no frontend code calls.
/// Each one needs a reason — an empty-by-default list is what makes the
/// "no orphan commands" assertion mean something.
const NO_WRAPPER: &[(&str, &str)] = &[
    (
        "sunday_account_status",
        "Sunday Account SSO — backend-only for now; no renderer surface reads \
         the shared session yet (see commands/account.rs).",
    ),
    (
        "sunday_sign_out",
        "Sunday Account SSO — backend-only for now; sign-out has no renderer \
         surface yet (see commands/account.rs).",
    ),
];

/// The one file allowed to `invoke()` a command name held in a variable:
/// `ipc.ts`'s own generic `call<T>(cmd, args)` dispatcher. Everywhere else a
/// dynamic command name would be invisible to this guard.
const DYNAMIC_INVOKE_ALLOWED_IN: &[&str] = &["src/lib/ipc.ts"];

// ── Source access ────────────────────────────────────────────────────────────

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip `//` line and `/* */` block comments while respecting string
/// literals, so a `//` inside a `"…"` (or a `*/` inside a doc comment) cannot
/// confuse the parsers below. Works for both TypeScript and Rust source.
/// Replaces comment bodies with spaces so byte offsets stay meaningful.
fn strip_comments(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            while i < b.len() && !(b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/') {
                out.push(if b[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            // the closing `*/`
            for _ in 0..2 {
                if i < b.len() {
                    out.push(' ');
                    i += 1;
                }
            }
        } else if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                if b[i] == '\\' {
                    i += 1;
                    if i < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                    continue;
                }
                if b[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

// ── 1. What ipc.ts sends ─────────────────────────────────────────────────────

#[derive(Debug)]
struct IpcCall {
    cmd: String,
    keys: Vec<String>,
    /// Repo-relative source file + 1-based line, so a failure points at the
    /// exact call site.
    file: String,
    line: usize,
}

/// Every non-test TypeScript file under `src/`, repo-relative, sorted.
/// Tests are excluded on purpose: they are full of `invoke` MOCKS and
/// `toHaveBeenCalledWith("cmd", …)` assertions, which are not real call sites.
fn frontend_sources() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, root, out);
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let is_ts = name.ends_with(".ts") || name.ends_with(".tsx");
            let is_test = name.contains(".test.") || name.contains(".spec.");
            if is_ts && !is_test {
                let rel = path
                    .strip_prefix(root)
                    .expect("under repo root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &root, &mut out);
    out.sort();
    assert!(
        out.len() > 20,
        "only {} frontend sources found under src/ — the walker is broken",
        out.len()
    );
    out
}

/// Extract every `call<T>("cmd", { key, key: expr, … })` (ipc.ts's typed
/// wrapper) and every direct `invoke<T>("cmd", { … })` from the frontend.
///
/// The argument object is read by brace depth, so only TOP-LEVEL keys are
/// collected (a nested object literal in a value contributes nothing).
fn ipc_calls() -> Vec<IpcCall> {
    let mut calls = Vec::new();
    for file in frontend_sources() {
        calls.extend(calls_in_file(&file));
    }
    // Tripwire against a parser that silently swallows call sites (the one
    // failure mode that would make this whole file pass vacuously). 109 sites
    // at the time of writing; the floor only needs to be close enough to catch
    // a parser that has stopped working. Lower it only alongside a real,
    // deliberate shrink of the command surface — never to make a red run green.
    assert!(
        calls.len() >= 100,
        "the frontend parser found only {} IPC call sites (expected ~109) — the \
         `call<…>(\"cmd\", {{…}})` / `invoke<…>(\"cmd\", {{…}})` shape probably \
         changed, and this parity test is now asserting almost nothing. Fix the \
         parser, do not delete the test.",
        calls.len()
    );
    calls
}

fn calls_in_file(file: &str) -> Vec<IpcCall> {
    let raw = read(file);
    let src = strip_comments(&raw);
    let chars: Vec<char> = src.chars().collect();

    // `call<…>(` / `invoke<…>(` / `invoke(` followed immediately by a string
    // literal. The generic never contains `(`. ipc.ts's own helper definition
    // (`async function call<T>(\n  cmd: string,`) has no string literal after
    // the paren, so it is not matched.
    let re = Regex::new(r#"\b(?:call|invoke)\s*(?:<[^(;{]*>)?\s*\(\s*"([a-z0-9_]+)""#).unwrap();

    let mut calls = Vec::new();
    for m in re.captures_iter(&src) {
        let whole = m.get(0).unwrap();
        let cmd = m.get(1).unwrap().as_str().to_string();
        let line = src[..whole.start()].matches('\n').count() + 1;

        // Continue scanning from just after the closing quote of the command.
        let mut i = src[..whole.end()].chars().count();
        let skip_ws = |i: &mut usize| {
            while *i < chars.len() && chars[*i].is_whitespace() {
                *i += 1;
            }
        };
        skip_ws(&mut i);

        let mut keys = Vec::new();
        if i < chars.len() && chars[i] == ',' {
            i += 1;
            skip_ws(&mut i);
            assert_eq!(
                chars.get(i),
                Some(&'{'),
                "{file}:{line}: the call to `{cmd}` must pass a literal object \
                 of arguments (or nothing at all) so this parity test can read \
                 the keys it sends. Found: {:?}",
                chars.get(i)
            );
            i += 1; // past the opening brace — we are now at depth 1
            let mut depth = 1usize;
            let mut at_element_start = true;
            while i < chars.len() && depth > 0 {
                let c = chars[i];
                match c {
                    '{' | '[' | '(' => {
                        depth += 1;
                        at_element_start = false;
                        i += 1;
                    }
                    '}' | ']' | ')' => {
                        depth -= 1;
                        at_element_start = depth == 1;
                        i += 1;
                    }
                    ',' if depth == 1 => {
                        at_element_start = true;
                        i += 1;
                    }
                    '"' | '\'' | '`' => {
                        // Skip a string literal wholesale (already balanced by
                        // strip_comments, which preserved them verbatim).
                        let quote = c;
                        i += 1;
                        while i < chars.len() {
                            if chars[i] == '\\' {
                                i += 2;
                                continue;
                            }
                            if chars[i] == quote {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                        at_element_start = false;
                    }
                    _ if c.is_whitespace() => i += 1,
                    _ => {
                        if at_element_start && depth == 1 && is_ident_char(c) {
                            let start = i;
                            while i < chars.len() && is_ident_char(chars[i]) {
                                i += 1;
                            }
                            keys.push(chars[start..i].iter().collect::<String>());
                            at_element_start = false;
                        } else {
                            at_element_start = false;
                            i += 1;
                        }
                    }
                }
            }
        }

        calls.push(IpcCall {
            cmd,
            keys,
            file: file.to_string(),
            line,
        });
    }

    calls
}

/// Every `invoke(…)` in the frontend whose command name is NOT a string
/// literal — i.e. invisible to the parser above. Returns `file:line` sites.
fn dynamic_invoke_sites() -> Vec<String> {
    let any = Regex::new(r"\binvoke\s*(?:<[^(;{]*>)?\s*\(").unwrap();
    let literal = Regex::new(r#"\binvoke\s*(?:<[^(;{]*>)?\s*\(\s*""#).unwrap();

    let mut sites = Vec::new();
    for file in frontend_sources() {
        if DYNAMIC_INVOKE_ALLOWED_IN.contains(&file.as_str()) {
            continue;
        }
        let src = strip_comments(&read(&file));
        for m in any.find_iter(&src) {
            if literal.find_at(&src, m.start()).map(|l| l.start()) == Some(m.start()) {
                continue;
            }
            let line = src[..m.start()].matches('\n').count() + 1;
            sites.push(format!("{file}:{line}"));
        }
    }
    sites
}

// ── 2. What Rust registers ───────────────────────────────────────────────────

/// `module::command` entries inside `tauri::generate_handler![…]` in lib.rs,
/// as (command_name, module) pairs.
fn registered_commands() -> BTreeMap<String, String> {
    let raw = read("src-tauri/src/lib.rs");
    let src = strip_comments(&raw);
    let start = src
        .find("generate_handler![")
        .expect("src-tauri/src/lib.rs calls tauri::generate_handler![…]");
    let chars: Vec<char> = src[start..].chars().collect();
    let open = chars.iter().position(|&c| c == '[').unwrap();
    let mut depth = 0usize;
    let mut end = None;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let body: String = chars[open + 1..end.expect("unterminated generate_handler![")]
        .iter()
        .collect();

    let mut out = BTreeMap::new();
    for entry in body.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let segs: Vec<&str> = entry.split("::").map(str::trim).collect();
        assert!(
            segs.len() >= 2,
            "generate_handler! entry {entry:?} is not a `commands::<module>::<fn>` path — \
             teach ipc_command_parity.rs about it."
        );
        let name = segs[segs.len() - 1].to_string();
        let module = segs[segs.len() - 2].to_string();
        assert!(
            out.insert(name.clone(), module).is_none(),
            "{name} is registered twice in generate_handler!"
        );
    }
    assert!(
        out.len() > 50,
        "only {} commands parsed out of generate_handler! — parser broke",
        out.len()
    );
    out
}

#[derive(Debug, Clone)]
struct RustParam {
    /// snake_case name as written in Rust.
    name: String,
    /// `true` when the type is `Option<…>` — the renderer may omit the key.
    optional: bool,
}

/// Parameter types Tauri injects at runtime; they never appear on the wire.
fn is_injected(ty: &str) -> bool {
    let t = ty.trim().trim_start_matches("tauri::");
    let t = t.trim_start_matches("ipc::");
    t.starts_with("Window")
        || t.starts_with("WebviewWindow")
        || t.starts_with("State<")
        || t.starts_with("AppHandle")
        || t.starts_with("Channel<")
}

/// Parse every `#[tauri::command]` function in `src-tauri/src/commands/<module>.rs`
/// into name → wire parameters.
fn rust_commands(module: &str) -> BTreeMap<String, Vec<RustParam>> {
    let raw = read(&format!("src-tauri/src/commands/{module}.rs"));
    let src = strip_comments(&raw);
    let chars: Vec<char> = src.chars().collect();

    let mut out = BTreeMap::new();
    let mut search = 0usize;
    while let Some(rel) = src[search..].find("#[tauri::command]") {
        let attr_end = search + rel + "#[tauri::command]".len();
        search = attr_end;

        // The next `fn ` after the attribute (possibly past `#[cfg(…)]`,
        // `pub`, `async`).
        let fn_rel = src[attr_end..]
            .find("fn ")
            .expect("#[tauri::command] with no following `fn`");
        let mut i = src[..attr_end + fn_rel + 3].chars().count();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < chars.len() && is_ident_char(chars[i]) {
            i += 1;
        }
        let name: String = chars[name_start..i].iter().collect();

        while i < chars.len() && chars[i] != '(' {
            i += 1;
        }
        i += 1; // past `(`
        let args_start = i;
        let mut depth = 1usize;
        while i < chars.len() && depth > 0 {
            match chars[i] {
                '(' | '<' | '[' => depth += 1,
                ')' | '>' | ']' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let args: String = chars[args_start..i - 1].iter().collect();

        let mut params = Vec::new();
        for part in split_top_level(&args) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (pname, ty) = part
                .split_once(':')
                .unwrap_or_else(|| panic!("{module}::{name}: unparsable parameter {part:?}"));
            let ty = ty.trim();
            if is_injected(ty) {
                continue;
            }
            params.push(RustParam {
                name: pname.trim().to_string(),
                optional: ty.starts_with("Option<"),
            });
        }
        out.insert(name, params);
    }
    out
}

/// Split a Rust parameter list on top-level commas — `Vec<(i64, i64)>` and
/// `tauri::State<'_, Ctl>` must stay in one piece.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '<' | '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            '>' | ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

/// `at_word_index` → `atWordIndex`. Mirrors Tauri v2's default argument-name
/// conversion (`rename_all = "camelCase"` on the generated args struct).
fn camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for c in snake.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

// ── The assertions ───────────────────────────────────────────────────────────

#[test]
fn every_ipc_call_matches_a_real_rust_command_signature() {
    let registered = registered_commands();
    let calls = ipc_calls();

    // Parse each command module once.
    let mut by_module: BTreeMap<String, BTreeMap<String, Vec<RustParam>>> = BTreeMap::new();
    for module in registered.values() {
        by_module
            .entry(module.clone())
            .or_insert_with(|| rust_commands(module));
    }

    let mut problems: Vec<String> = Vec::new();

    for call in &calls {
        let Some(module) = registered.get(&call.cmd) else {
            problems.push(format!(
                "{}:{}: `{}` is NOT registered in tauri::generate_handler! (src-tauri/src/lib.rs).\n    \
                 → At runtime this fails with \"command {} not found\". Add \
                 `commands::<module>::{}` to the handler list, or fix the name in the frontend.",
                call.file, call.line, call.cmd, call.cmd, call.cmd
            ));
            continue;
        };
        let cmds = &by_module[module];
        let Some(params) = cmds.get(&call.cmd) else {
            problems.push(format!(
                "{}:{}: `{}` is registered as `commands::{}::{}`, but no \
                 `#[tauri::command] fn {}` exists in \
                 src-tauri/src/commands/{}.rs.",
                call.file, call.line, call.cmd, module, call.cmd, call.cmd, module
            ));
            continue;
        };

        let expected: Vec<String> = params.iter().map(|p| camel(&p.name)).collect();
        let expected_set: BTreeSet<&String> = expected.iter().collect();
        let sent: BTreeSet<&String> = call.keys.iter().collect();

        for key in &call.keys {
            if !expected_set.contains(key) {
                problems.push(format!(
                    "{}:{}: `{}` is sent argument `{}`, which is not a parameter of \
                     `commands::{}::{}`.\n    \
                     Rust takes: [{}]  (snake_case source: [{}])\n    \
                     → Tauri will reject the call with \"invalid args\". Rename the key in \
                     the frontend, or the parameter in Rust — they must agree.",
                    call.file,
                    call.line,
                    call.cmd,
                    key,
                    module,
                    call.cmd,
                    expected.join(", "),
                    params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }
        }

        for p in params {
            let want = camel(&p.name);
            if !p.optional && !sent.contains(&want) {
                problems.push(format!(
                    "{}:{}: `{}` does NOT send required argument `{}` \
                     (Rust `{}: …` in commands::{}::{}).\n    \
                     Sent: [{}]\n    \
                     → Tauri will reject the call with \"invalid args\". Add the key at \
                     the call site, or make the Rust parameter `Option<…>`.",
                    call.file,
                    call.line,
                    call.cmd,
                    want,
                    p.name,
                    module,
                    call.cmd,
                    call.keys.join(", "),
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "\n\nipc.ts ↔ Rust command seam is broken ({} problem(s)):\n\n{}\n",
        problems.len(),
        problems.join("\n\n")
    );
}

#[test]
fn every_registered_command_has_an_ipc_wrapper_or_an_explicit_reason() {
    let registered = registered_commands();
    let wrapped: BTreeSet<String> = ipc_calls().into_iter().map(|c| c.cmd).collect();
    let allowed: BTreeSet<&str> = NO_WRAPPER.iter().map(|(name, _)| *name).collect();

    let orphans: Vec<&String> = registered
        .keys()
        .filter(|c| !wrapped.contains(*c) && !allowed.contains(c.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "\n\nRegistered in generate_handler! but never called from src/: {orphans:?}\n\
         → Either add a call site (a typed `call<…>(\"cmd\", {{…}})` wrapper in \
         src/lib/ipc.ts is the house style), or add the command to NO_WRAPPER in this \
         file WITH the reason it is backend-only.\n"
    );

    // …and the allow-list must not rot: an entry that HAS gained a caller (or
    // was deleted) is stale and hides the next real orphan.
    let stale: Vec<&str> = NO_WRAPPER
        .iter()
        .map(|(name, _)| *name)
        .filter(|n| wrapped.contains(*n) || !registered.contains_key(*n))
        .collect();
    assert!(
        stale.is_empty(),
        "\n\nStale NO_WRAPPER entries in ipc_command_parity.rs: {stale:?}\n\
         → These commands now have a frontend caller (or no longer exist). Remove them \
         from the allow-list so it keeps meaning something.\n"
    );
}

/// The guard reads command names as string LITERALS. An `invoke(someVar, …)`
/// anywhere but ipc.ts's own dispatcher would be a silent hole in it.
#[test]
fn no_frontend_code_invokes_a_command_name_the_guard_cannot_read() {
    let sites = dynamic_invoke_sites();
    assert!(
        sites.is_empty(),
        "\n\n`invoke(…)` with a non-literal command name at: {sites:?}\n\
         → ipc_command_parity.rs matches command names as string literals, so these \
         calls are invisible to it: a renamed Rust parameter behind one of them would \
         ship green. Route the call through `ipc.ts`'s `call()` helper (the one \
         dispatcher allowed to take a variable), or inline the literal name.\n"
    );
}

// ── Parser self-tests ────────────────────────────────────────────────────────
// The parity assertions are only as trustworthy as the two parsers. These pin
// the parsing itself, so a silently-broken parser (which would make the guard
// pass vacuously) fails here instead.

#[test]
fn camel_case_conversion_matches_tauris() {
    assert_eq!(camel("project"), "project");
    assert_eq!(camel("api_key"), "apiKey");
    assert_eq!(camel("at_word_index"), "atWordIndex");
    assert_eq!(camel("min_gap_ms"), "minGapMs");
}

#[test]
fn the_frontend_parser_reads_the_shapes_the_renderer_actually_uses() {
    let calls = ipc_calls();
    let find = |cmd: &str| {
        calls
            .iter()
            .find(|c| c.cmd == cmd)
            .unwrap_or_else(|| panic!("{cmd} not found in any frontend source"))
    };

    // Shorthand properties on one line.
    assert_eq!(
        find("op_split_caption").keys,
        ["project", "captionId", "atWordIndex"]
    );
    // A renamed key (`{ project: proj }`) must yield the WIRE name.
    assert_eq!(find("check_media_paths").keys, ["project"]);
    // A multi-line object with an expression value (`apiKey ?? null`).
    assert_eq!(find("polish_captions").keys, ["project", "model", "apiKey"]);
    // No argument object at all.
    assert!(find("compose_cancel").keys.is_empty());

    // …and the whole point of walking all of src/: raw `invoke` outside ipc.ts
    // is covered too. These two would be invisible to an ipc.ts-only guard.
    let extra = find("op_duplicate_timeline_item");
    assert_eq!(extra.file, "src/features/timeline/timelineOpsExtra.ts");
    assert_eq!(extra.keys, ["project", "itemId"]);
    assert_eq!(
        find("compose_default_encoder").file,
        "src/lib/composeEngine.ts"
    );
}

#[test]
fn the_rust_parser_reads_real_command_signatures() {
    let ops = rust_commands("operations");
    let split = &ops["op_split_caption"];
    assert_eq!(
        split.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["project", "caption_id", "at_word_index"]
    );

    // `tauri::Window` + `tauri::State<'_, …>` are injected, never on the wire —
    // and a generic with a lifetime must not split the parameter list.
    let compose = rust_commands("compose");
    assert_eq!(
        compose["compose_render"]
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["project", "output", "settings"]
    );

    // `Option<…>` is recognised as omittable…
    let polish = rust_commands("polish");
    let api_key = polish["polish_captions"]
        .iter()
        .find(|p| p.name == "api_key")
        .expect("polish_captions takes api_key");
    assert!(api_key.optional);
    // …while a plain type is required.
    assert!(
        !polish["polish_captions"]
            .iter()
            .find(|p| p.name == "project")
            .unwrap()
            .optional
    );

    // A tuple inside a generic must not be split on its inner comma.
    let cleanup = rust_commands("cleanup");
    assert_eq!(
        cleanup["apply_ripple_cuts"]
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["project", "cuts"]
    );
}
