//
//       user/editor/src/io_docx.rs
//       Pure-Rust bidirectional OOXML (.docx) reader and writer
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crate::doc::{
    normalize_runs, Align, Block, BlockKind, Document, FontFamily, ImageBlock, MarginPreset,
    Orientation, PageMargins, PageSetup, PageSize, Run, TableBlock, TableCell, TextStyle,
    PX_PER_MM,
};
use crate::zip_mini::{ZipArchive, ZipWriter};
use roxmltree::{Document as XmlDoc, Node};
use spui_ui::icons;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::Arc;

const WML_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";

const EMU_PER_PX: i64 = 9525; // 914400 / 96

pub fn save_docx(doc: &Document, path: &Path) -> Result<(), String> {
    let bytes = encode_docx_bytes(doc)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    fs::write(path, bytes).map_err(|e| format!("写入 .docx 失败: {e}"))
}

pub fn load_docx(path: &Path) -> Result<Document, String> {
    let bytes = fs::read(path).map_err(|e| format!("读取 .docx 失败: {e}"))?;
    let mut doc = decode_docx_bytes(&bytes)?;
    doc.path = Some(path.to_path_buf());
    doc.dirty = false;
    Ok(doc)
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn jc_name(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "both",
    }
}

fn nearest_docx_highlight(rgb: u32) -> &'static str {
    match rgb {
        0xfef08a | 0xfde047 => "yellow",
        0xbbf7d0 | 0x86efac => "green",
        0xbfdbfe | 0x93c5fd | 0xa5f3fc => "cyan",
        0xfecdd3 | 0xfda4af | 0xfbcfe8 => "magenta",
        0xe2e8f0 | 0xcbd5e1 => "lightGray",
        _ => "yellow",
    }
}

fn highlight_to_rgb(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "yellow" | "darkyellow" => Some(0xfef08a),
        "green" => Some(0xbbf7d0),
        "cyan" | "blue" => Some(0xbfdbfe),
        "magenta" | "red" => Some(0xfecdd3),
        "lightgray" | "darkgray" => Some(0xe2e8f0),
        "none" => None,
        _ => parse_hex6(name),
    }
}

fn parse_hex6(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() == 6 {
        u32::from_str_radix(hex, 16).ok()
    } else {
        None
    }
}

fn write_runs_xml(out: &mut String, runs: &[Run]) {
    for run in runs {
        if run.text.is_empty() && runs.len() > 1 {
            continue;
        }
        out.push_str("<w:r><w:rPr>");
        let f = run.style.font.docx_name();
        let _ = write!(
            out,
            "<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:eastAsia=\"{f}\"/>"
        );
        if run.style.bold {
            out.push_str("<w:b/>");
        }
        if run.style.italic {
            out.push_str("<w:i/>");
        }
        if run.style.underline {
            out.push_str("<w:u w:val=\"single\"/>");
        }
        if run.style.strikethrough {
            out.push_str("<w:strike/>");
        }
        let half_pts = (run.style.size_pt * 2.0).round().clamp(12.0, 144.0) as u32;
        let _ = write!(out, "<w:sz w:val=\"{half_pts}\"/><w:szCs w:val=\"{half_pts}\"/>");
        let _ = write!(out, "<w:color w:val=\"{:06X}\"/>", run.style.color & 0xff_ffff);
        if let Some(bg) = run.style.highlight {
            let hl = nearest_docx_highlight(bg);
            let _ = write!(
                out,
                "<w:highlight w:val=\"{hl}\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{:06X}\"/>",
                bg & 0xff_ffff
            );
        }
        out.push_str("</w:rPr>");
        let escaped = xml_escape(&run.text);
        let needs_preserve = run.text.starts_with(' ') || run.text.ends_with(' ') || run.text.contains("  ");
        if needs_preserve {
            let _ = write!(out, "<w:t xml:space=\"preserve\">{escaped}</w:t>");
        } else {
            let _ = write!(out, "<w:t>{escaped}</w:t>");
        }
        out.push_str("</w:r>");
    }
}

struct MediaOutput<'a> {
    rel_id: String,
    target: String,
    ext: &'static str,
    mime: &'static str,
    bytes: &'a [u8],
}

pub fn encode_docx_bytes(doc: &Document) -> Result<Vec<u8>, String> {
    let mut media: Vec<MediaOutput<'_>> = Vec::new();
    let mut media_seq = 1usize;

    let mut body = String::new();
    for blk in &doc.blocks {
        match &blk.kind {
            BlockKind::Paragraph
            | BlockKind::Heading(_)
            | BlockKind::BulletList
            | BlockKind::NumberedList
            | BlockKind::Quote => {
                body.push_str("<w:p><w:pPr>");
                match blk.kind {
                    BlockKind::Heading(1) => body.push_str("<w:pStyle w:val=\"Heading1\"/>"),
                    BlockKind::Heading(2) => body.push_str("<w:pStyle w:val=\"Heading2\"/>"),
                    BlockKind::Heading(_) => body.push_str("<w:pStyle w:val=\"Heading3\"/>"),
                    BlockKind::Quote => body.push_str(
                        "<w:pStyle w:val=\"Quote\"/><w:ind w:left=\"360\"/><w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"12\" w:color=\"3B82F6\"/></w:pBdr>",
                    ),
                    BlockKind::BulletList => body.push_str(
                        "<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>",
                    ),
                    BlockKind::NumberedList => body.push_str(
                        "<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"2\"/></w:numPr>",
                    ),
                    _ => {}
                }
                let _ = write!(body, "<w:jc w:val=\"{}\"/>", jc_name(blk.align));
                let after_twips = (blk.space_after_pt * 20.0).round() as u32;
                let line_240 = (blk.line_spacing * 240.0).round().clamp(200.0, 600.0) as u32;
                let _ = write!(
                    body,
                    "<w:spacing w:after=\"{after_twips}\" w:line=\"{line_240}\" w:lineRule=\"auto\"/>"
                );
                body.push_str("</w:pPr>");
                write_runs_xml(&mut body, &blk.runs);
                body.push_str("</w:p>");
            }
            BlockKind::Divider => {
                body.push_str(
                    "<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"8\" w:space=\"4\" w:color=\"CBD5E1\"/></w:pBdr><w:spacing w:before=\"120\" w:after=\"120\"/></w:pPr></w:p>",
                );
            }
            BlockKind::Table(tbl) => {
                body.push_str("<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>\
                    <w:top w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"CBD5E1\"/>\
                    <w:left w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"CBD5E1\"/>\
                    <w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"CBD5E1\"/>\
                    <w:right w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"CBD5E1\"/>\
                    <w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"E2E8F0\"/>\
                    <w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"E2E8F0\"/>\
                    </w:tblBorders></w:tblPr>");
                body.push_str("<w:tblGrid>");
                let cols = tbl.col_widths.len().max(tbl.col_count()).max(1);
                for c in 0..cols {
                    let px_w = tbl.col_widths.get(c).copied().unwrap_or(140.0);
                    let twips = (px_w * 15.0).round().max(480.0) as u32;
                    let _ = write!(body, "<w:gridCol w:w=\"{twips}\"/>");
                }
                body.push_str("</w:tblGrid>");
                for row in &tbl.rows {
                    body.push_str("<w:tr>");
                    for (c_idx, cell) in row.iter().enumerate() {
                        let px_w = tbl.col_widths.get(c_idx).copied().unwrap_or(140.0);
                        let twips = (px_w * 15.0).round().max(480.0) as u32;
                        body.push_str("<w:tc><w:tcPr>");
                        let _ = write!(body, "<w:tcW w:w=\"{twips}\" w:type=\"dxa\"/>");
                        if let Some(fill) = cell.shading {
                            let _ = write!(
                                body,
                                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{:06X}\"/>",
                                fill & 0xff_ffff
                            );
                        }
                        body.push_str("</w:tcPr><w:p><w:pPr>");
                        let _ = write!(body, "<w:jc w:val=\"{}\"/>", jc_name(cell.align));
                        body.push_str("<w:spacing w:after=\"40\"/></w:pPr>");
                        write_runs_xml(&mut body, &cell.runs);
                        body.push_str("</w:p></w:tc>");
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
            BlockKind::Image(img) => {
                let is_png = img.mime == "image/png" || img.bytes.starts_with(b"\x89PNG");
                let ext = if is_png { "png" } else { "jpg" };
                let mime = if is_png { "image/png" } else { "image/jpeg" };
                let rel_id = format!("rIdImg{media_seq}");
                let target = format!("media/image{media_seq}.{ext}");
                let doc_id = media_seq;
                media_seq += 1;
                media.push(MediaOutput {
                    rel_id: rel_id.clone(),
                    target,
                    ext,
                    mime,
                    bytes: &img.bytes,
                });

                let (dw, dh) = img.display_size(doc.setup.body_width_px());
                let cx = (dw as i64) * EMU_PER_PX;
                let cy = (dh as i64) * EMU_PER_PX;
                let fname = xml_escape(&img.filename);
                let alt = xml_escape(&img.caption);
                let _ = write!(
                    body,
                    "<w:p><w:pPr><w:jc w:val=\"{}\"/><w:spacing w:after=\"80\"/></w:pPr><w:r><w:drawing>\
                    <wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
                    <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
                    <wp:docPr id=\"{doc_id}\" name=\"{fname}\" descr=\"{alt}\"/>\
                    <a:graphic xmlns:a=\"{A_NS}\"><a:graphicData uri=\"{PIC_NS}\">\
                    <pic:pic xmlns:pic=\"{PIC_NS}\">\
                    <pic:nvPicPr><pic:cNvPr id=\"{doc_id}\" name=\"{fname}\" descr=\"{alt}\"/><pic:cNvPicPr/></pic:nvPicPr>\
                    <pic:blipFill><a:blip r:embed=\"{rel_id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
                    <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
                    </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>",
                    jc_name(img.align)
                );
                if !img.caption.trim().is_empty() {
                    let _ = write!(
                        body,
                        "<w:p><w:pPr><w:pStyle w:val=\"Caption\"/><w:jc w:val=\"{}\"/><w:spacing w:after=\"120\"/></w:pPr>",
                        jc_name(img.align)
                    );
                    write_runs_xml(
                        &mut body,
                        &[Run::new(
                            &img.caption,
                            TextStyle {
                                italic: true,
                                size_pt: 9.5,
                                color: 0x64748b,
                                ..TextStyle::default()
                            },
                        )],
                    );
                    body.push_str("</w:p>");
                }
            }
        }
    }

    // Section properties (page size & margins + header/footer refs)
    let (tw_w, tw_h) = doc.setup.size.dimensions_twips();
    let (pg_w, pg_h, orient_attr) = match doc.setup.orientation {
        Orientation::Portrait => (tw_w, tw_h, ""),
        Orientation::Landscape => (tw_h, tw_w, " w:orient=\"landscape\""),
    };
    let mm_to_twips = |mm: f32| -> u32 { (mm * 56.6929).round().max(180.0) as u32 };
    let top_t = mm_to_twips(doc.setup.margins.top_mm);
    let bot_t = mm_to_twips(doc.setup.margins.bottom_mm);
    let left_t = mm_to_twips(doc.setup.margins.left_mm);
    let right_t = mm_to_twips(doc.setup.margins.right_mm);
    let _ = write!(
        body,
        "<w:sectPr>\
        <w:headerReference w:type=\"default\" r:id=\"rIdHdr1\"/>\
        <w:footerReference w:type=\"default\" r:id=\"rIdFtr1\"/>\
        <w:pgSz w:w=\"{pg_w}\" w:h=\"{pg_h}\"{orient_attr}/>\
        <w:pgMar w:top=\"{top_t}\" w:right=\"{right_t}\" w:bottom=\"{bot_t}\" w:left=\"{left_t}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
        </w:sectPr>"
    );

    let document_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <w:document xmlns:w=\"{WML_NS}\" xmlns:r=\"{REL_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\">\
        <w:body>{body}</w:body></w:document>"
    );

    let mut hdr_xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    let _ = write!(
        hdr_xml,
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:sz w:val=\"18\"/><w:color w:val=\"64748B\"/></w:rPr><w:t>{}</w:t></w:r></w:p></w:hdr>",
        xml_escape(&doc.setup.header)
    );

    let footer_text = if doc.setup.show_page_numbers && !doc.setup.footer.is_empty() {
        doc.setup.footer.clone()
    } else {
        doc.setup.footer.clone()
    };
    let mut ftr_xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:ftr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    let _ = write!(
        ftr_xml,
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:rPr><w:sz w:val=\"18\"/><w:color w:val=\"64748B\"/></w:rPr><w:t>{}</w:t></w:r></w:p></w:ftr>",
        xml_escape(&footer_text)
    );

    let mut content_types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>",
    );
    let mut seen_exts = Vec::new();
    for m in &media {
        if !seen_exts.contains(&m.ext) {
            seen_exts.push(m.ext);
            let _ = write!(
                content_types,
                "<Default Extension=\"{}\" ContentType=\"{}\"/>",
                m.ext, m.mime
            );
        }
    }
    content_types.push_str(
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
        <Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>\
        <Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>\
        <Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/>\
        </Types>",
    );

    let pkg_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";

    let mut doc_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rIdSty\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
        <Relationship Id=\"rIdNum\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>\
        <Relationship Id=\"rIdHdr1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/header\" Target=\"header1.xml\"/>\
        <Relationship Id=\"rIdFtr1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer\" Target=\"footer1.xml\"/>",
    );
    for m in &media {
        let _ = write!(
            doc_rels,
            "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>",
            m.rel_id, m.target
        );
    }
    doc_rels.push_str("</Relationships>");

    let styles_xml = build_styles_xml();
    let numbering_xml = build_numbering_xml();

    let mut zip = ZipWriter::new();
    zip.add_deflated("[Content_Types].xml", content_types.as_bytes())?;
    zip.add_deflated("_rels/.rels", pkg_rels.as_bytes())?;
    zip.add_deflated("word/_rels/document.xml.rels", doc_rels.as_bytes())?;
    zip.add_deflated("word/styles.xml", styles_xml.as_bytes())?;
    zip.add_deflated("word/numbering.xml", numbering_xml.as_bytes())?;
    zip.add_deflated("word/header1.xml", hdr_xml.as_bytes())?;
    zip.add_deflated("word/footer1.xml", ftr_xml.as_bytes())?;
    zip.add_deflated("word/document.xml", document_xml.as_bytes())?;
    for m in &media {
        zip.add_stored(&format!("word/{}", m.target), m.bytes);
    }
    Ok(zip.finish())
}

fn build_styles_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
    <w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
    <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Noto Sans CJK SC\" w:eastAsia=\"Noto Sans CJK SC\" w:hAnsi=\"Noto Sans CJK SC\"/><w:sz w:val=\"23\"/><w:szCs w:val=\"23\"/><w:color w:val=\"1E293B\"/></w:rPr></w:rPrDefault></w:docDefaults>\
    <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:b/><w:sz w:val=\"40\"/><w:color w:val=\"0F172A\"/></w:rPr></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:b/><w:sz w:val=\"30\"/><w:color w:val=\"1E3A8A\"/></w:rPr></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:b/><w:sz w:val=\"25\"/><w:color w:val=\"334155\"/></w:rPr></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:i/><w:sz w:val=\"22\"/><w:color w:val=\"475569\"/></w:rPr></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/></w:style>\
    <w:style w:type=\"paragraph\" w:styleId=\"Caption\"><w:name w:val=\"caption\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:i/><w:sz w:val=\"19\"/><w:color w:val=\"64748B\"/></w:rPr></w:style>\
    </w:styles>"
}

fn build_numbering_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
    <w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
    <w:abstractNum w:abstractNumId=\"0\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/></w:lvl></w:abstractNum>\
    <w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl></w:abstractNum>\
    <w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>\
    <w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>\
    </w:numbering>"
}

// ---------- DOCX Reader ----------

fn attr_w<'a>(node: Node<'a, 'a>, local: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == local)
        .map(|a| a.value())
}

fn child_by_tag<'a>(node: Node<'a, 'a>, tag: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == tag)
}

fn collect_all_text(node: Node<'_, '_>, buf: &mut String) {
    if node.is_element() {
        let name = node.tag_name().name();
        if name == "t" || name == "delText" {
            if let Some(t) = node.text() {
                buf.push_str(t);
            }
            return;
        }
        if name == "tab" {
            buf.push('\t');
            return;
        }
        if name == "br" {
            buf.push('\n');
            return;
        }
    }
    for ch in node.children() {
        collect_all_text(ch, buf);
    }
}

fn parse_rels(zip: &ZipArchive, path: &str) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    let Some(xml_str) = zip.get_str(path) else {
        return map;
    };
    let Ok(xml) = XmlDoc::parse(&xml_str) else {
        return map;
    };
    for node in xml.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let id = node.attribute("Id").unwrap_or_default().to_string();
        let target = node.attribute("Target").unwrap_or_default().to_string();
        let kind = node.attribute("Type").unwrap_or_default().to_string();
        if !id.is_empty() && !target.is_empty() {
            map.insert(id, (kind, target));
        }
    }
    map
}

fn parse_numbering(zip: &ZipArchive) -> HashMap<String, bool> {
    // numId -> true if numbered (decimal), false if bullet
    let mut abs_fmt: HashMap<String, bool> = HashMap::new();
    let mut num_map: HashMap<String, bool> = HashMap::new();
    let Some(xml_str) = zip.get_str("word/numbering.xml") else {
        return num_map;
    };
    let Ok(xml) = XmlDoc::parse(&xml_str) else {
        return num_map;
    };
    for abs in xml.descendants().filter(|n| n.has_tag_name("abstractNum")) {
        let aid = attr_w(abs, "abstractNumId").unwrap_or_default().to_string();
        let mut is_ordered = false;
        if let Some(lvl) = child_by_tag(abs, "lvl") {
            if let Some(nf) = child_by_tag(lvl, "numFmt") {
                let v = attr_w(nf, "val").unwrap_or("bullet");
                is_ordered = v != "bullet";
            }
        }
        abs_fmt.insert(aid, is_ordered);
    }
    for num in xml.descendants().filter(|n| n.has_tag_name("num")) {
        let nid = attr_w(num, "numId").unwrap_or_default().to_string();
        if let Some(aid_node) = child_by_tag(num, "abstractNumId") {
            let aid = attr_w(aid_node, "val").unwrap_or_default();
            num_map.insert(nid, abs_fmt.get(aid).copied().unwrap_or(false));
        }
    }
    num_map
}

fn parse_rpr(rpr: Option<Node<'_, '_>>, base: TextStyle) -> TextStyle {
    let mut st = base;
    let Some(rpr) = rpr else {
        return st;
    };
    for child in rpr.children().filter(|c| c.is_element()) {
        match child.tag_name().name() {
            "b" => {
                let val = attr_w(child, "val").unwrap_or("1");
                st.bold = val != "0" && !val.eq_ignore_ascii_case("false");
            }
            "i" => {
                let val = attr_w(child, "val").unwrap_or("1");
                st.italic = val != "0" && !val.eq_ignore_ascii_case("false");
            }
            "u" => {
                let val = attr_w(child, "val").unwrap_or("single");
                st.underline = !val.eq_ignore_ascii_case("none");
            }
            "strike" => {
                let val = attr_w(child, "val").unwrap_or("1");
                st.strikethrough = val != "0" && !val.eq_ignore_ascii_case("false");
            }
            "rFonts" => {
                if let Some(name) = attr_w(child, "eastAsia")
                    .or_else(|| attr_w(child, "ascii"))
                    .or_else(|| attr_w(child, "hAnsi"))
                {
                    st.font = FontFamily::from_docx_name(name);
                }
            }
            "sz" | "szCs" => {
                if let Some(v) = attr_w(child, "val").and_then(|s| s.parse::<f32>().ok()) {
                    st.size_pt = (v * 0.5).clamp(7.0, 48.0);
                }
            }
            "color" => {
                if let Some(rgb) = attr_w(child, "val").and_then(parse_hex6) {
                    st.color = rgb;
                }
            }
            "highlight" => {
                if let Some(val) = attr_w(child, "val") {
                    st.highlight = highlight_to_rgb(val);
                }
            }
            "shd" => {
                if st.highlight.is_none() {
                    if let Some(rgb) = attr_w(child, "fill").and_then(parse_hex6) {
                        st.highlight = Some(rgb);
                    }
                }
            }
            _ => {}
        }
    }
    st
}

fn parse_paragraph_node(
    p_node: Node<'_, '_>,
    num_map: &HashMap<String, bool>,
    rels: &HashMap<String, (String, String)>,
    zip: &ZipArchive,
    next_img_id: &mut u64,
    default_body_w: i32,
) -> Vec<Block> {
    let ppr = child_by_tag(p_node, "pPr");
    let mut kind = BlockKind::Paragraph;
    let mut align = Align::Left;
    let mut line_spacing = 1.35f32;
    let mut space_after_pt = 6.0f32;
    let mut is_divider = false;
    let mut is_caption = false;

    if let Some(ppr) = ppr {
        if let Some(style) = child_by_tag(ppr, "pStyle") {
            let sid = attr_w(style, "val").unwrap_or_default().to_ascii_lowercase();
            if sid == "heading1" || sid == "1" || sid == "title" {
                kind = BlockKind::Heading(1);
                space_after_pt = 10.0;
            } else if sid == "heading2" || sid == "2" || sid == "subtitle" {
                kind = BlockKind::Heading(2);
                space_after_pt = 8.0;
            } else if sid.starts_with("heading") || sid == "3" {
                kind = BlockKind::Heading(3);
                space_after_pt = 6.0;
            } else if sid.contains("quote") {
                kind = BlockKind::Quote;
            } else if sid.contains("caption") {
                is_caption = true;
            }
        }
        if let Some(num_pr) = child_by_tag(ppr, "numPr") {
            let nid = child_by_tag(num_pr, "numId")
                .and_then(|n| attr_w(n, "val"))
                .unwrap_or("1");
            if num_map.get(nid).copied().unwrap_or(false) {
                kind = BlockKind::NumberedList;
            } else {
                kind = BlockKind::BulletList;
            }
            space_after_pt = 3.5;
        }
        if let Some(jc) = child_by_tag(ppr, "jc") {
            align = match attr_w(jc, "val").unwrap_or("left") {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                "both" => Align::Justify,
                _ => Align::Left,
            };
        }
        if let Some(sp) = child_by_tag(ppr, "spacing") {
            if let Some(after) = attr_w(sp, "after").and_then(|v| v.parse::<f32>().ok()) {
                space_after_pt = (after / 20.0).clamp(0.0, 36.0);
            }
            if let Some(line) = attr_w(sp, "line").and_then(|v| v.parse::<f32>().ok()) {
                line_spacing = (line / 240.0).clamp(1.0, 2.5);
            }
        }
        if let Some(bdr) = child_by_tag(ppr, "pBdr") {
            if child_by_tag(bdr, "bottom").is_some() {
                is_divider = true;
            }
            if child_by_tag(bdr, "left").is_some() && matches!(kind, BlockKind::Paragraph) {
                kind = BlockKind::Quote;
            }
        }
    }

    let base_style = kind.default_style();
    let mut runs = Vec::new();
    let mut images = Vec::new();

    for r_node in p_node.children().filter(|c| c.is_element() && c.tag_name().name() == "r") {
        let r_style = parse_rpr(child_by_tag(r_node, "rPr"), base_style);
        // Check for embedded drawing / image inside this run.
        if let Some(drawing) = child_by_tag(r_node, "drawing") {
            if let Some(blip) = drawing.descendants().find(|d| d.has_tag_name("blip")) {
                if let Some(rid) = attr_w(blip, "embed") {
                    if let Some((_, target)) = rels.get(rid) {
                        let full = if target.starts_with("word/") {
                            target.clone()
                        } else {
                            format!("word/{}", target.trim_start_matches('/'))
                        };
                        if let Some(bytes) = zip.get(&full) {
                            if let Some(sprite) = icons::decode_image_bytes(bytes) {
                                let ext_w = drawing
                                    .descendants()
                                    .find(|d| d.has_tag_name("extent"))
                                    .and_then(|e| e.attribute("cx"))
                                    .and_then(|v| v.parse::<i64>().ok())
                                    .map(|emu| (emu / EMU_PER_PX) as u32)
                                    .unwrap_or(sprite.width);
                                let descr = drawing
                                    .descendants()
                                    .find(|d| d.has_tag_name("docPr"))
                                    .and_then(|e| e.attribute("descr"))
                                    .unwrap_or("")
                                    .to_string();
                                let mime = if bytes.starts_with(b"\x89PNG") {
                                    "image/png"
                                } else {
                                    "image/jpeg"
                                };
                                let fname = Path::new(&full)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "image.png".into());
                                let id = *next_img_id;
                                *next_img_id += 1;
                                images.push(ImageBlock {
                                    id,
                                    filename: fname,
                                    mime: mime.to_string(),
                                    bytes: Arc::new(bytes.to_vec()),
                                    intrinsic_w: sprite.width.max(1),
                                    intrinsic_h: sprite.height.max(1),
                                    display_w: ext_w.clamp(64, default_body_w.max(120) as u32),
                                    caption: descr,
                                    align,
                                });
                            }
                        }
                    }
                }
            }
        }

        let mut buf = String::new();
        for sub in r_node.children().filter(|c| c.is_element()) {
            match sub.tag_name().name() {
                "t" | "delText" => {
                    if let Some(t) = sub.text() {
                        buf.push_str(t);
                    }
                }
                "tab" => buf.push_str("    "),
                "br" => buf.push(' '),
                _ => {}
            }
        }
        if !buf.is_empty() {
            runs.push(Run::new(buf, r_style));
        }
    }

    if !images.is_empty() && runs.is_empty() {
        return images
            .into_iter()
            .map(|img| Block::with_kind(BlockKind::Image(img), Vec::new()))
            .collect();
    }

    if is_divider && runs.is_empty() {
        return vec![Block::with_kind(BlockKind::Divider, Vec::new())];
    }

    if is_caption && !images.is_empty() {
        // Rare case if image and caption shared paragraph; keep both cleanly.
    }

    let mut result = Vec::new();
    if !images.is_empty() {
        for img in images {
            result.push(Block::with_kind(BlockKind::Image(img), Vec::new()));
        }
    }
    if !runs.is_empty() || result.is_empty() {
        normalize_runs(&mut runs, base_style);
        let mut blk = Block::with_kind(kind, runs);
        blk.align = align;
        blk.line_spacing = line_spacing;
        blk.space_after_pt = space_after_pt;
        result.push(blk);
    }
    result
}

fn parse_table_node(
    tbl_node: Node<'_, '_>,
    body_w: f32,
) -> Option<TableBlock> {
    let mut col_widths = Vec::new();
    if let Some(grid) = child_by_tag(tbl_node, "tblGrid") {
        for gc in grid.children().filter(|c| c.is_element() && c.tag_name().name() == "gridCol") {
            let tw = attr_w(gc, "w")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(2000.0);
            col_widths.push((tw / 15.0).max(36.0));
        }
    }

    let mut rows = Vec::new();
    for tr in tbl_node.children().filter(|c| c.is_element() && c.tag_name().name() == "tr") {
        let mut row = Vec::new();
        for tc in tr.children().filter(|c| c.is_element() && c.tag_name().name() == "tc") {
            let mut shading = None;
            if let Some(tcpr) = child_by_tag(tc, "tcPr") {
                if let Some(shd) = child_by_tag(tcpr, "shd") {
                    shading = attr_w(shd, "fill").and_then(parse_hex6);
                }
            }
            let mut cell_runs = Vec::new();
            let mut cell_align = Align::Left;
            let mut first_p = true;
            for p in tc.children().filter(|c| c.is_element() && c.tag_name().name() == "p") {
                if !first_p {
                    cell_runs.push(Run::new(" ", TextStyle::default()));
                }
                first_p = false;
                if let Some(ppr) = child_by_tag(p, "pPr") {
                    if let Some(jc) = child_by_tag(ppr, "jc") {
                        cell_align = match attr_w(jc, "val").unwrap_or("left") {
                            "center" => Align::Center,
                            "right" | "end" => Align::Right,
                            "both" => Align::Justify,
                            _ => Align::Left,
                        };
                    }
                }
                for r in p.children().filter(|c| c.is_element() && c.tag_name().name() == "r") {
                    let st = parse_rpr(child_by_tag(r, "rPr"), TextStyle { size_pt: 10.5, ..TextStyle::default() });
                    let mut buf = String::new();
                    collect_all_text(r, &mut buf);
                    if !buf.is_empty() {
                        cell_runs.push(Run::new(buf, st));
                    }
                }
            }
            normalize_runs(
                &mut cell_runs,
                TextStyle {
                    size_pt: 10.5,
                    ..TextStyle::default()
                },
            );
            row.push(TableCell {
                runs: cell_runs,
                align: cell_align,
                shading,
            });
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }

    if rows.is_empty() {
        return None;
    }
    let max_cols = rows.iter().map(Vec::len).max().unwrap_or(1);
    for r in &mut rows {
        while r.len() < max_cols {
            r.push(TableCell::with_text("", TextStyle::default(), Align::Left, None));
        }
    }
    if col_widths.len() != max_cols {
        col_widths = vec![(body_w / max_cols as f32).max(40.0); max_cols];
    }
    let header_row = rows.len() > 1;
    Some(TableBlock {
        rows,
        col_widths,
        header_row,
    })
}

pub fn decode_docx_bytes(bytes: &[u8]) -> Result<Document, String> {
    let zip = ZipArchive::from_bytes(bytes)?;
    let doc_xml_str = zip
        .get_str("word/document.xml")
        .ok_or_else(|| "DOCX 缺少 word/document.xml".to_string())?;
    let xml = XmlDoc::parse(&doc_xml_str).map_err(|e| format!("解析 document.xml 失败: {e}"))?;
    let rels = parse_rels(&zip, "word/_rels/document.xml.rels");
    let num_map = parse_numbering(&zip);

    let mut setup = PageSetup::default();
    let body_node = xml
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "body")
        .ok_or_else(|| "DOCX 缺少 w:body".to_string())?;

    if let Some(sect) = child_by_tag(body_node, "sectPr") {
        if let Some(pgsz) = child_by_tag(sect, "pgSz") {
            let w_tw = attr_w(pgsz, "w").and_then(|s| s.parse::<u32>().ok()).unwrap_or(11906);
            let h_tw = attr_w(pgsz, "h").and_then(|s| s.parse::<u32>().ok()).unwrap_or(16838);
            let orient = attr_w(pgsz, "orient").unwrap_or("portrait");
            setup.orientation = if orient.eq_ignore_ascii_case("landscape") || w_tw > h_tw {
                Orientation::Landscape
            } else {
                Orientation::Portrait
            };
            let max_dim = w_tw.max(h_tw);
            setup.size = if max_dim < 16300 {
                PageSize::Letter
            } else {
                PageSize::A4
            };
        }
        if let Some(pgmar) = child_by_tag(sect, "pgMar") {
            let tw_to_mm = |tw: &str, def: f32| -> f32 {
                tw.parse::<f32>()
                    .map(|v| (v / 56.6929).clamp(6.0, 60.0))
                    .unwrap_or(def)
            };
            setup.margins = PageMargins {
                top_mm: attr_w(pgmar, "top").map_or(22.0, |v| tw_to_mm(v, 22.0)),
                bottom_mm: attr_w(pgmar, "bottom").map_or(22.0, |v| tw_to_mm(v, 22.0)),
                left_mm: attr_w(pgmar, "left").map_or(24.0, |v| tw_to_mm(v, 24.0)),
                right_mm: attr_w(pgmar, "right").map_or(24.0, |v| tw_to_mm(v, 24.0)),
            };
            setup.margin_preset = if setup.margins.left_mm < 16.0 {
                MarginPreset::Narrow
            } else if setup.margins.left_mm > 32.0 {
                MarginPreset::Wide
            } else {
                MarginPreset::Normal
            };
        }
        if let Some(href) = child_by_tag(sect, "headerReference") {
            if let Some(rid) = attr_w(href, "id") {
                if let Some((_, target)) = rels.get(rid) {
                    if let Some(hxml) = zip.get_str(&format!("word/{}", target.trim_start_matches('/'))) {
                        if let Ok(parsed) = XmlDoc::parse(&hxml) {
                            let mut txt = String::new();
                            collect_all_text(parsed.root_element(), &mut txt);
                            setup.header = txt.trim().to_string();
                        }
                    }
                }
            }
        }
        if let Some(fref) = child_by_tag(sect, "footerReference") {
            if let Some(rid) = attr_w(fref, "id") {
                if let Some((_, target)) = rels.get(rid) {
                    if let Some(fxml) = zip.get_str(&format!("word/{}", target.trim_start_matches('/'))) {
                        if let Ok(parsed) = XmlDoc::parse(&fxml) {
                            let mut txt = String::new();
                            collect_all_text(parsed.root_element(), &mut txt);
                            setup.footer = txt.trim().to_string();
                        }
                    }
                }
            }
        }
    }

    let body_w_px = setup.body_width_px();
    let _ = PX_PER_MM;
    let mut blocks: Vec<Block> = Vec::new();
    let mut next_img_id = 1u64;

    for node in body_node.children().filter(|c| c.is_element()) {
        match node.tag_name().name() {
            "p" => {
                let mut parsed = parse_paragraph_node(
                    node,
                    &num_map,
                    &rels,
                    &zip,
                    &mut next_img_id,
                    body_w_px,
                );
                // If previous block was an Image without caption and this paragraph is Caption style, attach as caption!
                let is_caption_p = child_by_tag(node, "pPr")
                    .and_then(|ppr| child_by_tag(ppr, "pStyle"))
                    .and_then(|s| attr_w(s, "val"))
                    .is_some_and(|s| s.eq_ignore_ascii_case("caption"));
                if is_caption_p && parsed.len() == 1 {
                    if let Some(prev) = blocks.last_mut() {
                        if let BlockKind::Image(ref mut img) = prev.kind {
                            let txt = crate::doc::runs_plain_text(&parsed[0].runs);
                            if !txt.trim().is_empty() {
                                img.caption = txt.trim().to_string();
                                continue;
                            }
                        }
                    }
                }
                blocks.append(&mut parsed);
            }
            "tbl" => {
                if let Some(tbl) = parse_table_node(node, body_w_px as f32) {
                    blocks.push(Block::with_kind(BlockKind::Table(tbl), Vec::new()));
                }
            }
            _ => {}
        }
    }

    Ok(Document::from_parts(setup, blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::runs_plain_text;

    #[test]
    fn docx_roundtrips_rich_demo_document_preserving_styles_tables_and_images() {
        let orig = Document::demo_document();
        let bytes = encode_docx_bytes(&orig).expect("encode demo docx");
        assert!(bytes.starts_with(b"PK\x03\x04"));

        let decoded = decode_docx_bytes(&bytes).expect("decode demo docx");
        assert_eq!(decoded.setup.header, orig.setup.header);
        assert_eq!(decoded.setup.footer, orig.setup.footer);
        assert_eq!(decoded.blocks.len(), orig.blocks.len());

        // Heading 1 preserved
        assert_eq!(decoded.blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(
            runs_plain_text(&decoded.blocks[0].runs),
            runs_plain_text(&orig.blocks[0].runs)
        );

        // Mixed font families & inline colors preserved in paragraph 1
        assert!(decoded.blocks[1].runs.iter().any(|r| r.style.bold && r.style.color == 0x1d4ed8));
        assert!(decoded.blocks[1].runs.iter().any(|r| r.style.font == FontFamily::Serif));
        assert!(decoded.blocks[1].runs.iter().any(|r| r.style.font == FontFamily::Mono && r.style.highlight.is_some()));

        // Table rows preserved
        let tbl = decoded
            .blocks
            .iter()
            .find_map(|b| match &b.kind {
                BlockKind::Table(t) => Some(t),
                _ => None,
            })
            .expect("table preserved");
        assert_eq!(tbl.rows.len(), 4);
        assert_eq!(tbl.col_count(), 4);
        assert_eq!(runs_plain_text(&tbl.rows[0][0].runs), "子系统模块");

        // Embedded PNG chart preserved with valid dimensions
        let img = decoded
            .blocks
            .iter()
            .find_map(|b| match &b.kind {
                BlockKind::Image(i) => Some(i),
                _ => None,
            })
            .expect("image preserved");
        assert_eq!((img.intrinsic_w, img.intrinsic_h), (440, 180));
        assert!(img.caption.contains("SpaceOS-9"));

        // Bullet & numbered lists preserved
        assert!(decoded.blocks.iter().any(|b| b.kind == BlockKind::BulletList));
        assert!(decoded.blocks.iter().any(|b| b.kind == BlockKind::NumberedList));
        assert!(decoded.blocks.iter().any(|b| b.kind == BlockKind::Divider));
    }
}
