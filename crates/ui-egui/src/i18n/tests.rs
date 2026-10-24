use std::collections::BTreeSet;

use super::*;
use crate::{CadApp, Services};
use cadcraft_engine::Session;
use serde_json::json;

fn placeholders(text: &str) -> BTreeSet<&str> {
    text.split('{').skip(1).filter_map(|s| s.split_once('}').map(|(key, _)| key)).collect()
}

#[test]
fn catalog_is_valid_and_preserves_placeholders_and_ellipses() {
    for language in &LANGUAGES {
        let (catalog, errors) = parse(language.source);
        assert!(errors.is_empty(), "{}: {errors:?}", language.code);
        for ((context, key), value) in catalog {
            assert!(["", "menu", "@plural"].contains(&context), "unknown context: {context}");
            let forms: Vec<_> = if context == "@plural" { value.split('|').collect() } else { vec![value] };
            if context == "@plural" {
                assert_eq!(forms.len(), language.plural_forms, "{}: {key}", language.code);
            }
            for form in forms {
                // A translation may leave out an English plural ending ("object{0}" with "s").
                let ending = |p: &&str| key.split(&format!("{{{p}}}")).next().is_some_and(|b| b.ends_with(|c: char| c.is_ascii_alphabetic()));
                let dropped: Vec<_> = placeholders(key).difference(&placeholders(form)).copied().collect();
                assert!(placeholders(form).is_subset(&placeholders(key)) && dropped.iter().all(ending), "{key}");
                for suffix in ["...", "…"] {
                    assert_eq!(key.ends_with(suffix), form.ends_with(suffix), "{key}");
                }
            }
        }
    }
    let (parsed, errors) = parse("broken\n\tOpen\tВідкрити\n\tOpen\tВідкрити\n\tEmpty\t\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(errors.len(), 3);
    assert_eq!(tr("uk", "a future untranslated label"), "a future untranslated label");
    assert_eq!(tr("unknown", "Open"), "Open");
}

/// Ukrainian is the reference translation and must cover everything. Other languages fall back
/// to English for what they don't cover yet; their gaps are reported, not enforced, so adding a
/// command doesn't require a row in every language.
const REQUIRED: &[&str] = &["uk"];
const REPORTED: &[&str] = &["pt-BR", "de", "es", "cs", "ru", "fr", "zh", "it", "ko", "sv", "ar"];

fn check(language: &str, what: &str, missing: Vec<String>) {
    if REQUIRED.contains(&language) {
        assert!(missing.is_empty(), "{language}: missing {what}: {missing:?}");
    } else if !missing.is_empty() {
        eprintln!("{language}: {} {what} fall back to English: {missing:?}", missing.len());
    }
}

#[test]
fn translations_cover_commands_menus_tools_and_snap_labels() {
    for language in REQUIRED.iter().chain(REPORTED) {
        check(language, "translations", missing_commands_menus_tools_and_snap_labels(language));
    }
}

fn missing_commands_menus_tools_and_snap_labels(language: &str) -> Vec<String> {
    let catalog = catalog(language).unwrap();
    let mut sources = BTreeSet::new();
    sources.extend(crate::menus::MENUS.iter().copied());
    for c in cadcraft_engine::command_specs() {
        sources.insert(c.label);
        sources.extend(c.menu.iter().copied());
    }
    for (_, label, path, _) in crate::menus::UI_COMMANDS {
        sources.insert(*label);
        sources.extend(path.iter().copied());
    }
    for groups in [crate::palettes::DRAFTING, crate::palettes::MODELING] {
        for (name, large, small) in groups {
            sources.insert(*name);
            sources.extend(large.iter().chain(*small).map(|(_, _, label)| *label));
        }
    }
    sources.extend(cadcraft_engine::snap::mode::ALL.iter().map(|(_, name)| *name));
    sources.into_iter().filter(|s| !s.is_empty() && !catalog.contains_key(&("", *s))).map(str::to_string).collect()
}

#[test]
fn every_marked_ui_literal_has_a_translation() {
    for language in REQUIRED.iter().chain(REPORTED) {
        check(language, "UI translations", missing_marked_ui_literals(language).into_iter().collect());
    }
}

fn missing_marked_ui_literals(language: &str) -> BTreeSet<String> {
    let catalog = catalog(language).unwrap();
    let sources = [
        include_str!("../xrefs.rs"),
        include_str!("../canvas.rs"),
        include_str!("../chrome.rs"),
        include_str!("../cmdline.rs"),
        include_str!("../credits.rs"),
        include_str!("../palettes/quickcalc.rs"),
        include_str!("../palettes/patterns.rs"),
        include_str!("../dialogs.rs"),
        include_str!("../hatchdlg.rs"),
        include_str!("../mtext_editor.rs"),
        include_str!("../layers.rs"),
        include_str!("../canvas/compare.rs"),
        include_str!("../canvas/blockedit.rs"),
        include_str!("../canvas/tiles.rs"),
        include_str!("../canvas/gumball.rs"),
        include_str!("../viewcube.rs"),
        include_str!("../visualstyles.rs"),
        include_str!("../bedit.rs"),
        include_str!("../palettes.rs"),
        include_str!("../parametric.rs"),
        include_str!("../quick.rs"),
        include_str!("../vpscale.rs"),
        include_str!("../options/dialog.rs"),
        include_str!("../options/ai.rs"),
        include_str!("../assistant.rs"),
        include_str!("../managers/current.rs"),
        include_str!("../managers/dimstyle.rs"),
        include_str!("../managers/find.rs"),
        include_str!("../managers/layerstates.rs"),
        include_str!("../managers/mlstyle.rs"),
        include_str!("../managers/wblock.rs"),
        include_str!("../managers/dwgprops.rs"),
        include_str!("../managers/pastespec.rs"),
        include_str!("../managers/filter.rs"),
        include_str!("../managers/laywalk.rs"),
        include_str!("../managers/laytrans.rs"),
        include_str!("../chrome/annoscale.rs"),
        include_str!("../context_menu.rs"),
        include_str!("../plotstyles.rs"),
        include_str!("../spacemouse.rs"),
        include_str!("../designcenter.rs"),
        include_str!("../managers/ucsman.rs"),
        include_str!("../render_window.rs"),
    ];
    let mut missing = BTreeSet::new();
    for source in sources {
        for marker in ["crate::tl!(", "crate::tf!("] {
            for rest in source.split(marker).skip(1) {
                // These macros require a quoted literal. Use JSON's string decoding for escapes.
                let rest = rest.trim_start();
                let mut escape = false;
                let end = rest
                    .char_indices()
                    .skip(1)
                    .find_map(|(i, c)| {
                        if escape {
                            escape = false;
                            None
                        } else if c == '\\' {
                            escape = true;
                            None
                        } else if c == '"' {
                            Some(i + 1)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                let key: String = serde_json::from_str(&rest[..end]).unwrap();
                // Coordinate axes, glyph buttons and product names are not translated.
                if !["X", "Y", "B", "Cadino"].contains(&key.as_str()) && !catalog.contains_key(&("", key.as_str())) {
                    missing.insert(key);
                }
            }
        }
    }
    missing
}

/// Everything the engine (and the UI) puts on the command line in English: prompt messages,
/// echoes, refusals and results reach Korean and Ukrainian users translated ([`line`]). Adding a
/// prompt or a message means adding its ko and uk rows; this test lists the missing ones as
/// catalog rows to fill in.
#[test]
fn engine_prompts_and_messages_are_translated() {
    let messages = command_line_messages();
    assert!(messages.len() > 1000, "the scanner found only {} messages", messages.len());
    for must in ["Specify first point", "Command: {0}", "Select objects", "*Cancel*", "nothing selected"] {
        assert!(messages.contains_key(must), "the scanner missed {must:?}");
    }
    // The 3D kernel's Rust errors and refusals, its C++ failures (a concatenation as a template)
    // and operation names, a `map_err` refusal and a result message set by index.
    for must in [
        "the modelling kernel failed: {why}",
        "the box has no volume",
        "{what} left nothing",
        "rounding the edges",
        "Parameters, actions and visibility states are added in the Block Editor (BEDIT).",
        "Added {kind} \"{found}\".",
    ] {
        assert!(messages.contains_key(must), "the scanner missed {must:?}");
    }
    let mut report = String::new();
    for language in ["ko", "uk"] {
        let catalog = catalog(language).unwrap();
        let missing: Vec<_> = messages.iter().filter(|(key, _)| !catalog.contains_key(&("", key.as_str()))).collect();
        if !missing.is_empty() {
            report.push_str(&format!("\n{language}.tsv is missing {} row(s):\n", missing.len()));
            for (key, file) in missing {
                report.push_str(&format!("\t{key}\t\t# {file}\n"));
            }
        }
    }
    assert!(report.is_empty(), "command-line text without a translation (add the rows):{report}");
}

/// English that stays English on the command line: a keyword, a command name, the xref and image
/// lists' aligned column headings and a plural ending a message inserts.
const ENGLISH_ONLY: &[&str] = &["Current", "MSPACE", "Xref name", "Image name", "Refs", "{0} {1} {2} Path", "s were", "ZNONe"];

/// The command-line text in the engine's and the UI's sources, as catalog keys (a `format!`
/// string with its `{}` numbered `{0}`, `{1}`…, as [`line`] matches it), with a file it is in.
/// It is what the sources pass to `Prompt::new`, `echo`, `EngineError::Other`, the `other(…)`
/// helpers, `Err(…)` and `map_err(|_| …)`, and a result's `"message"` (also set as
/// `r["message"] = json!(…)`); tests are left out.
fn command_line_messages() -> std::collections::BTreeMap<String, String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    let mut dirs = vec![root.join("engine/src"), root.join("ui-egui/src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" {
                    dirs.push(path);
                }
            } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
                files.push(path);
            }
        }
    }
    let mut found = std::collections::BTreeMap::new();
    for path in files {
        let file = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let ui = file.starts_with("ui-egui/");
        let text = std::fs::read_to_string(&path).unwrap();
        let src = without_tests(&text);
        let sinks: &[&str] = if ui {
            &[".echo("]
        } else {
            &["Prompt::new(", ".echo(", "EngineError::Other(", "other(", "Err(", "\"message\":", "[\"message\"] = json!(", "map_err(|_| "]
        };
        for sink in sinks {
            let mut from = 0;
            while let Some(at) = src[from..].find(sink).map(|i| i + from) {
                from = at + sink.len();
                let before = src[..at].chars().next_back().unwrap_or(' ');
                let after = src[from..].trim_start();
                if *sink == "other(" && (before.is_alphanumeric() || "_.:".contains(before)) {
                    continue;
                }
                if *sink == "Err(" && !(after.starts_with('"') || after.starts_with("format!(")) {
                    continue;
                }
                for literal in literals(&src[from..], *sink == "\"message\":") {
                    for line in literal.split('\n') {
                        if prose(line) && !ENGLISH_ONLY.contains(&line) {
                            found.entry(line.to_string()).or_insert_with(|| file.clone());
                        }
                    }
                }
            }
        }
    }
    for (key, file) in kernel_messages(&root) {
        found.entry(key).or_insert(file);
    }
    found
}

/// The 3D kernel's messages, which reach the command line as the engine's errors: its Rust
/// refusals (`KernelError::Invalid`, `bad(…)`), its failures and error texts, and what its C++
/// operations throw (`fail(…)`, whose `a + "b"` concatenations become `{a}b` templates) and the
/// operation names they report with (`finish(…, "rounding the edges")`).
fn kernel_messages(root: &std::path::Path) -> Vec<(String, String)> {
    let mut files = vec![root.join("kernel/occt/src/ops.cpp")];
    let mut dirs = vec![root.join("kernel/src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let (path, name) = (entry.path(), entry.file_name().to_string_lossy().to_string());
            if path.is_dir() {
                dirs.push(path);
            } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
                files.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for path in files {
        let file = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let cpp = file.ends_with(".cpp");
        let text = std::fs::read_to_string(&path).unwrap();
        let src = if cpp { text.as_str() } else { without_tests(&text) };
        let sinks: &[&str] =
            if cpp { &["fail(", "finish(", "return \""] } else { &["KernelError::Invalid(", "KernelError::Failed(", "bad(", "#[error("] };
        for sink in sinks {
            let mut from = 0;
            while let Some(at) = src[from..].find(sink).map(|i| i + from) {
                from = at + sink.len();
                let before = src[..at].chars().next_back().unwrap_or(' ');
                if ["bad(", "fail(", "finish("].contains(sink) && (before.is_alphanumeric() || "_.:".contains(before)) {
                    continue;
                }
                let keys = match *sink {
                    "return \"" => literal(&src[from - 1..]).map(|(v, _)| v).into_iter().collect(),
                    "fail(" => cpp_keys(&src[from..]),
                    _ => literals(&src[from..], false),
                };
                out.extend(keys.into_iter().filter(|k| prose(k)).map(|k| (k, file.clone())));
            }
        }
    }
    out
}

/// The message keys of the C++ call argument at the start of `src`: a concatenation `a + "b" +
/// f(c)` as the template `{a}b{0}` (a plain name keeps its name, so `{what}` is translated in
/// turn), else its string literals (`x ? "one" : "other"`).
fn cpp_keys(src: &str) -> Vec<String> {
    let (mut depth, mut pieces, mut piece, mut chars) = (1usize, Vec::new(), String::new(), src.char_indices().peekable());
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                let Some((_, len)) = literal(&src[i..]) else { break };
                piece.push_str(&src[i..i + len]);
                while chars.peek().is_some_and(|(j, _)| *j < i + len) {
                    chars.next();
                }
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            '+' if depth == 1 => {
                pieces.push(std::mem::take(&mut piece));
                continue;
            }
            _ => {}
        }
        piece.push(c);
    }
    pieces.push(piece);
    if pieces.len() == 1 {
        return literals(src, false);
    }
    let mut key = String::new();
    let mut n = 0;
    for p in pieces.iter().map(|p| p.trim()) {
        if p.starts_with('"') {
            key.push_str(&literal(p).map(|(v, _)| v).unwrap_or_default());
        } else if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            key.push_str(&format!("{{{p}}}"));
        } else {
            key.push_str(&format!("{{{n}}}"));
            n += 1;
        }
    }
    vec![key]
}

/// A source file up to its `#[cfg(test)] mod …`.
fn without_tests(src: &str) -> &str {
    let lines: Vec<&str> = src.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let module = lines.iter().skip(i + 1).take(2).any(|n| n.trim_start().starts_with("mod ") || n.trim_start().starts_with("pub mod "));
        if l.trim() == "#[cfg(test)]" && module {
            let offset = l.as_ptr() as usize - src.as_ptr() as usize;
            return &src[..offset];
        }
    }
    src
}

/// Text a user reads: a phrase or a capitalised word, not a key, an id or bare punctuation.
fn prose(line: &str) -> bool {
    let mut bare = String::new();
    let mut inside = false;
    for c in line.chars() {
        match c {
            '{' => inside = true,
            '}' => inside = false,
            c if !inside => bare.push(c),
            _ => {}
        }
    }
    let bare = bare.trim();
    let letters = bare.chars().collect::<Vec<_>>().windows(2).any(|w| w.iter().all(|c| c.is_ascii_alphabetic()));
    letters && !line.contains('\t') && (bare.contains(' ') || bare.chars().find(|c| c.is_alphabetic()).is_some_and(|c| c.is_ascii_uppercase()))
}

/// The string literals of the call argument (or, with `field`, the JSON value) at the start of
/// `src`, a `format!` string as a catalog key.
fn literals(src: &str, field: bool) -> Vec<String> {
    let b = src.as_bytes();
    let (mut depth, mut i, mut out) = (1usize, 0usize, Vec::new());
    while let Some(&c) = b.get(i) {
        let ident = i > 0 && b.get(i - 1).is_some_and(|p| p.is_ascii_alphanumeric() || *p == b'_');
        let raw = c == b'r' && matches!(b.get(i + 1), Some(b'"' | b'#')) && !ident;
        if c == b'"' || raw {
            let Some((value, end)) = literal(&src[i..]) else { break };
            let format = src[..i].trim_end().ends_with("format!(");
            out.push(if format { format_key(&value) } else { value });
            i += end;
            continue;
        }
        match c {
            b'\'' => {
                // A char literal ('"', '\'', '{'); a lifetime has no closing quote.
                let close = if b.get(i + 1) == Some(&b'\\') { src.get(i + 2..).and_then(|s| s.find('\'')).map(|e| e + 2) } else { Some(2) };
                if let Some(e) = close.filter(|e| b.get(i + e) == Some(&b'\'')) {
                    i += e;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => i += src[i..].find('\n').unwrap_or(src.len() - i),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b',' if field && depth == 1 => break,
            _ => {}
        }
        i += 1;
    }
    out
}

/// A string literal at the start of `src` (`"…"`, `r"…"`, `r#"…"#`): its value and length.
fn literal(src: &str) -> Option<(String, usize)> {
    if let Some(raw) = src.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let open = 1 + hashes + 1;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = src.get(open..)?.find(&close)? + open;
        return Some((src[open..end].to_string(), end + close.len()));
    }
    let mut value = String::new();
    let mut chars = src.char_indices().skip(1).peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((value, i + 1)),
            '\\' => match chars.next()?.1 {
                'n' => value.push('\n'),
                't' => value.push('\t'),
                'r' => value.push('\r'),
                '0' => value.push('\0'),
                'u' => {
                    let hex: String = chars.by_ref().map(|(_, c)| c).skip(1).take_while(|c| *c != '}').collect();
                    value.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                'x' => {
                    let hex: String = chars.by_ref().take(2).map(|(_, c)| c).collect();
                    value.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                '\n' | '\r' => {
                    // A line continuation drops the next line's indentation.
                    while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
                        chars.next();
                    }
                }
                other => value.push(other),
            },
            c => value.push(c),
        }
    }
    None
}

/// A `format!` string as a catalog key: `{}` and `{:…}` numbered `{0}`, `{1}`…, a named
/// argument's format spec dropped, `{{`/`}}` single braces.
fn format_key(fmt: &str) -> String {
    let (mut out, mut next, mut rest) = (String::new(), 0, fmt);
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
        } else if let Some(end) = tail.strip_prefix('{').and_then(|t| t.find('}')) {
            let name = tail[1..=end].split(':').next().unwrap_or("");
            if name.is_empty() {
                out.push_str(&format!("{{{next}}}"));
                next += 1;
            } else {
                out.push_str(&format!("{{{name}}}"));
            }
            rest = &tail[end + 2..];
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn the_message_scanner_reads_rust_literals() {
    assert_eq!(format_key("{} of {n:.2} in {:?} {{x}}"), "{0} of {n} in {1} {x}");
    assert_eq!(literal("\"a \\\"b\\\" \\u{e9}\\\n        c\" tail"), Some(("a \"b\" éc".to_string(), 27)));
    assert_eq!(literal("r#\"say \"hi\"\"# x"), Some(("say \"hi\"".to_string(), 13)));
    let src = "if self.x { \"Specify one\" } else { format!(\"Got {} of {n}\", a.join(\", \")) }, '\"', \"{\"); next(\"no\")";
    assert_eq!(literals(src, false), ["Specify one", "Got {0} of {n}", ", ", "{"]);
    assert_eq!(literals(" format!(\"{n} done\"), \"other\": \"x\" }", true), ["{n} done"]);
    assert!(prose("Specify first point") && prose("Constant?") && prose("{0} object(s) found") && prose("*Cancel*"));
    assert!(!prose("path") && !prose("{0}") && !prose(", ") && !prose("x\ty"));
    assert_eq!(without_tests("a\n#[cfg(test)]\nmod tests {\nb"), "a\n");
    assert_eq!(cpp_keys("what + \" left nothing\");"), ["{what} left nothing"]);
    assert_eq!(cpp_keys("\"no face \" + std::to_string(i) + \" (of \" + std::to_string(n) + \")\");"), ["no face {0} (of {1})"]);
    assert_eq!(cpp_keys("plus ? \"inside\" : \"on no face\");"), ["inside", "on no face"]);
}

/// History lines are English in the log and translated as they are drawn: a whole-line row, a
/// `{placeholder}` row filled in (a reason or a purpose translated in turn, a name kept), and
/// anything unknown as it is.
#[test]
fn history_lines_are_translated_as_drawn() {
    let cases = [
        ("Command: LINE", "명령: LINE", "Команда: LINE"),
        ("*Cancel*", "*취소*", "*Скасовано*"),
        ("Current osnap modes: End,Mid", "현재 객체 스냅 모드: End,Mid", "Поточні режими об'єктної прив'язки: End,Mid"),
        ("3 objects changed to layer \"Layer\".", "객체 3개가 도면층 \"Layer\"(으)로 변경되었습니다.", "Переміщено на шар \"Layer\" об'єктів: 3."),
        (
            "command `erase` is not available right now: nothing selected",
            "지금은 `erase` 명령을 사용할 수 없습니다: 선택된 항목 없음",
            "команда `erase` зараз недоступна: нічого не вибрано",
        ),
        ("Enter layer name(s) to freeze", "동결할 도면층 이름 입력", "Введіть назви шарів, які заморозити"),
        // The kernel's failure inside a refusal, and the operation inside the failure.
        (
            "invalid parameters for `filletedge`: the modelling kernel failed: rounding the edges left nothing",
            "`filletedge`의 매개변수가 잘못되었습니다: 모델링 커널 오류: 모서리 모깎기: 결과가 비어 있습니다",
            "неправильні параметри для `filletedge`: помилка ядра моделювання: заокруглення ребер: нічого не лишилося",
        ),
        (
            "invalid parameters for `box`: the box has no volume",
            "`box`의 매개변수가 잘못되었습니다: 상자에 부피가 없습니다",
            "неправильні параметри для `box`: паралелепіпед не має об'єму",
        ),
        (
            "Reset Model space limits:\nInvalid point.",
            "모형 공간 한계 재설정:\n잘못된 점입니다.",
            "Скинути межі простору моделі:\nНеправильна точка.",
        ),
        ("Something no catalog knows", "Something no catalog knows", "Something no catalog knows"),
        // Names a list echoes (-PLOT ?: layouts, devices; styles) stay, so they can be typed back.
        ("Model", "Model", "Model"),
        ("None", "None", "None"),
        ("Standard", "Standard", "Standard"),
    ];
    for (english, ko, uk) in cases {
        assert_eq!(with_language("ko", || line(english)), ko);
        assert_eq!(with_language("uk", || line(english)), uk);
        // A second lookup comes from the cache and agrees.
        assert_eq!(with_language("ko", || line(english)), ko);
        assert_eq!(line(english), english);
    }
}

/// A Korean user reads the command line's history in Korean while the log, which scripts, the
/// control channel and the AI read, stays English.
#[test]
fn korean_history_is_drawn_translated_and_logged_in_english() {
    if !crate::theme::can_show("ko") {
        return;
    }
    let mut app = CadApp::new(Session::new(), Services::default());
    let ctx = egui::Context::default();
    app.run("ui.language", json!({"lang": "ko"})).unwrap();
    app.logic(&ctx);
    app.cmdline("nosuchcommand");
    app.cmdline("line");
    app.cmdline("nowhere");
    let text = render(&mut app, &ctx);
    for shown in ["알 수 없는 명령 `nosuchcommand`", "명령: LINE", "점 또는 옵션 키워드가 필요합니다."] {
        assert!(text.contains(shown), "missing {shown}: {text}");
    }
    let log = &app.session.log;
    for logged in ["unknown command `nosuchcommand`", "Command: LINE", "Point or option keyword required."] {
        assert!(log.iter().any(|l| l == logged), "{logged}: {log:?}");
    }
    assert!(crate::control::cmdline_state(&app)["history"].to_string().contains("Command: LINE"));
}

/// The AI assistant's own messages reach the palette in English with their `{names}` filled in;
/// the palette translates them ([`crate::assistant::translated`]).
#[test]
fn the_assistants_messages_are_translated() {
    for language in ["uk", "ko"] {
        let catalog = catalog(language).unwrap();
        let missing: Vec<_> = cadcraft_ai::text::ALL.iter().filter(|t| !catalog.contains_key(&("", **t))).collect();
        assert!(missing.is_empty(), "{language}: {missing:?}");
    }
}

#[test]
fn locale_and_saved_preference_resolution() {
    for tag in ["uk", "uk-UA", "uk_UA.UTF-8", "UK-ua", " uk-UA ", "uk_UA@euro"] {
        assert_eq!(locale_language(tag), Some("uk"), "{tag}");
    }
    for tag in ["de", "de-DE", "de_DE.UTF-8", "DE-at", "de-CH"] {
        assert_eq!(locale_language(tag), Some("de"), "{tag}");
    }
    for (tag, code) in [("ru", "ru"), ("ru-RU", "ru"), ("ru_RU.UTF-8", "ru"), ("fr", "fr"), ("fr-FR", "fr"), ("fr_CA.UTF-8", "fr")] {
        assert_eq!(locale_language(tag), Some(code), "{tag}");
    }
    for tag in ["ua", "ukulele", "pl-PL", "", "garbage", "💙"] {
        assert_eq!(locale_language(tag), None, "{tag}");
    }
    for tag in ["pt", "pt-BR", "pt_BR.UTF-8", "PT-br", "pt-PT"] {
        assert_eq!(locale_language(tag), Some("pt-BR"), "{tag}");
    }
    assert_eq!(locale_language("ptolemy"), None);
    assert_eq!(Preference::Auto.resolve(&["pl-PL".into(), "pt-BR".into()]), "pt-BR");
    assert_eq!(Preference::Auto.resolve(&["fr-FR".into(), "pt-BR".into()]), "fr");
    for tag in ["es", "es-ES", "es_MX.UTF-8", "ES-ar"] {
        assert_eq!(locale_language(tag), Some("es"), "{tag}");
    }
    assert_eq!(Preference::Auto.resolve(&["es-ES".into(), "pt-BR".into()]), "es");
    assert_eq!(Preference::parse("es"), Some(Preference::Spanish));
    assert_eq!(Preference::BrazilianPortuguese.resolve(&["uk-UA".into()]), "pt-BR");
    assert_eq!(Preference::parse("pt-BR"), Some(Preference::BrazilianPortuguese));
    let locales = vec!["pl-PL".into(), "uk-UA".into(), "en-US".into()];
    assert_eq!(Preference::Auto.resolve(&locales), "uk");
    assert_eq!(Preference::English.resolve(&locales), "en");
    assert_eq!(Preference::German.resolve(&locales), "de");
    assert_eq!(Preference::Auto.resolve(&["de-DE".into(), "en-US".into()]), "de");
    assert_eq!(Preference::Auto.resolve(&["en-US".into(), "uk-UA".into()]), "en");
    assert_eq!(Preference::Auto.resolve(&[]), "en");
    assert_eq!(Preference::restore(None), Preference::Auto);
    assert_eq!(Preference::restore(Some("obsolete")), Preference::Auto);
    for preference in Preference::ALL {
        assert_eq!(Preference::restore(Some(preference.code())), preference);
        let saved = serde_json::to_string(&preference).unwrap();
        assert_eq!(serde_json::from_str::<Preference>(&saved).unwrap(), preference);
    }
    assert!(serde_json::from_str::<Preference>("\"unsupported\"").is_err());
    assert_eq!(serde_json::from_value::<crate::UiState>(json!({})).unwrap().interface_language, Preference::Auto);
    // Saved with the host's other preferences; a bad saved value keeps the current choice.
    let mut a = CadApp::new(Session::new(), Services::default());
    a.ui.interface_language = Preference::Ukrainian;
    let mut b = CadApp::new(Session::new(), Services::default());
    b.load_prefs(&a.prefs_json());
    assert_eq!(b.ui.interface_language, Preference::Ukrainian);
    b.load_prefs(r#"{"interfaceLanguage":"klingon"}"#);
    assert_eq!(b.ui.interface_language, Preference::Ukrainian);
}

#[test]
fn plural_forms_and_templates_preserve_inserted_user_data() {
    with_language("uk", || {
        for (n, form) in [
            (0, 2),
            (1, 0),
            (2, 1),
            (4, 1),
            (5, 2),
            (11, 2),
            (12, 2),
            (14, 2),
            (21, 0),
            (22, 1),
            (25, 2),
            (101, 0),
            (111, 2),
            (114, 2),
            (u64::MAX, 2),
        ] {
            assert_eq!(plural_uk(n), form, "{n}");
        }
        assert_eq!(count(1, "{n} contributor", "{n} contributors"), "1 учасник");
        assert_eq!(count(22, "{n} contributor", "{n} contributors"), "22 учасники");
        assert_eq!(count(11, "{n} contributor", "{n} contributors"), "11 учасників");
        assert_eq!(format("Current: {color}", &[("color", "{n} Open".into())]), "Поточний: {n} Open");
        assert_eq!(format("{b}/{a}", &[("a", "А".into()), ("b", "Б".into())]), "Б/А");
    });
    assert_eq!(t("Layers"), "Layers");
    with_language("de", || {
        assert_eq!(t("Layers"), "Layer");
        assert_eq!(count(1, "{n} contributor", "{n} contributors"), "1 Mitwirkender");
        assert_eq!(count(2, "{n} contributor", "{n} contributors"), "2 Mitwirkende");
        assert_eq!(count(11, "{n} contributor", "{n} contributors"), "11 Mitwirkende");
    });
    with_language("uk", || {
        assert_eq!(t("Layers"), "Шари");
        with_language("en", || assert_eq!(t("Layers"), "Layers"));
        assert_eq!(t("Layers"), "Шари");
    });
    assert_eq!(count(2, "{n} contributor", "{n} contributors"), "2 contributors");
}

#[test]
fn brazilian_portuguese_plurals_and_accents() {
    for (n, form) in [(0, 0), (1, 0), (2, 1), (5, 1), (21, 1), (u64::MAX, 1)] {
        assert_eq!(plural_pt(n), form, "{n}");
    }
    with_language("pt-BR", || {
        assert_eq!(count(1, "{n} contributor", "{n} contributors"), "1 colaborador");
        assert_eq!(count(3, "{n} contributor", "{n} contributors"), "3 colaboradores");
        assert_eq!(t("Layers"), "Camadas");
        assert_eq!(t("Dimension"), "Cota");
        assert_eq!(format("Height: {height}", &[("height", "{n}".into())]), "Altura: {n}");
    });
    assert_eq!(count(2, "{n} contributor", "{n} contributors"), "2 contributors");
}

#[test]
fn live_switch_to_brazilian_portuguese_translates_the_shell() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.in_window_menu = true;
    let ctx = egui::Context::default();
    app.logic(&ctx);
    app.start("line");
    app.run("ui.language", json!({"lang":"pt-BR"})).unwrap();
    let text = render(&mut app, &ctx);
    assert!(text.contains("Propriedades"), "{text}");
    assert!(text.contains("Arquivo"), "{text}");
    assert_eq!(app.language(), "pt-BR");
    assert_eq!(t("Properties"), "Properties", "language must not leak out of the render scope");
    app.run("ui.language", json!({"lang":"en"})).unwrap();
    assert!(render(&mut app, &ctx).contains("Properties"));
}

fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> String {
    fn collect(shape: &egui::Shape, text: &mut String) {
        match shape {
            egui::Shape::Text(s) => {
                text.push_str(s.galley.text());
                text.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, text);
                }
            }
            _ => {}
        }
    }
    let mut text = String::new();
    for shape in shapes {
        collect(&shape.shape, &mut text);
    }
    text
}

fn render(app: &mut CadApp, ctx: &egui::Context) -> String {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
    // Windows fade in; a second pass makes their text visible without a time dependency.
    let mut text = String::new();
    for _ in 0..2 {
        let mut output = ctx.run_ui(input.clone(), |ui| app.ui(ui));
        text = painted_text(&output.shapes);
        output.textures_delta.clear();
    }
    text
}

#[test]
fn live_switch_translates_the_shell_without_mutating_drawing_or_automation() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.in_window_menu = true;
    app.session.state_mut().unwrap().title = "Open".into();
    let ctx = egui::Context::default();
    app.logic(&ctx);
    let before = render(&mut app, &ctx);
    assert!(before.contains("Properties"));
    let commands = crate::control::all_commands(&app);
    let (menu_request, _) = crate::ControlRequest::new("ui.menu.list", json!({}));
    let crate::control::Outcome::Done(menu_before) = crate::control::handle(&mut app, &ctx, &menu_request) else { panic!("expected menu reply") };
    app.start("line");
    let prompt = crate::control::cmdline_state(&app);
    app.run("ui.language", json!({"lang":"uk"})).unwrap();
    let after = render(&mut app, &ctx);
    assert!(after.contains("Властивості"), "{after}");
    assert!(after.contains("Файл"), "{after}");
    assert!(after.contains("Укажіть першу точку"), "{after}");
    assert!(after.contains("Open"), "the drawing title must remain unchanged");
    assert_eq!(crate::control::cmdline_state(&app), prompt);
    assert_eq!(crate::control::all_commands(&app), commands);
    let crate::control::Outcome::Done(menu_after) = crate::control::handle(&mut app, &ctx, &menu_request) else { panic!("expected menu reply") };
    assert_eq!(menu_before, menu_after);
    assert_eq!(app.session.state().unwrap().title, "Open");
    assert_eq!(t("Properties"), "Properties", "language must not leak out of the render scope");
    assert!(app.run("ui.language", json!({"lang":"bad"})).is_err());
    assert_eq!(app.language(), "uk");
    app.run("ui.resetpalettes", json!({})).unwrap();
    assert_eq!(app.language(), "uk");
    app.run("ui.language", json!({"lang":"en"})).unwrap();
    assert!(render(&mut app, &ctx).contains("Properties"));
    // The menu tree used by the control channel keeps its original labels and IDs.
    assert_eq!(crate::menus::tree(&app).first().unwrap().0, "File");
}

#[test]
fn dialogs_render_in_ukrainian_and_keep_their_source_ids() {
    for (dialog, heading) in [
        ("dsettings", "Налаштування креслення"),
        ("about", "Про Cadino"),
        ("commands", "Довідник команд"),
        ("blocks", "Блоки"),
        ("layers", "Диспетчер властивостей шарів"),
        ("qselect", "Швидкий вибір"),
        ("parameters", "Диспетчер параметрів"),
        ("language", "Мова інтерфейсу"),
        ("hatch", "Штрихування та градієнт"),
    ] {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.interface_language = Preference::Ukrainian;
        app.ui.dialog = Some(dialog.into());
        let ctx = egui::Context::default();
        app.logic(&ctx);
        let text = render(&mut app, &ctx);
        assert!(text.contains(heading), "{dialog}: {text}");
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog));
    }
}

#[test]
fn default_fonts_cover_every_german_special_letter_in_the_catalog() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = include_str!("de.tsv").chars().filter(|c| ('\u{00c0}'..='\u{00ff}').contains(c)).collect();
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}

#[test]
fn default_fonts_cover_every_brazilian_portuguese_letter_in_the_catalog() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = include_str!("pt_br.tsv").chars().filter(|c| ('\u{00c0}'..='\u{00ff}').contains(c)).collect();
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}

#[test]
fn default_fonts_cover_every_ukrainian_letter_in_the_catalog() {
    let ctx = egui::Context::default();
    // Use only egui's bundled fonts, exactly as the web build does (no system-font loader).
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = include_str!("uk.tsv").chars().filter(|c| ('\u{0400}'..='\u{04ff}').contains(c)).collect();
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            // In egui 0.36, has_glyph can report a false negative for glyphs in the same
            // face as the replacement character. Check the family's actual character maps.
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}

#[test]
fn czech_locales_preferences_and_plural_forms() {
    for tag in ["cs", "cs-CZ", "cs_CZ.UTF-8", "CS-cz", " cs-CZ ", "cs_CZ@euro"] {
        assert_eq!(locale_language(tag), Some("cs"), "{tag}");
    }
    for tag in ["cz", "csharp", "csomething"] {
        assert_eq!(locale_language(tag), None, "{tag}");
    }
    assert_eq!(Preference::parse("cs"), Some(Preference::Czech));
    assert_eq!(Preference::Czech.name(), "Čeština");
    assert_eq!(Preference::Czech.resolve(&["en-US".into()]), "cs");
    assert_eq!(Preference::Auto.resolve(&["pl-PL".into(), "cs-CZ".into(), "en-US".into()]), "cs");
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::Czech;
    let mut restored = CadApp::new(Session::new(), Services::default());
    restored.load_prefs(&app.prefs_json());
    assert_eq!(restored.ui.interface_language, Preference::Czech);
    with_language("cs", || {
        for (n, form) in [(0, 2), (1, 0), (2, 1), (4, 1), (5, 2), (11, 2), (21, 2), (22, 2), (25, 2), (u64::MAX, 2)] {
            assert_eq!(plural_cs(n), form, "{n}");
            let contributor = ["přispěvatel", "přispěvatelé", "přispěvatelů"][form];
            let commit = ["commit", "commity", "commitů"][form];
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("{n} {contributor}"));
            assert_eq!(count(n, "{n} commit", "{n} commits"), format!("{n} {commit}"));
        }
        assert_eq!(format("Current: {color}", &[("color", "{n} Open".into())]), "Aktuální: {n} Open");
    });
}

#[test]
fn czech_menu_headings_keep_object_labels_and_other_catalogs() {
    with_language("cs", || {
        assert_eq!(menu("View"), "Zobrazení");
        assert_eq!(t("View"), "Pohled");
        assert_eq!(menu("Dimension"), "Kótování");
        assert_eq!(t("Dimension"), "Kóta");
        assert_eq!(menu("Draw"), "Kreslení");
        assert_eq!(menu("Modify"), "Modifikace");
        assert_eq!(menu("Edit"), "Úpravy");
        assert_eq!(menu("File"), "Soubor");
    });
    for language in ["en", "uk", "de", "es", "pt-BR", "unknown"] {
        for source in crate::menus::MENUS {
            assert_eq!(tr_menu(language, source), tr(language, source));
        }
    }
}

#[test]
fn live_czech_switch_preserves_drawing_command_and_saved_choice() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.in_window_menu = true;
    app.session.state_mut().unwrap().title = "Open".into();
    let ctx = egui::Context::default();
    app.logic(&ctx);
    assert!(render(&mut app, &ctx).contains("Properties"));
    app.start("line");
    let prompt = crate::control::cmdline_state(&app);
    let commands = crate::control::all_commands(&app);
    app.run("ui.language", json!({"lang":"cs"})).unwrap();
    let text = render(&mut app, &ctx);
    for label in ["Vlastnosti", "Soubor", "Zobrazení", "Kreslení", "Kótování", "Určete první bod"] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert_eq!(app.language(), "cs");
    assert_eq!(app.session.state().unwrap().title, "Open");
    assert_eq!(crate::control::cmdline_state(&app), prompt);
    assert_eq!(crate::control::all_commands(&app), commands);
    assert_eq!(t("Properties"), "Properties");
    app.run("ui.resetpalettes", json!({})).unwrap();
    assert_eq!(app.language(), "cs");
    app.run("ui.language", json!({"lang":"en"})).unwrap();
    assert!(render(&mut app, &ctx).contains("Properties"));
}

#[test]
fn czech_dialogs_render_with_stable_ids() {
    for (dialog, heading) in [
        ("dsettings", "Nastavení kreslení"),
        ("about", "O aplikaci Cadino"),
        ("commands", "Přehled příkazů"),
        ("blocks", "Bloky"),
        ("layers", "Správce vlastností hladin"),
        ("qselect", "Rychlý výběr"),
        ("parameters", "Správce parametrů"),
        ("language", "Čeština"),
        ("options:preferences", "Uživatelské předvolby"),
    ] {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.interface_language = Preference::Czech;
        app.ui.dialog = Some(dialog.into());
        let ctx = egui::Context::default();
        app.logic(&ctx);
        let text = render(&mut app, &ctx);
        assert!(text.contains(heading), "{dialog}: {text}");
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog));
    }
}

#[test]
fn default_fonts_cover_czech_letters_and_typographic_quotes() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = "ÁČĎÉĚÍŇÓŘŠŤÚŮÝŽáčďéěíňóřšťúůýž„“".chars().collect();
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}

#[test]
fn russian_and_french_plurals() {
    with_language("ru", || {
        for (n, word) in [(1, "участник"), (2, "участника"), (5, "участников"), (21, "участник"), (22, "участника"), (11, "участников")]
        {
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("{n} {word}"));
        }
    });
    with_language("fr", || {
        for (n, word) in [(0, "contributeur"), (1, "contributeur"), (2, "contributeurs"), (10, "contributeurs")] {
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("{n} {word}"));
        }
    });
}

#[test]
fn italian_locales_plurals_and_accents() {
    for tag in ["it", "it-IT", "it_IT.UTF-8", "IT-ch", " it-CH ", "it_IT@euro"] {
        assert_eq!(locale_language(tag), Some("it"), "{tag}");
    }
    assert_eq!(locale_language("italic"), None);
    assert_eq!(Preference::parse("it"), Some(Preference::Italian));
    assert_eq!(Preference::Auto.resolve(&["it-IT".into(), "en-US".into()]), "it");
    assert_eq!(Preference::Italian.resolve(&["uk-UA".into()]), "it");
    with_language("it", || {
        for (n, word) in [(0, "collaboratori"), (1, "collaboratore"), (2, "collaboratori"), (21, "collaboratori")] {
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("{n} {word}"));
        }
        assert_eq!(t("Draw"), "Disegna");
        assert_eq!(tr_menu("it", "Modify"), "Edita");
        assert_eq!(tr("it", "Modify"), "Modifica");
        assert_eq!(tr_menu("it", "View"), "Visualizza");
        assert_eq!(tr("it", "View"), "Vista");
    });
    // Italian accented letters (à è é ì ò ù and capitals) come from the bundled egui fonts.
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = include_str!("it.tsv").chars().filter(|c| ('\u{00c0}'..='\u{00ff}').contains(c)).collect();
    assert!(letters.contains(&'à') && letters.contains(&'è'));
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}

/// Chinese shows only with an installed CJK font (none is bundled); without one the interface
/// stays English instead of drawing boxes, and choosing it says why.
#[test]
fn chinese_needs_an_installed_cjk_font() {
    assert_eq!(locale_language("zh-CN"), Some("zh"));
    let mut app = CadApp::new(Session::new(), Services::default());
    let r = app.run("ui.language", json!({"lang": "zh"})).unwrap();
    assert_eq!(r["interfaceLanguage"], "zh");
    let font = crate::theme::cjk_font();
    assert_eq!(app.language(), if font.is_some() { "zh" } else { "en" });
    assert_eq!(r["shown"], app.language());
    assert!(crate::theme::can_show("uk") && crate::theme::can_show("en"));
    if font.is_none() {
        assert!(app.status.as_ref().is_some_and(|(s, _)| s.contains("No Chinese font")));
    }
}

#[test]
fn hatch_dialog_extents_label_says_extents_in_ukrainian() {
    assert_eq!(tr("uk", "Default to boundary extents"), "Типово за габаритами контуру");
}

#[test]
fn korean_representative_translations_remain_available() {
    let ko = catalog("ko").unwrap();
    // Fixed regression examples; new source strings remain reported, not required for Korean.
    assert!(REPORTED.contains(&"ko"));
    for (context, source, expected) in [
        ("", "File", "파일"),
        ("menu", "View", "보기"),
        ("menu", "Draw", "그리기"),
        ("menu", "Dimension", "치수"),
        ("menu", "Modify", "수정"),
        ("", "Options...", "옵션..."),
        ("", "User Preferences", "사용자 기본 설정"),
        ("", "Interface Language", "인터페이스 언어"),
        ("", "Properties", "특성"),
        ("", "Degree", "도 기호"),
        ("", "Style: Standard", "스타일: Standard"),
        ("", "WCS ⌄", "WCS 선택"),
        ("@plural", "{n} contributor", "기여자 {n}명"),
        ("@plural", "{n} commit", "커밋 {n}개"),
    ] {
        assert_eq!(ko.get(&(context, source)).copied(), Some(expected), "{context}: {source}");
    }
    assert_eq!(tr("ko", "future untranslated label"), "future untranslated label");
}

#[test]
fn korean_locale_saved_preference_and_single_count_form() {
    for tag in ["ko", "ko-KR", "ko_KR.UTF-8", "KO-kr", " ko-KR ", "ko_KR@euro"] {
        assert_eq!(locale_language(tag), Some("ko"), "{tag}");
    }
    for tag in ["kr", "korean", "kotlin"] {
        assert_eq!(locale_language(tag), None, "{tag}");
    }
    assert_eq!(Preference::parse("ko"), Some(Preference::Korean));
    assert_eq!(Preference::Korean.resolve(&["en-US".into()]), "ko");
    assert_eq!(Preference::Auto.resolve(&["pl-PL".into(), "ko-KR".into(), "en-US".into()]), "ko");
    let mut app = CadApp::new(Session::new(), Services::default());
    app.system_languages = vec!["ko-KR".into()];
    app.run("ui.language", json!({"lang": "ko"})).unwrap();
    let mut restored = CadApp::new(Session::new(), Services::default());
    restored.system_languages = vec!["en-US".into()];
    restored.load_prefs(&app.prefs_json());
    assert_eq!(restored.ui.interface_language, Preference::Korean);
    assert_eq!(restored.language(), app.language());
    with_language("ko", || {
        for n in [0, 1, 2, 5, 11, 21, 101, u64::MAX] {
            assert_eq!(plural_ko(n), 0);
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("기여자 {n}명"));
        }
        assert_eq!(format("Current: {color}", &[("color", "{n} Open".into())]), "현재: {n} Open");
    });
}

#[test]
fn korean_live_switch_keeps_automation_and_drawing_and_falls_back_without_font() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.in_window_menu = true;
    app.session.state_mut().unwrap().title = "Open".into();
    let ctx = egui::Context::default();
    app.logic(&ctx);
    assert!(render(&mut app, &ctx).contains("Properties"));
    app.start("line");
    let prompt = crate::control::cmdline_state(&app);
    let commands = crate::control::all_commands(&app);
    let result = app.run("ui.language", json!({"lang": "ko"})).unwrap();
    let supported = crate::theme::can_show("ko");
    assert_eq!(result["shown"], if supported { "ko" } else { "en" });
    assert_eq!(app.ui.interface_language, Preference::Korean);
    app.logic(&ctx);
    assert_eq!(app.fonts_language, Some(app.language()));
    let text = render(&mut app, &ctx);
    if supported {
        for label in ["특성", "파일", "첫 번째 점 지정"] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    } else {
        assert!(text.contains("Properties"));
        assert!(app.status.as_ref().is_some_and(|(s, _)| s.contains("No font covers the Korean interface")));
        assert_eq!(Preference::Korean.name(), "Korean");
    }
    assert_eq!(app.session.state().unwrap().title, "Open");
    assert_eq!(crate::control::cmdline_state(&app), prompt);
    assert_eq!(crate::control::all_commands(&app), commands);
    assert_eq!(t("Properties"), "Properties");
    assert!(app.run("ui.language", json!({"lang": "bad"})).is_err());
    app.run("ui.resetpalettes", json!({})).unwrap();
    assert_eq!(app.ui.interface_language, Preference::Korean);
    // The general ui.set route uses the same saved preference and font installation.
    let (request, _) = crate::ControlRequest::new("ui.set", json!({"interfaceLanguage": "zh"}));
    let _ = crate::control::handle(&mut app, &ctx, &request);
    assert_eq!(app.ui.interface_language, Preference::Chinese);
    app.logic(&ctx);
    assert_eq!(app.fonts_language, Some(if crate::theme::can_show("zh") { "zh" } else { "en" }));
    render(&mut app, &ctx);
    let (request, _) = crate::ControlRequest::new("ui.set", json!({"interfaceLanguage": "ko"}));
    let _ = crate::control::handle(&mut app, &ctx, &request);
    assert_eq!(app.ui.interface_language, Preference::Korean);
    app.logic(&ctx);
    assert_eq!(app.fonts_language, Some(app.language()));
    let text = render(&mut app, &ctx);
    assert!(text.contains(if supported { "특성" } else { "Properties" }));
    let (request, _) = crate::ControlRequest::new("ui.set", json!({"interfaceLanguage": "en"}));
    let _ = crate::control::handle(&mut app, &ctx, &request);
    app.logic(&ctx);
    assert_eq!(app.fonts_language, Some("en"));
    assert_eq!(app.ui.interface_language, Preference::English);
    assert!(render(&mut app, &ctx).contains("Properties"));
    assert_eq!(crate::menus::tree(&app).first().unwrap().0, "File");
}

#[test]
fn korean_dialogs_keep_source_ids() {
    if !crate::theme::can_show("ko") {
        return; // The fallback path is covered above; do not claim glyph rendering here.
    }
    for (dialog, source) in [
        ("dsettings", "Drafting Settings"),
        ("about", "About Cadino"),
        ("commands", "Command Reference"),
        ("blocks", "Blocks"),
        ("layers", "Layer Properties Manager"),
        ("qselect", "Quick Select"),
        ("parameters", "Parameters Manager"),
        ("language", "Interface Language"),
        ("options:preferences", "User Preferences"),
    ] {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.interface_language = Preference::Korean;
        app.ui.dialog = Some(dialog.into());
        let ctx = egui::Context::default();
        app.logic(&ctx);
        let text = render(&mut app, &ctx);
        assert!(text.contains(tr("ko", source)), "{dialog}: {text}");
        if dialog == "language" {
            assert!(text.contains("한국어"), "{dialog}: {text}");
        }
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog));
    }
}

#[test]
fn korean_ui_fonts_cover_catalog_and_language_picker_in_both_families() {
    if !crate::theme::can_show("ko") {
        return;
    }
    let letters: BTreeSet<_> = include_str!("ko.tsv")
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .flat_map(str::chars)
        .chain("한국어".chars())
        .filter(|c| !c.is_ascii())
        .collect();
    // The picker shows 한국어 in an English interface only when the CJK fonts loaded in every
    // language cover Hangul; the Korean interface itself always does, with a fallback font.
    let ctx = egui::Context::default();
    let english = crate::theme::cjk_covers_hangul();
    assert_eq!(with_language("en", || Preference::Korean.name()), if english { "한국어" } else { "Korean" });
    assert_eq!(with_language("ko", || Preference::Korean.name()), "한국어");
    crate::theme::install_fonts(&ctx, if english { "en" } else { "ko" });
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let missing = ctx.fonts_mut(|view| {
        let mut missing = Vec::new();
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                if !font.characters().contains_key(c) {
                    missing.push(format!("{family:?}: {c} (U+{:04X})", u32::from(*c)));
                }
            }
        }
        missing
    });
    assert!(missing.is_empty(), "missing Korean UI glyphs: {}", missing.join(", "));
}

#[test]
fn swedish_locales_preferences_and_plural_forms() {
    for tag in ["sv", "sv-SE", "sv_SE.UTF-8", "SV-se", " sv-SE ", "sv-FI"] {
        assert_eq!(locale_language(tag), Some("sv"), "{tag}");
    }
    // `se` is Northern Sami and `swe` the three-letter code; neither picks Swedish.
    for tag in ["se", "swe", "svenska"] {
        assert_eq!(locale_language(tag), None, "{tag}");
    }
    assert_eq!(Preference::parse("sv"), Some(Preference::Swedish));
    assert_eq!(Preference::Swedish.name(), "Svenska");
    assert_eq!(Preference::Swedish.resolve(&["en-US".into()]), "sv");
    assert_eq!(Preference::Auto.resolve(&["pl-PL".into(), "sv-SE".into(), "en-US".into()]), "sv");
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::Swedish;
    let mut restored = CadApp::new(Session::new(), Services::default());
    restored.load_prefs(&app.prefs_json());
    assert_eq!(restored.ui.interface_language, Preference::Swedish);
    with_language("sv", || {
        for n in [0, 1, 2, 5, 21, u64::MAX] {
            // `bidragsgivare` is the same in the singular and the plural; `commit` takes an -s.
            assert_eq!(count(n, "{n} contributor", "{n} contributors"), format!("{n} bidragsgivare"));
            let commit = if n == 1 { "commit" } else { "commits" };
            assert_eq!(count(n, "{n} commit", "{n} commits"), format!("{n} {commit}"));
        }
        assert_eq!(format("Current: {color}", &[("color", "{n} Open".into())]), "Aktuell: {n} Open");
    });
}

#[test]
fn swedish_menu_headings_keep_object_labels() {
    with_language("sv", || {
        assert_eq!(menu("Dimension"), "Måttsättning");
        assert_eq!(t("Dimension"), "Mått");
        assert_eq!(menu("View"), "Visa");
        assert_eq!(menu("Draw"), "Rita");
        assert_eq!(menu("Modify"), "Ändra");
        assert_eq!(menu("Edit"), "Redigera");
        assert_eq!(menu("File"), "Arkiv");
    });
}

#[test]
fn live_swedish_switch_preserves_drawing_command_and_saved_choice() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.in_window_menu = true;
    app.session.state_mut().unwrap().title = "Open".into();
    let ctx = egui::Context::default();
    app.logic(&ctx);
    assert!(render(&mut app, &ctx).contains("Properties"));
    app.start("line");
    let prompt = crate::control::cmdline_state(&app);
    let commands = crate::control::all_commands(&app);
    app.run("ui.language", json!({"lang":"sv"})).unwrap();
    let text = render(&mut app, &ctx);
    for label in ["Egenskaper", "Arkiv", "Visa", "Rita", "Måttsättning", "Ange första punkten"] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert_eq!(app.language(), "sv");
    assert_eq!(app.session.state().unwrap().title, "Open");
    assert_eq!(crate::control::cmdline_state(&app), prompt);
    assert_eq!(crate::control::all_commands(&app), commands);
    app.run("ui.language", json!({"lang":"en"})).unwrap();
    assert!(render(&mut app, &ctx).contains("Properties"));
}

#[test]
fn swedish_dialogs_render_with_stable_ids() {
    for (dialog, heading) in [
        ("dsettings", "Ritinställningar"),
        ("about", "Om Cadino"),
        ("blocks", "Block"),
        ("layers", "Hantera lageregenskaper"),
        ("qselect", "Snabbmarkering"),
        ("parameters", "Hantera parametrar"),
        ("language", "Svenska"),
        ("options:preferences", "Användarinställningar"),
    ] {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.interface_language = Preference::Swedish;
        app.ui.dialog = Some(dialog.into());
        let ctx = egui::Context::default();
        app.logic(&ctx);
        let text = render(&mut app, &ctx);
        assert!(text.contains(heading), "{dialog}: {text}");
        assert_eq!(app.ui.dialog.as_deref(), Some(dialog));
    }
}

#[test]
fn default_fonts_cover_swedish_letters_and_quotes() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    output.textures_delta.clear();
    let letters: BTreeSet<_> = "ÅÄÖÉåäöé”–".chars().collect();
    ctx.fonts_mut(|view| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let mut font = view.fonts.font(&family);
            for c in &letters {
                assert!(font.characters().contains_key(c), "missing {c} in {family:?}");
            }
        }
    });
}
