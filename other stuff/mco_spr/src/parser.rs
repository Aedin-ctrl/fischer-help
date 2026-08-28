//! Turns a dropped source file into a list of [`Block`]s.
//!
//! This is structural parsing, not code understanding: each top-level
//! function becomes one block, named after the function, with its
//! original source text (including any `///` doc comment directly above
//! it) preserved verbatim. Anything not shaped as free top-level
//! functions (methods inside `impl` blocks, closures, etc.) is skipped
//! for now — fisch_new.rs and friends are already written as a flat list
//! of free functions, so this covers the real case.
//!
//! .rs and .py get a real structural parse. Everything else falls back to
//! a brace-matching heuristic (parse_generic) that looks for
//! `name(...) {` at column 0 — it's a best-effort guess for C-like
//! languages, not a real parser, and says so in the block's doc field.

use std::path::Path;

use crate::blocks::Block;

pub fn parse_file(path: &Path) -> Result<Vec<Block>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("couldn't read file: {e}"))?;

    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => parse_rust(&content),
        Some("py") => parse_python(&content),
        _ => parse_generic(&content),
    }
}

fn parse_rust(content: &str) -> Result<Vec<Block>, String> {
    let file = syn::parse_file(content).map_err(|e| format!("couldn't parse Rust source: {e}"))?;
    let lines: Vec<&str> = content.lines().collect();

    let mut blocks = Vec::new();
    for item in &file.items {
        let syn::Item::Fn(item_fn) = item else {
            continue;
        };

        use syn::spanned::Spanned;
        let start_line = item_fn.span().start().line; // 1-indexed
        let end_line = item_fn.span().end().line;

        let doc_start = extend_upward_over_doc_comment(&lines, start_line);

        let source = lines
            .get(doc_start.saturating_sub(1)..end_line)
            .map(|s| s.join("\n"))
            .unwrap_or_default();

        let doc = lines
            .get(doc_start.saturating_sub(1)..start_line.saturating_sub(1))
            .map(|doc_lines| {
                doc_lines
                    .iter()
                    .map(|l| l.trim().trim_start_matches("///").trim_start_matches("//!").trim())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        blocks.push(Block::Function {
            name: item_fn.sig.ident.to_string(),
            doc,
            source,
        });
    }

    if blocks.is_empty() {
        return Err("no top-level `fn` items found in that file".to_string());
    }
    Ok(blocks)
}

/// Walks backward from a 1-indexed function start line over any
/// contiguous `///` doc-comment lines (and blank lines directly between
/// the comment and the fn), returning the 1-indexed line the block
/// should actually start on.
fn extend_upward_over_doc_comment(lines: &[&str], fn_start_line: usize) -> usize {
    let mut line = fn_start_line;
    while line > 1 {
        let prev = lines[line - 2].trim();
        if prev.starts_with("///") || prev.starts_with("//!") {
            line -= 1;
        } else {
            break;
        }
    }
    line
}

/// `def name(...):` blocks, using indentation to find each function's
/// extent, with a leading `"""..."""` docstring (if present) used as the
/// block's description.
fn parse_python(content: &str) -> Result<Vec<Block>, String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut blocks = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();

        if indent == 0 && (trimmed.starts_with("def ") || trimmed.starts_with("async def ")) {
            let name = trimmed
                .trim_start_matches("async ")
                .trim_start_matches("def ")
                .split(['(', ' '])
                .next()
                .unwrap_or("")
                .to_string();

            let start = i;
            let mut end = i + 1;
            while end < lines.len() {
                let l = lines[end];
                let this_indent = l.len() - l.trim_start().len();
                if !l.trim().is_empty() && this_indent == 0 {
                    break;
                }
                end += 1;
            }

            let doc = lines[start + 1..end]
                .iter()
                .find(|l| l.trim_start().starts_with("\"\"\""))
                .map(|l| l.trim().trim_matches('"').trim().to_string())
                .unwrap_or_default();

            blocks.push(Block::Function {
                name,
                doc,
                source: lines[start..end].join("\n"),
            });

            i = end;
        } else {
            i += 1;
        }
    }

    if blocks.is_empty() {
        return Err("no top-level `def` functions found in that file".to_string());
    }
    Ok(blocks)
}

/// Best-effort brace-matching for anything that isn't .rs/.py: looks for
/// a column-0 line shaped like `name(...) {` (skipping obvious
/// control-flow keywords), then finds its matching closing brace by
/// counting braces. Not a real parser for any of these languages — this
/// will misfire on macros, multi-line signatures, and plenty else. Each
/// resulting block's doc says so, since there's no way to detect
/// confidence here otherwise.
fn parse_generic(content: &str) -> Result<Vec<Block>, String> {
    const CONTROL_KEYWORDS: &[&str] = &["if", "for", "while", "switch", "else", "catch", "do"];

    let lines: Vec<&str> = content.lines().collect();
    let mut blocks = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        let looks_like_fn = !line.starts_with(char::is_whitespace)
            && trimmed.contains('(')
            && trimmed.trim_end().ends_with('{')
            && !CONTROL_KEYWORDS.iter().any(|kw| {
                trimmed.starts_with(kw) && trimmed[kw.len()..].starts_with([' ', '('])
            });

        if !looks_like_fn {
            i += 1;
            continue;
        }

        let name = trimmed
            .split('(')
            .next()
            .unwrap_or("")
            .rsplit(|c: char| c.is_whitespace() || c == '*')
            .next()
            .unwrap_or("block")
            .to_string();

        let mut depth = 0i32;
        let mut end = i;
        for (offset, l) in lines[i..].iter().enumerate() {
            depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
            if depth <= 0 && offset > 0 {
                end = i + offset;
                break;
            }
            end = i + offset;
        }

        blocks.push(Block::Function {
            name,
            doc: "heuristically parsed — verify this block's extent".to_string(),
            source: lines[i..=end].join("\n"),
        });
        i = end + 1;
    }

    if blocks.is_empty() {
        return Err("couldn't find any function-shaped blocks in that file".to_string());
    }
    Ok(blocks)
}
