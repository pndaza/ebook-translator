use crate::error::Result;
use crate::types::{Block, MODE_BILINGUAL};
use lol_html::html_content::ContentType;
use lol_html::{element, end_tag, rewrite_str, text, RewriteStrSettings};
use std::cell::RefCell;
use std::rc::Rc;

const BLOCK_SELECTOR: &str =
    "p, h1, h2, h3, h4, h5, h6, li, blockquote, td, th, dd, dt, figcaption";

const STYLE: &str = ".ebtr-translation{margin:.45em 0;}";

fn qualify(text: &str) -> bool {
    crate::types::is_translatable(text)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode the common named entities plus numeric character references.
pub fn unescape_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'&' {
                i += 1;
            }
            out.push_str(&s[start..i]);
            continue;
        }
        // find the terminator ';'; cap the scan at 10 bytes
        let rest = &s[i + 1..];
        let end = rest.find(';');
        let Some(end) = end.filter(|e| *e <= 10) else {
            out.push('&');
            i += 1;
            continue;
        };
        let ent = &rest[..end];
        let replacement: Option<String> = match ent {
            "amp" => Some("&".into()),
            "lt" => Some("<".into()),
            "gt" => Some(">".into()),
            "quot" => Some("\"".into()),
            "apos" => Some("'".into()),
            "nbsp" => Some("\u{a0}".into()),
            _ => {
                if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from)
                } else if let Some(dec) = ent.strip_prefix('#') {
                    dec.parse::<u32>().ok().and_then(char::from_u32).map(String::from)
                } else {
                    None
                }
            }
        };
        match replacement {
            Some(r) => {
                out.push_str(&r);
                i += end + 2;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

struct CollectState {
    blocks: Vec<Block>,
    /// Stack of indices into `blocks`; innermost open block is last.
    stack: Vec<usize>,
}

/// Pass 1: walk a spine document and record one entry per block-level
/// element (in pre-order), with its concatenated text.
pub fn collect_blocks(html: &str) -> Vec<Block> {
    let state = Rc::new(RefCell::new(CollectState {
        blocks: Vec::new(),
        stack: Vec::new(),
    }));

    let s = state.clone();
    let element_handler = element!(BLOCK_SELECTOR, move |el| {
        let mut st = s.borrow_mut();
        let idx = st.blocks.len();
        st.blocks.push(Block {
            tag: el.tag_name().to_string(),
            ..Default::default()
        });
        st.stack.push(idx);
        drop(st);
        let s2 = s.clone();
        el.on_end_tag(end_tag!(move |_end| {
            let mut st = s2.borrow_mut();
            if let Some(idx) = st.stack.pop() {
                st.blocks[idx].text = collapse_ws(&unescape_entities(&st.blocks[idx].text));
                st.blocks[idx].skipped = !qualify(&st.blocks[idx].text);
            }
            Ok(())
        }))?;
        Ok(())
    });

    let s = state.clone();
    let text_handler = text!("*", move |t| {
        let mut st = s.borrow_mut();
        if let Some(&idx) = st.stack.last() {
            st.blocks[idx].text.push_str(t.as_str());
        }
        Ok(())
    });

    let _ = rewrite_str(
        html,
        RewriteStrSettings::new()
            .append_element_content_handler(element_handler)
            .append_element_content_handler(text_handler),
    );

    let mut st = state.borrow_mut();
    while let Some(idx) = st.stack.pop() {
        st.blocks[idx].text = collapse_ws(&unescape_entities(&st.blocks[idx].text));
        st.blocks[idx].skipped = !qualify(&st.blocks[idx].text);
    }
    std::mem::take(&mut st.blocks)
}

/// Tag used for the inserted sibling so lists/tables stay valid.
fn sibling_tag(tag: &str) -> &'static str {
    match tag {
        "li" => "li",
        "dt" => "dt",
        "dd" => "dd",
        "td" | "th" => "td",
        _ => "p",
    }
}

/// Pass 2: produce the translated document. `blocks` must be the exact
/// list returned by [`collect_blocks`] for the same document.
pub fn rewrite_doc(html: &str, blocks: &[Block], mode: &str) -> Result<String> {
    if mode == MODE_BILINGUAL {
        rewrite_bilingual(html, blocks)
    } else {
        rewrite_translated(html, blocks)
    }
}

fn rewrite_bilingual(html: &str, blocks: &[Block]) -> Result<String> {
    let cursor = Rc::new(RefCell::new(0usize));

    let c = cursor.clone();
    let block_handler = element!(BLOCK_SELECTOR, move |el| {
        let mut cur = c.borrow_mut();
        let idx = *cur;
        *cur += 1;
        let block = blocks.get(idx);
        if let Some(b) = block {
            if !b.skipped {
                if let Some(tr) = &b.translation {
                    let tag = sibling_tag(&b.tag);
                    el.after(
                        &format!(
                            "<{tag} class=\"ebtr-translation\">{}</{tag}>",
                            escape_html(tr)
                        ),
                        ContentType::Html,
                    );
                }
            }
        }
        Ok(())
    });

    let style_handler = element!("head", |el| {
        el.append(
            &format!("<style type=\"text/css\">{STYLE}</style>"),
            ContentType::Html,
        );
        Ok(())
    });

    rewrite_str(
        html,
        RewriteStrSettings::new()
            .append_element_content_handler(block_handler)
            .append_element_content_handler(style_handler),
    )
    .map_err(|e| crate::error::AppError::msg(format!("rewrite failed: {e}")))
}

struct TranslatedState {
    cursor: usize,
    stack: Vec<usize>,
}

fn rewrite_translated(html: &str, blocks: &[Block]) -> Result<String> {
    let state = Rc::new(RefCell::new(TranslatedState {
        cursor: 0,
        stack: Vec::new(),
    }));

    let s = state.clone();
    let element_handler = element!(BLOCK_SELECTOR, move |el| {
        let mut st = s.borrow_mut();
        let idx = st.cursor;
        st.cursor += 1;
        st.stack.push(idx);
        // Insert the translation directly after the block's start tag — not
        // before its first text chunk, which may live inside a nested
        // inline element (<em>, <a>...) and would inherit its semantics.
        if let Some(b) = blocks.get(idx) {
            if !b.skipped {
                if let Some(tr) = &b.translation {
                    el.prepend(&escape_html(tr), ContentType::Html);
                }
            }
        }
        drop(st);
        let s2 = s.clone();
        el.on_end_tag(end_tag!(move |_end| {
            s2.borrow_mut().stack.pop();
            Ok(())
        }))?;
        Ok(())
    });

    let s = state.clone();
    let text_handler = text!("*", move |t| {
        let st = s.borrow_mut();
        if let Some(&idx) = st.stack.last() {
            if let Some(b) = blocks.get(idx) {
                if !b.skipped && b.translation.is_some() {
                    t.remove();
                }
            }
        }
        Ok(())
    });

    rewrite_str(
        html,
        RewriteStrSettings::new()
            .append_element_content_handler(element_handler)
            .append_element_content_handler(text_handler),
    )
    .map_err(|e| crate::error::AppError::msg(format!("rewrite failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str, tag: &str, translation: Option<&str>) -> Block {
        Block {
            text: text.to_string(),
            skipped: !qualify(text),
            translation: translation.map(str::to_string),
            tag: tag.to_string(),
            parts: Vec::new(),
        }
    }

    #[test]
    fn collects_simple_paragraphs() {
        let html = r#"<html><head><title>T</title></head><body><p>Hello world</p><p>42</p><p>Bye now</p></body></html>"#;
        let blocks = collect_blocks(html);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].text, "Hello world");
        assert!(!blocks[0].skipped);
        assert!(blocks[1].skipped, "numeric-only paragraph is skipped");
        assert_eq!(blocks[2].text, "Bye now");
    }

    #[test]
    fn collects_nested_lists_to_innermost() {
        let html = r#"<body><ul><li>Outer <ul><li>Inner</li></ul></li></ul></body>"#;
        let blocks = collect_blocks(html);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Outer");
        assert_eq!(blocks[1].text, "Inner");
        assert_eq!(blocks[0].tag, "li");
    }

    #[test]
    fn collects_inline_markup_text() {
        let html = r#"<body><p>Hi <em>there</em>, &amp; welcome!</p></body>"#;
        let blocks = collect_blocks(html);
        assert_eq!(blocks[0].text, "Hi there, & welcome!");
    }

    #[test]
    fn bilingual_inserts_sibling_after_block() {
        let html = r#"<html><head><title>T</title></head><body><p>One</p><ul><li>Two</li></ul></body></html>"#;
        let blocks = vec![
            block("One", "p", Some("၁")),
            block("Two", "li", Some("၂")),
        ];
        let out = rewrite_doc(html, &blocks, MODE_BILINGUAL).unwrap();
        assert!(out.contains("<p>One</p><p class=\"ebtr-translation\">၁</p>"), "{out}");
        assert!(
            out.contains("<li>Two</li><li class=\"ebtr-translation\">၂</li>"),
            "{out}"
        );
        assert!(out.contains("<style"), "style injected: {out}");
    }

    #[test]
    fn bilingual_leaves_untranslated_blocks_alone() {
        let html = r#"<body><p>One</p><p>Two</p></body>"#;
        let blocks = vec![block("One", "p", Some("၁")), block("Two", "p", None)];
        let out = rewrite_doc(html, &blocks, MODE_BILINGUAL).unwrap();
        assert_eq!(out.matches("ebtr-translation").count(), 1);
    }

    #[test]
    fn translated_replaces_text_preserving_nested_blocks() {
        let html =
            r#"<body><ul><li>Outer <ul><li>Inner</li></ul></li></ul></body>"#;
        let blocks = vec![
            block("Outer", "li", Some("အပြင်")),
            block("Inner", "li", Some("အတွင်း")),
        ];
        let out = rewrite_doc(html, &blocks, crate::types::MODE_TRANSLATED).unwrap();
        assert!(out.contains("အပြင်"), "{out}");
        assert!(out.contains("အတွင်း"), "{out}");
        assert!(!out.contains("Outer"), "{out}");
        assert!(!out.contains("Inner"), "{out}");
    }

    #[test]
    fn translated_removes_inline_text() {
        let html = r#"<body><p>Hi <em>there</em> friend</p></body>"#;
        let blocks = vec![block("Hi there friend", "p", Some("မင်္ဂလာပါ"))];
        let out = rewrite_doc(html, &blocks, crate::types::MODE_TRANSLATED).unwrap();
        assert!(out.contains("မင်္ဂလာပါ"), "{out}");
        assert!(!out.contains("Hi"), "{out}");
        assert!(!out.contains("friend"), "{out}");
    }

    #[test]
    fn escapes_entities() {
        assert_eq!(unescape_entities("a &amp; b &#65;&#x42;"), "a & b AB");
        assert_eq!(escape_html("&<>\""), "&amp;&lt;&gt;&quot;");
    }
}
