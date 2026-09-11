use crate::error::Result;
use crate::types::{Block, ContentDoc, MODE_BILINGUAL};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

fn stored_opts() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn deflated_opts() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

/// Rebuild the original EPUB, swapping in rewritten content documents.
/// The `mimetype` entry is always written first and stored uncompressed.
/// Replacement keys are decoded OPF paths; archive entry names may legally
/// be stored percent-encoded, so matching goes through `entry_key`.
/// Returns the bytes plus how many replacements matched no entry.
pub fn repack(
    original: &[u8],
    replacements: &HashMap<String, String>,
) -> Result<(Vec<u8>, usize)> {
    let mut reader = zip::ZipArchive::new(Cursor::new(original))?;
    let mut out = Vec::new();
    let mut applied = 0usize;
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
        writer.start_file("mimetype", stored_opts())?;
        writer.write_all(b"application/epub+zip")?;

        let mut names: Vec<String> = reader.file_names().map(str::to_string).collect();
        names.retain(|n| n != "mimetype");
        for name in names {
            let key = crate::epub::entry_key(&name);
            let content: Vec<u8> = match replacements
                .get(name.as_str())
                .or_else(|| replacements.get(&key))
            {
                Some(html) => {
                    applied += 1;
                    html.clone().into_bytes()
                }
                None => {
                    let mut f = reader.by_name(&name)?;
                    let mut buf = Vec::with_capacity(f.size() as usize);
                    f.read_to_end(&mut buf)?;
                    buf
                }
            };
            writer.start_file(name.as_str(), deflated_opts())?;
            writer.write_all(&content)?;
        }
        writer.finish()?;
    }
    Ok((out, replacements.len().saturating_sub(applied)))
}

fn xml_escape(s: &str) -> String {
    crate::epub::html::escape_html(s)
}

/// Best-effort map of language display names to BCP-47 codes for the
/// generated package metadata.
pub fn lang_code(lang: &str) -> String {
    let l = lang.to_lowercase();
    let code = if l.contains("burmese") || l.contains("myanmar") {
        "my"
    } else if l.contains("english") {
        "en"
    } else if l.contains("chinese") {
        if l.contains("traditional") { "zh-TW" } else { "zh-CN" }
    } else if l.contains("japanese") {
        "ja"
    } else if l.contains("korean") {
        "ko"
    } else if l.contains("thai") {
        "th"
    } else if l.contains("vietnamese") {
        "vi"
    } else if l.contains("french") {
        "fr"
    } else if l.contains("german") {
        "de"
    } else if l.contains("spanish") {
        "es"
    } else if l.contains("hindi") {
        "hi"
    } else if l.contains("indonesian") {
        "id"
    } else if l.contains("arabic") {
        "ar"
    } else if l.contains("russian") {
        "ru"
    } else {
        "en"
    };
    code.to_string()
}

fn xhtml_doc(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
<head><title>{}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
<body>\n{}</body>\n</html>\n",
        xml_escape(title),
        body
    )
}

fn paragraphs_html(blocks: &[Block], mode: &str) -> String {
    let mut body = String::new();
    for b in blocks {
        if b.skipped {
            if !b.text.is_empty() {
                body.push_str(&format!("<p>{}</p>\n", xml_escape(&b.text)));
            }
            continue;
        }
        let original = xml_escape(&b.text);
        let translated = b.translation.as_deref().map(xml_escape);
        match (mode, translated) {
            (MODE_BILINGUAL, Some(tr)) => {
                body.push_str(&format!(
                    "<p>{original}</p>\n<p class=\"ebtr-translation\">{tr}</p>\n"
                ));
            }
            (_, Some(tr)) => body.push_str(&format!("<p>{tr}</p>\n")),
            _ => body.push_str(&format!("<p>{original}</p>\n")),
        }
    }
    body
}

/// Build a brand-new EPUB from generated chapter documents (PDF pipeline).
pub fn build_new_epub(
    title: &str,
    author: &str,
    lang_name: &str,
    docs: &[ContentDoc],
    mode: &str,
) -> Result<Vec<u8>> {
    let code = lang_code(lang_name);
    let stamp = "2026-01-01T00:00:00Z";

    let mut items = String::new();
    let mut refs = String::new();
    let mut nav_list = String::new();
    let mut ncx_points = String::new();
    for (i, doc) in docs.iter().enumerate() {
        let file = format!("ch{:03}.xhtml", i + 1);
        items.push_str(&format!(
            "<item id=\"ch{i}\" href=\"{file}\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        refs.push_str(&format!("<itemref idref=\"ch{i}\"/>\n"));
        nav_list.push_str(&format!(
            "<li><a href=\"{file}\">{}</a></li>\n",
            xml_escape(&doc.title)
        ));
        ncx_points.push_str(&format!(
            "<navPoint id=\"np{i}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel>\
<content src=\"{file}\"/></navPoint>\n",
            i + 1,
            xml_escape(&doc.title)
        ));
    }

    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\">\n\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
<dc:identifier id=\"bookid\">urn:ebook-translator:{}</dc:identifier>\n\
<dc:title>{}</dc:title>\n\
<dc:creator>{}</dc:creator>\n\
<dc:language>{code}</dc:language>\n\
<meta property=\"dcterms:modified\">{stamp}</meta>\n\
</metadata>\n\
<manifest>\n\
<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n\
{items}\
</manifest>\n\
<spine toc=\"ncx\">\n{refs}</spine>\n\
</package>\n",
        xml_escape(&format!("{title}-{code}")),
        xml_escape(title),
        xml_escape(author),
    );

    let nav = xhtml_doc(
        title,
        &format!(
            "<nav epub:type=\"toc\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n\
<h1>{}</h1>\n<ol>\n{nav_list}</ol>\n</nav>\n",
            xml_escape(title)
        ),
    );

    let ncx = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
<head><meta name=\"dtb:uid\" content=\"urn:ebook-translator:{0}\"/></head>\n\
<docTitle><text>{1}</text></docTitle>\n\
<navMap>\n{ncx_points}</navMap>\n</ncx>\n",
        xml_escape(&format!("{title}-{code}")),
        xml_escape(title),
    );

    let mut out = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
        writer.start_file("mimetype", stored_opts())?;
        writer.write_all(b"application/epub+zip")?;

        let mut add = |name: &str, content: &str| -> Result<()> {
            writer.start_file(name, deflated_opts())?;
            writer.write_all(content.as_bytes())?;
            Ok(())
        };

        add(
            "META-INF/container.xml",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
<rootfiles><rootfile full-path=\"OEBPS/content.opf\" \
media-type=\"application/oebps-package+xml\"/></rootfiles>\n</container>\n",
        )?;
        add("OEBPS/content.opf", &opf)?;
        add("OEBPS/nav.xhtml", &nav)?;
        add("OEBPS/toc.ncx", &ncx)?;
        add(
            "OEBPS/style.css",
            "body{font-family:serif;line-height:1.6;margin:1.2em;}\n\
.ebtr-translation{margin:.45em 0;}\n",
        )?;
        for (i, doc) in docs.iter().enumerate() {
            let body = paragraphs_html(&doc.blocks, mode);
            add(&format!("OEBPS/ch{:03}.xhtml", i + 1), &xhtml_doc(&doc.title, &body))?;
        }
        writer.finish()?;
    }
    Ok(out)
}
