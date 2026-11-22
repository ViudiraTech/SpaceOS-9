//
//       user/editor/src/zip_mini.rs
//       Pure-Rust minimal ZIP archive reader/writer (Stored & Deflate) for OOXML .docx
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crc32fast::hash as crc32;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::collections::BTreeMap;
use std::io::{Read, Write};

const SIG_LFH: u32 = 0x0403_4b50;
const SIG_CDH: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;

fn le16(buf: &[u8], off: usize) -> Option<u16> {
    buf.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn le32(buf: &[u8], off: usize) -> Option<u32> {
    buf.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Decoded in-memory ZIP archive keyed by normalized forward-slash path.
pub struct ZipArchive {
    entries: BTreeMap<String, Vec<u8>>,
}

impl ZipArchive {
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 22 {
            return Err("无效的 ZIP/DOCX 文件：长度过短".into());
        }
        // Scan backwards for End of Central Directory (up to 64 KiB comment).
        let min_off = data.len().saturating_sub(22 + 65535);
        let mut eocd_off = None;
        let mut i = data.len() - 22;
        loop {
            if le32(data, i) == Some(SIG_EOCD) {
                let comment_len = le16(data, i + 20).unwrap_or(0) as usize;
                if i + 22 + comment_len == data.len() {
                    eocd_off = Some(i);
                    break;
                }
            }
            if i == min_off {
                break;
            }
            i -= 1;
        }
        let eocd = eocd_off.ok_or_else(|| "未找到 ZIP 中央目录结束标记".to_string())?;
        let total_entries = le16(data, eocd + 10)
            .ok_or_else(|| "损坏的 ZIP 条目计数".to_string())? as usize;
        let cd_size = le32(data, eocd + 12)
            .ok_or_else(|| "损坏的 ZIP 目录长度".to_string())? as usize;
        let cd_offset = le32(data, eocd + 16)
            .ok_or_else(|| "损坏的 ZIP 目录偏移".to_string())? as usize;

        if cd_offset.saturating_add(cd_size) > eocd {
            return Err("ZIP 中央目录超出文件边界".into());
        }

        let mut entries = BTreeMap::new();
        let mut pos = cd_offset;
        for _ in 0..total_entries {
            if pos + 46 > data.len() || le32(data, pos) != Some(SIG_CDH) {
                return Err("损坏的 ZIP 中央目录头".into());
            }
            let method = le16(data, pos + 10).ok_or_else(|| "ZIP 方法字段越界".to_string())?;
            let expected_crc = le32(data, pos + 16).ok_or_else(|| "ZIP CRC 越界".to_string())?;
            let comp_size = le32(data, pos + 20)
                .ok_or_else(|| "ZIP 压缩大小越界".to_string())? as usize;
            let uncomp_size = le32(data, pos + 24)
                .ok_or_else(|| "ZIP 未压缩大小越界".to_string())? as usize;
            let name_len = le16(data, pos + 28).ok_or_else(|| "ZIP 文件名长度越界".to_string())? as usize;
            let extra_len = le16(data, pos + 30).ok_or_else(|| "ZIP 扩展长度越界".to_string())? as usize;
            let comment_len = le16(data, pos + 32).ok_or_else(|| "ZIP 注释长度越界".to_string())? as usize;
            let lfh_offset = le32(data, pos + 42)
                .ok_or_else(|| "ZIP 本地偏移越界".to_string())? as usize;

            let name_start = pos + 46;
            let name_end = name_start + name_len;
            let name_bytes = data
                .get(name_start..name_end)
                .ok_or_else(|| "ZIP 文件名越界".to_string())?;
            let raw_name = String::from_utf8_lossy(name_bytes).replace('\\', "/");
            let name = raw_name.trim_start_matches('/').to_string();
            pos = name_end + extra_len + comment_len;

            if name.ends_with('/') {
                continue;
            }
            if lfh_offset + 30 > data.len() || le32(data, lfh_offset) != Some(SIG_LFH) {
                return Err(format!("ZIP 条目 {name} 本地头损坏"));
            }
            let lfh_name_len = le16(data, lfh_offset + 26).unwrap_or(0) as usize;
            let lfh_extra_len = le16(data, lfh_offset + 28).unwrap_or(0) as usize;
            let payload_start = lfh_offset + 30 + lfh_name_len + lfh_extra_len;
            let payload_end = payload_start
                .checked_add(comp_size)
                .ok_or_else(|| format!("ZIP 条目 {name} 大小溢出"))?;
            let payload = data
                .get(payload_start..payload_end)
                .ok_or_else(|| format!("ZIP 条目 {name} 数据越界"))?;

            let decoded = match method {
                0 => payload.to_vec(),
                8 => {
                    let mut out = Vec::with_capacity(uncomp_size.min(16 * 1024 * 1024));
                    let mut decoder = DeflateDecoder::new(payload);
                    decoder
                        .read_to_end(&mut out)
                        .map_err(|e| format!("ZIP 解压 {name} 失败: {e}"))?;
                    out
                }
                other => return Err(format!("不支持的 ZIP 压缩方法 {other}（{name}）")),
            };

            if crc32(&decoded) != expected_crc {
                return Err(format!("ZIP 条目 {name} CRC32 校验失败"));
            }
            entries.insert(name, decoded);
        }

        Ok(Self { entries })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let norm = name.trim_start_matches('/');
        self.entries.get(norm).map(Vec::as_slice)
    }

    pub fn get_str(&self, name: &str) -> Option<String> {
        self.get(name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}

struct WrittenEntry {
    name: String,
    method: u16,
    crc: u32,
    comp_data: Vec<u8>,
    uncomp_len: u32,
}

/// Deterministic pure-Rust ZIP writer for `.docx` export.
#[derive(Default)]
pub struct ZipWriter {
    entries: Vec<WrittenEntry>,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add_stored(&mut self, name: &str, data: &[u8]) {
        self.entries.push(WrittenEntry {
            name: name.trim_start_matches('/').to_string(),
            method: 0,
            crc: crc32(data),
            comp_data: data.to_vec(),
            uncomp_len: data.len() as u32,
        });
    }

    pub fn add_deflated(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(data)
            .map_err(|e| format!("压缩 {name} 失败: {e}"))?;
        let comp_data = encoder
            .finish()
            .map_err(|e| format!("完成压缩 {name} 失败: {e}"))?;
        if comp_data.len() >= data.len() {
            self.add_stored(name, data);
            return Ok(());
        }
        self.entries.push(WrittenEntry {
            name: name.trim_start_matches('/').to_string(),
            method: 8,
            crc: crc32(data),
            comp_data,
            uncomp_len: data.len() as u32,
        });
        Ok(())
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        let count = self.entries.len() as u16;

        for entry in &self.entries {
            let lfh_off = out.len() as u32;
            let name_bytes = entry.name.as_bytes();
            let name_len = name_bytes.len() as u16;
            let comp_len = entry.comp_data.len() as u32;
            let flags: u16 = 0x0800; // UTF-8 filenames

            // Local file header
            out.extend_from_slice(&SIG_LFH.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&entry.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0x5421u16.to_le_bytes()); // mod date (2022-01-01)
            out.extend_from_slice(&entry.crc.to_le_bytes());
            out.extend_from_slice(&comp_len.to_le_bytes());
            out.extend_from_slice(&entry.uncomp_len.to_le_bytes());
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(&entry.comp_data);

            // Central directory header
            cd.extend_from_slice(&SIG_CDH.to_le_bytes());
            cd.extend_from_slice(&0x031eu16.to_le_bytes()); // made by unix
            cd.extend_from_slice(&20u16.to_le_bytes()); // needed
            cd.extend_from_slice(&flags.to_le_bytes());
            cd.extend_from_slice(&entry.method.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0x5421u16.to_le_bytes());
            cd.extend_from_slice(&entry.crc.to_le_bytes());
            cd.extend_from_slice(&comp_len.to_le_bytes());
            cd.extend_from_slice(&entry.uncomp_len.to_le_bytes());
            cd.extend_from_slice(&name_len.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra len
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk number
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            cd.extend_from_slice(&lfh_off.to_le_bytes());
            cd.extend_from_slice(name_bytes);
        }

        let cd_offset = out.len() as u32;
        let cd_size = cd.len() as u32;
        out.extend_from_slice(&cd);

        // End of central directory
        out.extend_from_slice(&SIG_EOCD.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_stored_and_deflated_entries() {
        let mut w = ZipWriter::new();
        let big = "SpaceOS 文档排版引擎 ".repeat(40);
        w.add_deflated("word/document.xml", big.as_bytes()).unwrap();
        w.add_stored("word/media/tiny.png", b"\x89PNG\r\n\x1a\ntest");
        let zip_bytes = w.finish();

        let arch = ZipArchive::from_bytes(&zip_bytes).expect("parse zip");
        assert_eq!(arch.get_str("word/document.xml").as_deref(), Some(big.as_str()));
        assert_eq!(
            arch.get("word/media/tiny.png"),
            Some(b"\x89PNG\r\n\x1a\ntest".as_slice())
        );
    }

    #[test]
    fn reads_data_descriptor_entries() {
        // Construct a single Stored entry with bit 3 (data descriptor) set and zeroed LFH sizes.
        let name = b"hello.txt";
        let payload = b"OOXML data descriptor payload";
        let crc = crc32(payload);
        let len = payload.len() as u32;

        let mut out = Vec::new();
        out.extend_from_slice(&SIG_LFH.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0008u16.to_le_bytes()); // bit 3 set
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // crc in DD
        out.extend_from_slice(&0u32.to_le_bytes()); // comp len in DD
        out.extend_from_slice(&0u32.to_le_bytes()); // uncomp len in DD
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(payload);
        // Optional data descriptor (16 bytes)
        out.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());

        let cd_off = out.len() as u32;
        out.extend_from_slice(&SIG_CDH.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0008u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(name);
        let cd_size = out.len() as u32 - cd_off;

        out.extend_from_slice(&SIG_EOCD.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());

        let arch = ZipArchive::from_bytes(&out).expect("parse data descriptor zip");
        assert_eq!(arch.get("hello.txt"), Some(payload.as_slice()));
    }
}
