//
//       user/editor/src/io_text.rs
//       Bidirectional Markdown (.md), HTML (.html) & Plain Text (.txt) import/export
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crate::doc::{
    normalize_runs, runs_plain_text, Align, Block, BlockKind, Document, FontFamily, PageSetup, Run,
    TableBlock, TableCell, TextStyle,
};
use crate::io_docx::{load_docx, save_docx};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocFormat {
    Docx,
    Markdown,
    Html,
    Text,
}

impl DocFormat {
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("docx") => Self::Docx,
            Some("md" | "markdown") => Self::Markdown,
            Some("html" | "htm") => Self::Html,
            _ => Self::Text,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Text => "txt",
        }
    }
}

pub fn load_document(path: &Path) -> Result<Document, String> {
    match DocFormat::from_path(path) {
        DocFormat::Docx => load_docx(path),
        DocFormat::Markdown => {
            let raw = fs::read_to_string(path).map_err(|e| format!("读取 Markdown 失败: {e}"))?;
            let mut doc = parse_markdown(&raw);
            doc.path = Some(path.to_path_buf());
            doc.dirty = false;
            Ok(doc)
        }
        DocFormat::Html => {
            let raw = fs::read_to_string(path).map_err(|e| format!("读取 HTML 失败: {e}"))?;
            let mut doc = parse_html(&raw);
            doc.path = Some(path.to_path_buf());
            doc.dirty = false;
            Ok(doc)
        }
        DocFormat::Text => {
            let raw = fs::read_to_string(path).map_err(|e| format!("读取文本失败: {e}"))?;
            let mut doc = parse_plain_text(&raw);
            doc.path = Some(path.to_path_buf());
            doc.dirty = false;
            Ok(doc)
        }
    }
}

pub fn save_document(doc: &Document, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    match DocFormat::from_path(path) {
        DocFormat::Docx => save_docx(doc, path),
        DocFormat::Markdown => {
            fs::write(path, export_markdown(doc)).map_err(|e| format!("保存 Markdown 失败: {e}"))
        }
        DocFormat::Html => {
            fs::write(path, export_html(doc)).map_err(|e| format!("保存 HTML 失败: {e}"))
        }
        DocFormat::Text => {
            fs::write(path, export_plain_text(doc)).map_err(|e| format!("保存文本失败: {e}"))
        }
    }
}

pub fn parse_plain_text(raw: &str) -> Document {
    let mut blocks = Vec::new();
    for line in raw.replace("\r\n", "\n").lines() {
        blocks.push(Block::paragraph(vec![Run::new(line, TextStyle::default())]));
    }
    Document::from_parts(PageSetup::default(), blocks)
}

pub fn export_plain_text(doc: &Document) -> String {
    let mut out = String::new();
    for blk in &doc.blocks {
        match &blk.kind {
            BlockKind::Divider => out.push_str("---\n"),
            BlockKind::Image(img) => {
                if img.caption.is_empty() {
                    let _ = writeln!(out, "[图片: {}]", img.filename);
                } else {
                    let _ = writeln!(out, "[图片: {}]", img.caption);
                }
            }
            BlockKind::Table(tbl) => {
                for row in &tbl.rows {
                    let line: Vec<String> = row.iter().map(|c| runs_plain_text(&c.runs)).collect();
                    out.push_str(&line.join("\t"));
                    out.push('\n');
                }
            }
            _ => {
                out.push_str(&runs_plain_text(&blk.runs));
                out.push('\n');
            }
        }
    }
    out
}

pub fn export_markdown(doc: &Document) -> String {
    let mut out = String::new();
    let mut num_idx = 0usize;
    for blk in &doc.blocks {
        if !matches!(blk.kind, BlockKind::NumberedList) {
            num_idx = 0;
        }
        match &blk.kind {
            BlockKind::Heading(l) => {
                let hashes = "#".repeat((*l as usize).clamp(1, 3));
                let _ = writeln!(out, "{hashes} {}\n", runs_plain_text(&blk.runs));
            }
            BlockKind::BulletList => {
                let _ = writeln!(out, "- {}", runs_to_markdown(&blk.runs));
            }
            BlockKind::NumberedList => {
                num_idx += 1;
                let _ = writeln!(out, "{num_idx}. {}", runs_to_markdown(&blk.runs));
            }
            BlockKind::Quote => {
                let _ = writeln!(out, "> {}\n", runs_to_markdown(&blk.runs));
            }
            BlockKind::Divider => {
                out.push_str("---\n\n");
            }
            BlockKind::Table(tbl) => {
                for (r_idx, row) in tbl.rows.iter().enumerate() {
                    out.push('|');
                    for cell in row {
                        let txt = runs_to_markdown(&cell.runs).replace('|', "\\|");
                        let _ = write!(out, " {txt} |");
                    }
                    out.push('\n');
                    if r_idx == 0 {
                        out.push('|');
                        for _ in row {
                            out.push_str(" --- |");
                        }
                        out.push('\n');
                    }
                }
                out.push('\n');
            }
            BlockKind::Image(img) => {
                let alt = if img.caption.is_empty() {
                    img.filename.as_str()
                } else {
                    img.caption.as_str()
                };
                let _ = writeln!(out, "![{alt}]({})\n", img.filename);
            }
            BlockKind::Paragraph => {
                let s = runs_to_markdown(&blk.runs);
                let _ = writeln!(out, "{s}\n");
            }
        }
    }
    out
}

fn runs_to_markdown(runs: &[Run]) -> String {
    let mut out = String::new();
    for r in runs {
        if r.text.is_empty() {
            continue;
        }
        if r.style.font == FontFamily::Mono {
            let _ = write!(out, "`{}`", r.text);
        } else if r.style.bold && r.style.italic {
            let _ = write!(out, "***{}***", r.text);
        } else if r.style.bold {
            let _ = write!(out, "**{}**", r.text);
        } else if r.style.italic {
            let _ = write!(out, "*{}*", r.text);
        } else if r.style.strikethrough {
            let _ = write!(out, "~~{}~~", r.text);
        } else {
            out.push_str(&r.text);
        }
    }
    out
}

fn parse_inline_md(s: &str, base: TextStyle) -> Vec<Run> {
    let mut runs = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut cur = String::new();
    let flush = |cur: &mut String, runs: &mut Vec<Run>| {
        if !cur.is_empty() {
            runs.push(Run::new(std::mem::take(cur), base));
        }
    };
    while i < n {
        if i + 1 < n && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_pair(&chars, i + 2, '*', '*') {
                flush(&mut cur, &mut runs);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut st = base;
                st.bold = true;
                runs.push(Run::new(inner, st));
                i = end + 2;
                continue;
            }
        }
        if chars[i] == '*' && (i + 1 >= n || chars[i + 1] != '*') {
            if let Some(end) = (i + 1..n).find(|&k| chars[k] == '*') {
                flush(&mut cur, &mut runs);
                let inner: String = chars[i + 1..end].iter().collect();
                let mut st = base;
                st.italic = true;
                runs.push(Run::new(inner, st));
                i = end + 1;
                continue;
            }
        }
        if chars[i] == '`' {
            if let Some(end) = (i + 1..n).find(|&k| chars[k] == '`') {
                flush(&mut cur, &mut runs);
                let inner: String = chars[i + 1..end].iter().collect();
                let mut st = base;
                st.font = FontFamily::Mono;
                runs.push(Run::new(inner, st));
                i = end + 1;
                continue;
            }
        }
        if i + 1 < n && chars[i] == '~' && chars[i + 1] == '~' {
            if let Some(end) = find_pair(&chars, i + 2, '~', '~') {
                flush(&mut cur, &mut runs);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut st = base;
                st.strikethrough = true;
                runs.push(Run::new(inner, st));
                i = end + 2;
                continue;
            }
        }
        cur.push(chars[i]);
        i += 1;
    }
    flush(&mut cur, &mut runs);
    normalize_runs(&mut runs, base);
    runs
}

fn find_pair(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == a && chars[i + 1] == b {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn parse_markdown(raw: &str) -> Document {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = raw.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            blocks.push(Block::with_kind(BlockKind::Divider, Vec::new()));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            blocks.push(Block::heading(3, rest.trim()));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            blocks.push(Block::heading(2, rest.trim()));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            blocks.push(Block::heading(1, rest.trim()));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            let st = BlockKind::Quote.default_style();
            blocks.push(Block::with_kind(BlockKind::Quote, parse_inline_md(rest, st)));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            blocks.push(Block::with_kind(
                BlockKind::BulletList,
                parse_inline_md(rest, TextStyle::default()),
            ));
            i += 1;
            continue;
        }
        if let Some(dot) = trimmed.find(". ") {
            if dot > 0 && trimmed[..dot].chars().all(|c| c.is_ascii_digit()) {
                blocks.push(Block::with_kind(
                    BlockKind::NumberedList,
                    parse_inline_md(trimmed[dot + 2..].trim_start(), TextStyle::default()),
                ));
                i += 1;
                continue;
            }
        }
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            let mut rows_raw = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                let row_line = lines[i].trim();
                let cells: Vec<String> = row_line
                    .trim_matches('|')
                    .split('|')
                    .map(|c| c.trim().to_string())
                    .collect();
                let is_sep = cells
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
                if !is_sep {
                    rows_raw.push(cells);
                }
                i += 1;
            }
            if !rows_raw.is_empty() {
                let cols = rows_raw.iter().map(Vec::len).max().unwrap_or(1).max(1);
                let body_w = PageSetup::default().body_width_px() as f32;
                let mut rows = Vec::new();
                for (r_idx, r) in rows_raw.into_iter().enumerate() {
                    let mut row = Vec::with_capacity(cols);
                    for c_idx in 0..cols {
                        let txt = r.get(c_idx).map(String::as_str).unwrap_or("");
                        let st = TextStyle {
                            bold: r_idx == 0,
                            size_pt: 10.5,
                            ..TextStyle::default()
                        };
                        row.push(TableCell {
                            runs: parse_inline_md(txt, st),
                            align: Align::Left,
                            shading: if r_idx == 0 { Some(0xeff6ff) } else { None },
                        });
                    }
                    rows.push(row);
                }
                let tbl = TableBlock {
                    rows,
                    col_widths: vec![body_w / cols as f32; cols],
                    header_row: true,
                };
                blocks.push(Block::with_kind(BlockKind::Table(tbl), Vec::new()));
                continue;
            }
        }
        blocks.push(Block::paragraph(parse_inline_md(trimmed, TextStyle::default())));
        i += 1;
    }
    Document::from_parts(PageSetup::default(), blocks)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn export_html(doc: &Document) -> String {
    let mut out = String::from(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<title>SpaceEditor 文档</title>\n\
        <style>body{font-family:sans-serif;max-width:794px;margin:32px auto;padding:0 24px;color:#1e293b;line-height:1.5}\
        table{border-collapse:collapse;width:100%;margin:12px 0}th,td{border:1px solid #cbd5e1;padding:6px 10px;text-align:left}\
        blockquote{border-left:3px solid #3b82f6;padding-left:12px;color:#475569;font-style:italic}</style>\n</head>\n<body>\n",
    );
    let write_runs = |buf: &mut String, runs: &[Run], def: TextStyle| {
        for r in runs {
            let mut txt = html_escape(&r.text);
            if r.style.font == FontFamily::Mono {
                txt = format!("<code>{txt}</code>");
            }
            if r.style.bold && !def.bold {
                txt = format!("<strong>{txt}</strong>");
            }
            if r.style.italic && !def.italic {
                txt = format!("<em>{txt}</em>");
            }
            if r.style.underline && !def.underline {
                txt = format!("<u>{txt}</u>");
            }
            if r.style.strikethrough && !def.strikethrough {
                txt = format!("<s>{txt}</s>");
            }
            if r.style.color != def.color || r.style.highlight.is_some() {
                let mut style = format!("color:#{:06x};", r.style.color & 0xff_ffff);
                if let Some(bg) = r.style.highlight {
                    let _ = write!(style, "background-color:#{:06x};", bg & 0xff_ffff);
                }
                txt = format!("<span style=\"{style}\">{txt}</span>");
            }
            buf.push_str(&txt);
        }
    };

    for blk in &doc.blocks {
        let def_st = blk.kind.default_style();
        match &blk.kind {
            BlockKind::Heading(1) => {
                out.push_str("<h1>");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</h1>\n");
            }
            BlockKind::Heading(2) => {
                out.push_str("<h2>");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</h2>\n");
            }
            BlockKind::Heading(_) => {
                out.push_str("<h3>");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</h3>\n");
            }
            BlockKind::BulletList | BlockKind::NumberedList => {
                out.push_str("<p>• ");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</p>\n");
            }
            BlockKind::Quote => {
                out.push_str("<blockquote>");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</blockquote>\n");
            }
            BlockKind::Divider => out.push_str("<hr>\n"),
            BlockKind::Table(tbl) => {
                out.push_str("<table>\n");
                for (r_idx, row) in tbl.rows.iter().enumerate() {
                    out.push_str("<tr>");
                    let tag = if r_idx == 0 { "th" } else { "td" };
                    let cell_def = TextStyle {
                        bold: r_idx == 0,
                        ..TextStyle::default()
                    };
                    for cell in row {
                        let _ = write!(out, "<{tag}>");
                        write_runs(&mut out, &cell.runs, cell_def);
                        let _ = write!(out, "</{tag}>");
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</table>\n");
            }
            BlockKind::Image(img) => {
                let _ = writeln!(out, "<p>[图片: {}]</p>", html_escape(&img.caption));
            }
            BlockKind::Paragraph => {
                out.push_str("<p>");
                write_runs(&mut out, &blk.runs, def_st);
                out.push_str("</p>\n");
            }
        }
    }
    out.push_str("</body>\n</html>\n");
    out
}

pub fn parse_html(raw: &str) -> Document {
    // If <body> exists, start parsing from <body> so <head>/<title> do not leak into body.
    let lower_raw = raw.to_ascii_lowercase();
    let sliced = if let Some(bpos) = lower_raw.find("<body") {
        raw[bpos..].split_once('>').map_or(raw, |(_, rest)| rest)
    } else {
        raw
    };
    let mut normalized = sliced.to_string();
    for (tag, marker) in [
        ("<h1>", "\n# "),
        ("<h2>", "\n## "),
        ("<h3>", "\n### "),
        ("<li>", "\n- "),
        ("<blockquote>", "\n> "),
        ("<hr>", "\n---\n"),
        ("<p>", "\n"),
        ("<br>", "\n"),
        ("</div>", "\n"),
        ("</p>", "\n"),
        ("</h1>", "\n"),
        ("</h2>", "\n"),
        ("</h3>", "\n"),
        ("</li>", "\n"),
        ("</blockquote>", "\n"),
    ] {
        normalized = normalized.replace(tag, marker);
        normalized = normalized.replace(&tag.to_ascii_uppercase(), marker);
    }
    let mut plain = String::with_capacity(normalized.len());
    let mut in_tag = false;
    let mut in_style_or_script = false;
    let lower = normalized.to_ascii_lowercase();
    let bytes = normalized.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if lower[i..].starts_with("<style") || lower[i..].starts_with("<script") {
            in_style_or_script = true;
        } else if lower[i..].starts_with("</style>") {
            in_style_or_script = false;
            i += 8;
            continue;
        } else if lower[i..].starts_with("</script>") {
            in_style_or_script = false;
            i += 9;
            continue;
        }
        let ch = normalized[i..].chars().next().unwrap();
        let clen = ch.len_utf8();
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag && !in_style_or_script {
            plain.push(ch);
        }
        i += clen;
    }
    let decoded = plain
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ");
    parse_markdown(&decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_and_html_roundtrip_headings_and_tables() {
        let md = "# 文档标题\n\n这是 **加粗** 与 *斜体* 以及 `code`。\n\n- 第一项\n- 第二项\n\n| 列A | 列B |\n| --- | --- |\n| 值1 | 值2 |\n";
        let doc = parse_markdown(md);
        assert_eq!(doc.blocks[0].kind, BlockKind::Heading(1));
        assert!(doc.blocks[1].runs.iter().any(|r| r.style.bold));
        assert!(doc.blocks[1].runs.iter().any(|r| r.style.italic));
        assert!(doc.blocks[1].runs.iter().any(|r| r.style.font == FontFamily::Mono));
        assert!(doc.blocks.iter().any(|b| matches!(b.kind, BlockKind::Table(_))));

        let html = export_html(&doc);
        assert!(html.contains("<h1>文档标题</h1>"));
        assert!(html.contains("<table>"));
        let reparsed = parse_html(&html);
        assert_eq!(reparsed.blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(runs_plain_text(&reparsed.blocks[0].runs), "文档标题");
    }
}
