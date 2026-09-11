pub mod build;
pub mod html;

use crate::error::{AppError, Result};
use crate::types::{BookInfo, ContentDoc, LoadedBook, SegmentInfo};
use base64::Engine;
use std::collections::HashMap;
use std::io::Read;
use std::io::Cursor;

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve an OPF-relative href to a zip entry path.
fn normalize_path(base_dir: &str, href: &str) -> String {
    let path = href.split('#').next().unwrap_or(href);
    let decoded = percent_decode(path);
    let joined = if decoded.starts_with('/') || base_dir.is_empty() {
        decoded.trim_start_matches('/').to_string()
    } else {
        format!("{base_dir}/{decoded}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub fn is_epub(bytes: &[u8]) -> bool {
    bytes.len() > 4 && &bytes[..4] == b"PK\x03\x04"
}

struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

/// Parse an EPUB into a LoadedBook: metadata, cover, TOC titles, and the
/// translatable block list of every spine document that has content.
pub fn parse(bytes: Vec<u8>, file_path: &str) -> Result<LoadedBook> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice()))?;

    let container = read_entry(&mut zip, "META-INF/container.xml")
        .ok_or_else(|| AppError::msg("not an EPUB: missing META-INF/container.xml"))?;
    let container = String::from_utf8_lossy(&container);
    let doc = roxmltree::Document::parse(&container)?;
    let opf_path = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "rootfile")
        .find_map(|n| n.attribute("full-path").map(str::to_string))
        .ok_or_else(|| AppError::msg("container.xml has no rootfile"))?;

    let opf_raw = read_entry(&mut zip, &opf_path)
        .ok_or_else(|| AppError::msg(format!("OPF not found in archive: {opf_path}")))?;
    let opf = String::from_utf8_lossy(&opf_raw);
    let doc = roxmltree::Document::parse(&opf)?;

    const DC: &str = "http://purl.org/dc/elements/1.1/";
    fn dc_text(doc: &roxmltree::Document, name: &str) -> String {
        doc.descendants()
            .find(|n| n.tag_name().namespace() == Some(DC) && n.tag_name().name() == name)
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_string()
    }

    let title = dc_text(&doc, "title");
    let author = dc_text(&doc, "creator");

    // manifest: id -> item
    let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
    for n in doc.descendants().filter(|n| n.tag_name().name() == "item") {
        let (Some(id), Some(href)) = (n.attribute("id"), n.attribute("href")) else {
            continue;
        };
        manifest.insert(
            id.to_string(),
            ManifestItem {
                href: href.to_string(),
                media_type: n.attribute("media-type").unwrap_or("").to_string(),
                properties: n.attribute("properties").unwrap_or("").to_string(),
            },
        );
    }

    // spine order
    let opf_dir = dir_of(&opf_path);
    let mut spine_paths: Vec<String> = Vec::new();
    for n in doc.descendants().filter(|n| n.tag_name().name() == "itemref") {
        let Some(idref) = n.attribute("idref") else { continue };
        if let Some(item) = manifest.get(idref) {
            spine_paths.push(normalize_path(&opf_dir, &item.href));
        }
    }
    if spine_paths.is_empty() {
        return Err(AppError::msg("EPUB spine is empty"));
    }

    // TOC labels: EPUB3 nav doc and/or EPUB2 NCX.
    let mut titles: HashMap<String, String> = HashMap::new();
    let nav_href = manifest
        .values()
        .find(|i| i.properties.split_whitespace().any(|p| p == "nav"))
        .map(|i| normalize_path(&opf_dir, &i.href));
    if let Some(nav_path) = &nav_href {
        if let Some(raw) = read_entry(&mut zip, nav_path) {
            let nav = String::from_utf8_lossy(&raw);
            if let Ok(nd) = roxmltree::Document::parse(&nav) {
                for a in nd.descendants().filter(|n| n.tag_name().name() == "a") {
                    if let Some(href) = a.attribute("href") {
                        let label = a
                            .descendants()
                            .filter(|n| n.is_text())
                            .filter_map(|n| n.text())
                            .collect::<String>()
                            .trim()
                            .to_string();
                        if !label.is_empty() {
                            titles
                                .entry(normalize_path(&opf_dir, href))
                                .or_insert(label);
                        }
                    }
                }
            }
        }
    }
    let ncx_href = doc
        .descendants()
        .find(|n| n.tag_name().name() == "spine")
        .and_then(|s| s.attribute("toc"))
        .and_then(|id| manifest.get(id))
        .map(|i| normalize_path(&opf_dir, &i.href));
    if let Some(ncx_path) = &ncx_href {
        if let Some(raw) = read_entry(&mut zip, ncx_path) {
            let ncx = String::from_utf8_lossy(&raw);
            if let Ok(nd) = roxmltree::Document::parse(&ncx) {
                for np in nd.descendants().filter(|n| n.tag_name().name() == "navPoint") {
                    let Some(content) =
                        np.children().find(|n| n.tag_name().name() == "content")
                    else {
                        continue;
                    };
                    let Some(src) = content.attribute("src") else { continue };
                    let label = np
                        .descendants()
                        .find(|n| n.tag_name().name() == "text")
                        .and_then(|n| n.text())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !label.is_empty() {
                        titles.entry(normalize_path(&opf_dir, src)).or_insert(label);
                    }
                }
            }
        }
    }

    // cover image -> data URL for the UI
    let cover_id = doc
        .descendants()
        .find(|n| n.tag_name().name() == "meta" && n.attribute("name") == Some("cover"))
        .and_then(|n| n.attribute("content").map(str::to_string));
    let cover_path = cover_id
        .as_deref()
        .and_then(|id| manifest.get(id))
        .map(|i| normalize_path(&opf_dir, &i.href))
        .or_else(|| {
            manifest
                .values()
                .find(|i| {
                    i.properties.split_whitespace().any(|p| p == "cover-image")
                        && i.media_type.starts_with("image/")
                })
                .map(|i| normalize_path(&opf_dir, &i.href))
        });
    let cover_data_url = cover_path
        .as_deref()
        .and_then(|p| read_entry(&mut zip, p))
        .map(|img| {
            let mime = cover_path
                .as_deref()
                .and_then(|p| p.rsplit('.').next())
                .map(|ext| match ext {
                    "png" => "image/png",
                    "gif" => "image/gif",
                    "svg" => "image/svg+xml",
                    "webp" => "image/webp",
                    _ => "image/jpeg",
                })
                .unwrap_or("image/jpeg");
            let b64 = base64::engine::general_purpose::STANDARD.encode(&img);
            format!("data:{mime};base64,{b64}")
        });

    // parse spine content documents
    let mut docs: Vec<ContentDoc> = Vec::new();
    let mut total_chars = 0usize;
    for path in &spine_paths {
        let Some(raw) = read_entry(&mut zip, path) else { continue };
        let content = String::from_utf8_lossy(&raw).into_owned();
        let blocks = html::collect_blocks(&content);
        let has_content = blocks.iter().any(|b| !b.skipped);
        if !has_content {
            continue;
        }
        total_chars += blocks
            .iter()
            .filter(|b| !b.skipped)
            .map(|b| b.text.chars().count())
            .sum::<usize>();
        let title = titles
            .get(path)
            .cloned()
            .unwrap_or_else(|| pretty_name(path));
        docs.push(ContentDoc {
            path: path.clone(),
            title,
            html: content,
            blocks,
        });
    }

    let file_name = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .to_string();

    let info = BookInfo {
        format: "epub".into(),
        file_path: file_path.to_string(),
        file_name: file_name.clone(),
        title: if title.is_empty() {
            file_name.trim_end_matches(".epub").to_string()
        } else {
            title
        },

        author,
        cover_data_url,
        total_chars,
        segments: docs
            .iter()
            .map(|d| SegmentInfo {
                id: d.path.clone(),
                title: d.title.clone(),
                blocks: d.blocks.iter().filter(|b| !b.skipped).count(),
                chars: d
                    .blocks
                    .iter()
                    .filter(|b| !b.skipped)
                    .map(|b| b.text.chars().count())
                    .sum(),
            })
            .collect(),
    };

    Ok(LoadedBook {
        info,
        source: crate::types::BookSource::Epub { bytes, docs },
    })
}

fn pretty_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".xhtml")
        .trim_end_matches(".html")
        .trim_end_matches(".htm")
        .replace(['_', '-'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Block, MODE_BILINGUAL, MODE_TRANSLATED};
    use std::io::Write;

    fn sample_epub() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut out));
            let stored =
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            let deflated =
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let mut add = |name: &str, content: &str, opts: zip::write::SimpleFileOptions| {
                w.start_file(name, opts).unwrap();
                w.write_all(content.as_bytes()).unwrap();
            };
            add("mimetype", "application/epub+zip", stored);
            add(
                "META-INF/container.xml",
                r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
                deflated,
            );
            add(
                "OEBPS/content.opf",
                r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bid">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="bid">urn:test</dc:identifier>
<dc:title>Test Book</dc:title>
<dc:creator>Test Author</dc:creator>
<dc:language>en</dc:language>
</metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
<item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
<item id="cover" href="images/cover.png" media-type="image/png" properties="cover-image"/>
</manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#,
                deflated,
            );
            add(
                "OEBPS/nav.xhtml",
                r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>nav</title></head><body>
<nav xmlns:epub="http://www.idpf.org/2007/ops" epub:type="toc"><ol><li><a href="text/ch1.xhtml">One</a></li><li><a href="text/ch2.xhtml">Two</a></li></ol></nav>
</body></html>"#,
                deflated,
            );
            add(
                "OEBPS/text/ch1.xhtml",
                r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c1</title></head><body>
<h1>Chapter One</h1><p>Hello world, this is the first chapter.</p><p>42</p>
</body></html>"#,
                deflated,
            );
            add(
                "OEBPS/text/ch2.xhtml",
                r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c2</title></head><body>
<p>Second chapter &amp; more text here.</p><ul><li>Item one</li><li>Item two</li></ul>
</body></html>"#,
                deflated,
            );
            add("OEBPS/images/cover.png", "\u{89}PNG-fake", deflated);
            w.finish().unwrap();
        }
        out
    }

    #[test]
    fn round_trip_bilingual_epub() {
        let bytes = sample_epub();
        let mut book = parse(bytes.clone(), "/tmp/Test Book.epub").unwrap();

        assert_eq!(book.info.title, "Test Book");
        assert_eq!(book.info.author, "Test Author");
        assert_eq!(book.info.format, "epub");
        assert!(book.info.cover_data_url.is_some());
        assert_eq!(book.info.segments.len(), 2);
        assert_eq!(book.info.segments[0].title, "One"); // from nav TOC
        assert_eq!(book.info.segments[0].blocks, 2); // h1 + p; "42" skipped

        // Translate everything.
        let mut replacements = std::collections::HashMap::new();
        {
            let docs = crate::types::docs_of_mut(&mut book);
            for doc in docs {
                let translated_texts: Vec<String> = doc
                    .blocks
                    .iter()
                    .map(|b| format!("[MY] {}", b.text))
                    .collect();
                for (b, t) in doc.blocks.iter_mut().zip(translated_texts) {
                    if !b.skipped {
                        b.translation = Some(t);
                    }
                }
                let rewritten =
                    super::html::rewrite_doc(&doc.html, &doc.blocks, MODE_BILINGUAL).unwrap();
                replacements.insert(doc.path.clone(), rewritten);
            }
        }
        let out = super::build::repack(&bytes, &replacements).unwrap();

        // Output parses again, keeps originals, and carries translations.
        let mut reread = zip::ZipArchive::new(Cursor::new(out.as_slice())).unwrap();
        assert_eq!(reread.by_index(0).unwrap().name(), "mimetype");
        let ch1 = read_entry(&mut reread, "OEBPS/text/ch1.xhtml").unwrap();
        let ch1 = String::from_utf8(ch1).unwrap();
        assert!(ch1.contains("Hello world, this is the first chapter."), "{ch1}");
        assert!(
            ch1.contains("<p class=\"ebtr-translation\">[MY] Hello world"),
            "{ch1}"
        );
        assert!(!ch1.contains("[MY] 42"), "skipped block not translated: {ch1}");

        let parsed2 = parse(out, "/tmp/out.epub").unwrap();
        let docs2 = crate::types::docs_of(&parsed2);
        // The inserted translations must not be double-collected on re-run.
        let all_texts: Vec<String> = docs2.iter().flat_map(|d| d.blocks.clone()).map(|b: Block| b.text).collect();
        assert!(
            all_texts.iter().all(|t| !t.contains("[MY] [MY]")),
            "double translation detected"
        );
    }

    #[test]
    fn round_trip_translated_only_epub() {
        let bytes = sample_epub();
        let mut book = parse(bytes.clone(), "/tmp/Test Book.epub").unwrap();
        let mut replacements = std::collections::HashMap::new();
        {
            let docs = crate::types::docs_of_mut(&mut book);
            for doc in docs {
                for b in doc.blocks.iter_mut() {
                    if !b.skipped {
                        b.translation = Some(format!("[MY] {}", b.text));
                    }
                }
                let rewritten =
                    super::html::rewrite_doc(&doc.html, &doc.blocks, MODE_TRANSLATED).unwrap();
                replacements.insert(doc.path.clone(), rewritten);
            }
        }
        let out = super::build::repack(&bytes, &replacements).unwrap();
        let mut reread = zip::ZipArchive::new(Cursor::new(out.as_slice())).unwrap();
        let ch2 = read_entry(&mut reread, "OEBPS/text/ch2.xhtml").unwrap();
        let ch2 = String::from_utf8(ch2).unwrap();
        assert!(ch2.contains("<p>[MY] Second chapter"), "{ch2}");
        assert!(ch2.contains("<li>[MY] Item one</li>"), "{ch2}");
        // every paragraph/list item must start with the marker -> originals replaced
        for line in ch2.lines() {
            let t = line.trim();
            if t.starts_with("<p>") && t.ends_with("</p>") {
                assert!(t.contains("[MY] "), "unreplaced paragraph: {t}");
            }
        }
        assert!(ch2.contains("<li>"), "list markup preserved: {ch2}");
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize_path("OEBPS", "text/ch1.xhtml"), "OEBPS/text/ch1.xhtml");
        assert_eq!(normalize_path("OEBPS", "../cover.jpeg"), "cover.jpeg");
        assert_eq!(normalize_path("OEBPS", "/mimetype"), "mimetype");
        assert_eq!(normalize_path("", "a%20b.xhtml"), "a b.xhtml");
        assert_eq!(normalize_path("d", "ch1.xhtml#frag"), "d/ch1.xhtml");
    }
}
