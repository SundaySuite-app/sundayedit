//! Reference-document text extraction — Phase 3.4 (killer feature #2, mode 4).
//!
//! Modes 1–3 build the glossary from what the user types or from a first-pass
//! transcript. Mode 4 lets them point at the document they're already working
//! from — a sermon manuscript, lecture notes, a script — and seed the glossary
//! *before* transcription, so Whisper is primed on the proper nouns and jargon
//! that document contains.
//!
//! This module is only the extraction half: file → plain text. The LLM that
//! turns that text into candidate terms reuses the same propose-and-approve
//! pipeline as mode 3 (`llm::glossary_suggest`), so nothing here ever touches
//! the project.
//!
//! Discipline, mirroring the rest of `services`:
//!   - The XML→text parse (`extract_docx_xml_text`) and the prompt-budget
//!     truncation (`truncate_for_prompt`) are PURE and tested offline.
//!   - File/zip I/O is a thin shell around them.
//!
//! Formats: plain text (.txt/.md/.csv …) and Word (.docx, which is just an
//! OOXML zip — no new dependency, we already ship `zip` for DOCX *export*).
//! PDF is deliberately deferred: reliable PDF text extraction needs a heavy,
//! flaky dependency, and a half-working parser would violate "output quality
//! is non-negotiable." We fail with an actionable message instead.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};

/// Roughly 6k tokens — enough to cover a long sermon or lecture script while
/// keeping a single Haiku call comfortably under a cent. Longer documents are
/// truncated and the caller is told.
pub const MAX_DOC_CHARS: usize = 24_000;

/// How we'll read a given file, decided by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    /// UTF-8 plain text (.txt, .md, .csv, …).
    Text,
    /// Word OOXML (.docx) — a zip with the body in `word/document.xml`.
    Docx,
    /// PDF — recognized, but extraction is deliberately not supported yet.
    Pdf,
    /// Anything else.
    Unsupported,
}

impl DocFormat {
    /// Stable label for the UI / bindings.
    pub fn label(self) -> &'static str {
        match self {
            DocFormat::Text => "text",
            DocFormat::Docx => "docx",
            DocFormat::Pdf => "pdf",
            DocFormat::Unsupported => "unsupported",
        }
    }
}

/// The result handed back to the renderer after a successful extraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ExtractedDocument.ts")]
pub struct ExtractedDocument {
    /// File name only (no path) — for display.
    pub file_name: String,
    /// Which extractor ran: `"text"` or `"docx"`.
    pub format: String,
    /// The extracted text, already truncated to the prompt budget.
    pub text: String,
    /// Character count of `text` (post-truncation).
    pub char_count: usize,
    /// True when the source was longer than `MAX_DOC_CHARS` and got cut.
    pub truncated: bool,
}

/// Decide how to read a path purely from its extension. Pure.
pub fn detect_format(path: &Path) -> DocFormat {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt" | "text" | "md" | "markdown" | "csv" | "tsv" | "log" | "srt" | "vtt") => {
            DocFormat::Text
        }
        Some("docx") => DocFormat::Docx,
        Some("pdf") => DocFormat::Pdf,
        _ => DocFormat::Unsupported,
    }
}

/// Decode the five predefined XML entities. `&amp;` is undone last so an
/// escaped entity like `&amp;lt;` round-trips to the literal `&lt;`. Pure.
fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Pull readable text out of a Word `document.xml` body. Pure.
///
/// We collect the character data inside `<w:t>…</w:t>` runs and turn the
/// document's structure into whitespace: each paragraph end (`</w:p>`) becomes
/// a newline, `<w:tab/>` a tab, `<w:br/>`/`<w:cr/>` a newline. Word always
/// emits the `w:` namespace prefix, so matching it literally is safe.
pub fn extract_docx_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    let mut rest = xml;

    while let Some(lt) = rest.find('<') {
        // Character data preceding this tag belongs to the output only when
        // we're inside a <w:t> run.
        if in_text {
            let chunk = &rest[..lt];
            if !chunk.is_empty() {
                out.push_str(&decode_entities(chunk));
            }
        }

        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        rest = &after[gt + 1..];

        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        let name_src = tag.strip_prefix('/').unwrap_or(tag);
        let name: &str = name_src
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");

        match name {
            "w:t" => in_text = !closing && !self_closing,
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            "w:p" if closing => out.push('\n'),
            _ => {}
        }
    }

    out
}

/// Collapse the raw extractor output into tidy prose: normalize line endings,
/// trim trailing whitespace per line, and squeeze runs of blank lines down to
/// a single separator. Keeps paragraph breaks, drops the noise. Pure.
fn normalize_whitespace(s: &str) -> String {
    let mut out = String::new();
    let mut pending_blank = false;

    for line in s.replace('\r', "\n").lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            pending_blank = true;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if pending_blank {
                out.push('\n');
            }
        }
        pending_blank = false;
        out.push_str(trimmed);
    }

    out
}

/// Cap text at the prompt budget, cutting on a word boundary when possible.
/// Returns `(text, truncated)`. Pure.
pub fn truncate_for_prompt(text: &str) -> (String, bool) {
    if text.chars().count() <= MAX_DOC_CHARS {
        return (text.to_string(), false);
    }
    let mut cut: String = text.chars().take(MAX_DOC_CHARS).collect();
    // Prefer to end on whitespace so we don't slice a word in half, but only
    // if a boundary exists reasonably near the end.
    if let Some(idx) = cut.rfind(char::is_whitespace)
        && idx > MAX_DOC_CHARS * 9 / 10
    {
        cut.truncate(idx);
    }
    (cut.trim_end().to_string(), true)
}

/// Read a `.docx` file and return its extracted body text (pre-normalization).
fn read_docx_file(path: &Path) -> AppResult<String> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::Validation(format!("not a readable .docx (zip) file: {e}")))?;
    let mut entry = archive.by_name("word/document.xml").map_err(|_| {
        AppError::Validation(
            "this .docx has no word/document.xml — is it a real Word file?".to_string(),
        )
    })?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml)?;
    Ok(extract_docx_xml_text(&xml))
}

/// Extract plain text from a reference document, normalized but *not* yet
/// truncated. Errors with an actionable message for PDF and unknown types.
pub fn extract_text(path: &Path) -> AppResult<String> {
    let raw = match detect_format(path) {
        DocFormat::Text => {
            let bytes = std::fs::read(path)?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
        DocFormat::Docx => read_docx_file(path)?,
        DocFormat::Pdf => {
            return Err(AppError::Validation(
                "PDF reference documents aren't supported yet. Export or copy the document to \
                 .txt or .docx, or paste the key passage into the context description."
                    .to_string(),
            ));
        }
        DocFormat::Unsupported => {
            return Err(AppError::Validation(
                "Unsupported reference-document type. Use a plain-text file (.txt, .md) or a \
                 Word document (.docx)."
                    .to_string(),
            ));
        }
    };
    Ok(normalize_whitespace(&raw))
}

/// Full path → display-ready, truncated `ExtractedDocument`. The thin file-I/O
/// shell over the pure helpers above.
pub fn extract_document(path: &str) -> AppResult<ExtractedDocument> {
    let p = Path::new(path);
    let format = detect_format(p);
    let full = extract_text(p)?;
    let (text, truncated) = truncate_for_prompt(&full);
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    Ok(ExtractedDocument {
        file_name,
        format: format.label().to_string(),
        char_count: text.chars().count(),
        text,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_format_from_extension_case_insensitively() {
        assert_eq!(detect_format(Path::new("a.txt")), DocFormat::Text);
        assert_eq!(detect_format(Path::new("a.MD")), DocFormat::Text);
        assert_eq!(detect_format(Path::new("notes.docx")), DocFormat::Docx);
        assert_eq!(detect_format(Path::new("manuscript.DOCX")), DocFormat::Docx);
        assert_eq!(detect_format(Path::new("scan.pdf")), DocFormat::Pdf);
        assert_eq!(detect_format(Path::new("clip.mp4")), DocFormat::Unsupported);
        assert_eq!(detect_format(Path::new("noext")), DocFormat::Unsupported);
    }

    #[test]
    fn decodes_xml_entities_amp_last() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("&quot;hi&apos;"), "\"hi'");
        // Escaped entity round-trips to a literal entity, not a double-decode.
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn docx_xml_extracts_runs_and_paragraph_breaks() {
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>Pastor Bjørnstad</w:t></w:r></w:p>
            <w:p><w:r><w:t>spoke on kerygma</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let text = extract_docx_xml_text(&xml.replace('\n', ""));
        assert!(text.contains("Pastor Bjørnstad"));
        assert!(text.contains("kerygma"));
        // Two paragraphs → a newline between them.
        assert!(text.contains("Bjørnstad\nspoke") || text.contains("Bjørnstad\n\nspoke"));
    }

    #[test]
    fn docx_xml_joins_split_runs_within_a_paragraph() {
        // Word often splits a single word across runs; <w:t> boundaries must
        // not insert whitespace.
        let xml = "<w:p><w:r><w:t>sote</w:t></w:r><w:r><w:t>riologi</w:t></w:r></w:p>";
        assert!(extract_docx_xml_text(xml).contains("soteriologi"));
    }

    #[test]
    fn docx_xml_honours_preserve_space_attribute() {
        let xml = "<w:p><w:r><w:t xml:space=\"preserve\">a </w:t></w:r>\
                   <w:r><w:t>b</w:t></w:r></w:p>";
        assert!(extract_docx_xml_text(xml).contains("a b"));
    }

    #[test]
    fn docx_xml_ignores_text_outside_runs() {
        // Markup and attributes must never leak into the output.
        let xml = "<w:p w:rsidR=\"00ABCDEF\"><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
                   <w:r><w:t>only this</w:t></w:r></w:p>";
        let text = extract_docx_xml_text(xml);
        assert_eq!(text.trim(), "only this");
        assert!(!text.contains("00ABCDEF"));
        assert!(!text.contains("center"));
    }

    #[test]
    fn docx_xml_self_closing_text_run_is_empty() {
        // A stray self-closing <w:t/> must not flip us into text mode and
        // swallow following markup.
        let xml = "<w:p><w:r><w:t/></w:r><w:pPr/><w:r><w:t>real</w:t></w:r></w:p>";
        assert_eq!(extract_docx_xml_text(xml).trim(), "real");
    }

    #[test]
    fn normalize_collapses_blank_runs_and_trailing_space() {
        let messy = "Title   \n\n\n\nBody line\n   \n\nEnd";
        assert_eq!(normalize_whitespace(messy), "Title\n\nBody line\n\nEnd");
    }

    #[test]
    fn truncate_passes_short_text_through() {
        let (text, cut) = truncate_for_prompt("short enough");
        assert_eq!(text, "short enough");
        assert!(!cut);
    }

    #[test]
    fn truncate_caps_long_text_on_a_word_boundary() {
        // A long run of "word " — the cut should land on whitespace, not mid-word.
        let long = "word ".repeat(MAX_DOC_CHARS); // far over the cap
        let (text, cut) = truncate_for_prompt(&long);
        assert!(cut);
        assert!(text.chars().count() <= MAX_DOC_CHARS);
        assert!(!text.ends_with("wor")); // no half-word at the end
    }

    #[test]
    fn extract_text_reads_a_plain_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "Kerygma og soteriologi.\n").unwrap();
        let text = extract_text(&path).unwrap();
        assert_eq!(text, "Kerygma og soteriologi.");
    }

    #[test]
    fn extract_text_rejects_pdf_with_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.pdf");
        std::fs::write(&path, b"%PDF-1.7\n").unwrap();
        let err = extract_text(&path).unwrap_err();
        assert_eq!(err.code(), "validation");
        assert!(err.to_string().contains(".docx"));
    }

    #[test]
    fn extract_document_reads_a_real_docx_zip() {
        // Build a minimal valid .docx (a zip with word/document.xml) using the
        // same `zip` crate we ship, then round-trip it through extraction.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manuscript.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("word/document.xml", opts).unwrap();
        zw.write_all(
            b"<w:document><w:body><w:p><w:r><w:t>Bonhoeffer on discipleship</w:t></w:r></w:p>\
              </w:body></w:document>",
        )
        .unwrap();
        zw.finish().unwrap();

        let doc = extract_document(path.to_str().unwrap()).unwrap();
        assert_eq!(doc.format, "docx");
        assert_eq!(doc.file_name, "manuscript.docx");
        assert!(doc.text.contains("Bonhoeffer on discipleship"));
        assert!(!doc.truncated);
        assert_eq!(doc.char_count, doc.text.chars().count());
    }
}
