#!/usr/bin/env python3
#
#       tools/build_spime_dict.py
#       Offline compiler for SPIME Chinese IME binary dictionary (`spime.dict`)
#       and bigram language model (`spime.lm`).
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
"""Compile `/usr/share/spime/spime.dict` and `spime.lm` from CC-BY-4.0 / MIT lexicons."""

from __future__ import annotations

import argparse
import collections
import math
import pathlib
import struct
import sys
import unicodedata
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_CACHE = ROOT / "build" / "downloads" / "spime"
DEFAULT_OUT = ROOT / "build" / "spime"

SOURCES = {
    "zi.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/zi.dict.yaml",
    "jichu.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/jichu.dict.yaml",
    "duoyin.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/duoyin.dict.yaml",
    "cuoyin.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/cuoyin.dict.yaml",
    "diming.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/diming.dict.yaml",
    "en.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/en.dict.yaml",
    "mixed.dict.yaml": "https://raw.githubusercontent.com/amzxyz/rime_wanxiang/wanxiang/dicts/mixed.dict.yaml",
    "pinyin.txt": "https://raw.githubusercontent.com/mozillazg/pinyin-data/master/pinyin.txt",
}

# Canonical sorted list of Mandarin syllables used by SPIME (`v` denotes ü after l/n).
MANDARIN_SYLLABLES: tuple[str, ...] = tuple(sorted({
    "a", "ai", "an", "ang", "ao",
    "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi", "bian", "biao", "bie", "bin", "bing", "bo", "bu",
    "ca", "cai", "can", "cang", "cao", "ce", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou", "cu", "cuan", "cui", "cun", "cuo",
    "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo",
    "e", "ei", "en", "eng", "er",
    "fa", "fan", "fang", "fei", "fen", "feng", "fiao", "fo", "fou", "fu",
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo",
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo",
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun",
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu",
    "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve",
    "o", "ou",
    "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu",
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun",
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "rua", "ruan", "rui", "run", "ruo",
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo",
    "ta", "tai", "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting", "tong", "tou", "tu", "tuan", "tui", "tun", "tuo",
    "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu",
    "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan", "xue", "xun",
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan", "yue", "yun",
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
}))

SYL_TO_ID = {syl: idx for idx, syl in enumerate(MANDARIN_SYLLABLES)}

TONE_TABLE = str.maketrans({
    "ā": "a", "á": "a", "ǎ": "a", "à": "a",
    "ō": "o", "ó": "o", "ǒ": "o", "ò": "o",
    "ē": "e", "é": "e", "ě": "e", "è": "e", "ê": "e", "ế": "e", "ề": "e",
    "ī": "i", "í": "i", "ǐ": "i", "ì": "i",
    "ū": "u", "ú": "u", "ǔ": "u", "ù": "u",
    "ǖ": "ü", "ǘ": "ü", "ǚ": "ü", "ǜ": "ü",
    "ń": "n", "ň": "n", "ǹ": "n",
})

# Built-in fallback seed entries when running strictly offline with empty cache.
FALLBACK_ENTRIES: list[tuple[str, str, int]] = [
    ("的", "de", 900000), ("了", "le", 700000), ("是", "shi", 680000), ("我", "wo", 660000),
    ("在", "zai", 640000), ("有", "you", 620000), ("不", "bu", 600000), ("人", "ren", 580000),
    ("中", "zhong", 560000), ("国", "guo", 540000), ("他", "ta", 520000), ("她", "ta", 490000),
    ("这", "zhe", 480000), ("个", "ge", 470000), ("大", "da", 460000), ("上", "shang", 450000),
    ("们", "men", 440000), ("来", "lai", 430000), ("到", "dao", 420000), ("说", "shuo", 410000),
    ("为", "wei", 400000), ("和", "he", 390000), ("地", "de", 380000), ("出", "chu", 370000),
    ("要", "yao", 360000), ("以", "yi", 350000), ("时", "shi", 340000), ("就", "jiu", 330000),
    ("得", "de", 320000), ("也", "ye", 310000), ("会", "hui", 300000), ("你", "ni", 295000),
    ("好", "hao", 290000), ("对", "dui", 285000), ("天", "tian", 280000), ("今", "jin", 275000),
    ("北", "bei", 270000), ("京", "jing", 265000), ("系", "xi", 260000), ("统", "tong", 255000),
    ("输", "shu", 250000), ("入", "ru", 245000), ("法", "fa", 240000), ("文", "wen", 235000),
    ("本", "ben", 230000), ("编", "bian", 225000), ("辑", "ji", 220000), ("器", "qi", 215000),
    ("测", "ce", 210000), ("试", "shi", 205000), ("界", "jie", 200000), ("面", "mian", 195000),
    ("窗", "chuang", 190000), ("口", "kou", 185000), ("气", "qi", 180000), ("真", "zhen", 175000),
    ("你好", "ni hao", 650000), ("我们", "wo men", 620000), ("中国", "zhong guo", 600000),
    ("北京", "bei jing", 560000), ("今天", "jin tian", 540000), ("天气", "tian qi", 510000),
    ("可以", "ke yi", 500000), ("不是", "bu shi", 490000), ("什么", "shen me", 480000),
    ("这个", "zhe ge", 470000), ("一个", "yi ge", 460000), ("输入法", "shu ru fa", 450000),
    ("中文", "zhong wen", 440000), ("系统", "xi tong", 430000), ("文本", "wen ben", 420000),
    ("编辑器", "bian ji qi", 410000), ("测试", "ce shi", 400000), ("谢谢", "xie xie", 390000),
    ("再见", "zai jian", 380000), ("操作系统", "cao zuo xi tong", 350000),
    ("浏览器", "liu lan qi", 340000), ("终端", "zhong duan", 330000),
    ("文件", "wen jian", 320000), ("设置", "she zhi", 310000), ("真好", "zhen hao", 300000),
    ("现在", "xian zai", 290000), ("没有", "mei you", 280000), ("知道", "zhi dao", 270000),
    ("喜欢", "xi huan", 260000), ("工作", "gong zuo", 250000), ("开发", "kai fa", 240000),
    ("世界", "shi jie", 230000), ("软件", "ruan jian", 220000), ("硬件", "ying jian", 210000),
    ("垃圾", "la ji", 240000), ("电脑", "dian nao", 235000), ("鼠标", "shu biao", 230000),
    ("点击", "dian ji", 225000), ("优化", "you hua", 220000), ("性能", "xing neng", 215000),
    ("问题", "wen ti", 210000), ("怎么", "zen me", 205000), ("为什么", "wei shen me", 200000),
    ("因为", "yin wei", 195000), ("所以", "suo yi", 190000),
]

CURATED_BIGRAMS: list[tuple[str, str, int]] = [
    ("你好", "世界", 120000), ("今天", "天气", 180000), ("天气", "真好", 150000),
    ("我们", "是", 200000), ("我", "喜欢", 140000), ("中文", "输入法", 160000),
    ("文本", "编辑器", 150000), ("操作", "系统", 180000), ("不是", "什么", 90000),
    ("一个", "测试", 100000), ("在", "北京", 130000), ("可以", "使用", 120000),
]


def fnv1a32(data: bytes) -> int:
    h = 0x811C9DC5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def mix64(x: int) -> int:
    x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9 & 0xFFFFFFFFFFFFFFFF
    x = (x ^ (x >> 27)) * 0x94D049BB133111EB & 0xFFFFFFFFFFFFFFFF
    return x ^ (x >> 31)


def bigram_sig(w1: bytes, w2: bytes) -> int:
    sig = (fnv1a32(w1) << 32) | fnv1a32(w2)
    return 1 if sig == 0 else sig


def normalize_syllable(raw: str) -> str | None:
    s = unicodedata.normalize("NFC", raw.strip().lower()).translate(TONE_TABLE)
    if ";" in s:
        s = s.split(";", 1)[0]
    if not s:
        return None
    if s in ("lü", "lüe", "lue"):
        s = "lv" if s == "lü" else "lve"
    elif s in ("nü", "nüe", "nue"):
        s = "nv" if s == "nü" else "nve"
    elif "ü" in s:
        s = s.replace("ü", "v" if s.startswith(("l", "n")) else "u")
    if s == "r":
        s = "er"
    return s if s in SYL_TO_ID else None


def is_clean_cjk_char(ch: str) -> bool:
    cp = ord(ch)
    # BMP CJK Unified Ideographs + Ext-A; drop rare Extension B..I surrogate blocks to keep dict sharp.
    return (0x4E00 <= cp <= 0x9FFF) or (0x3400 <= cp <= 0x4DBF) or cp == 0x3007


def is_clean_cjk_word(word: str) -> bool:
    return 1 <= len(word) <= 10 and all(is_clean_cjk_char(ch) for ch in word)


def freq_to_qlogp(freq: int, total_freq: float) -> int:
    prob = max(float(freq), 0.5) / max(total_freq, 1.0)
    val = int(round(math.log10(prob) * 100.0))
    return max(-3000, min(-10, val))


def fetch_cached(cache_dir: pathlib.Path, name: str, url: str, offline: bool) -> pathlib.Path | None:
    path = cache_dir / name
    if path.exists() and path.stat().st_size > 0:
        return path
    if offline:
        return None
    cache_dir.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".part")
    try:
        print(f"[spime-dict] downloading {name} ...", flush=True)
        req = urllib.request.Request(url, headers={"User-Agent": "SpaceKernel-spime/1.0"})
        with urllib.request.urlopen(req, timeout=120) as resp, open(tmp, "wb") as out:
            while True:
                chunk = resp.read(65536)
                if not chunk:
                    break
                out.write(chunk)
        tmp.rename(path)
        return path
    except (urllib.error.URLError, TimeoutError, OSError) as err:
        print(f"[spime-dict] warning: failed downloading {name}: {err}", file=sys.stderr)
        tmp.unlink(missing_ok=True)
        return None


def parse_rime_dict(path: pathlib.Path, max_entries: int | None = None):
    in_body = False
    rows: list[tuple[str, tuple[int, ...], int]] = []
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not in_body:
                if line == "...":
                    in_body = True
                continue
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            word = parts[0].strip()
            if not is_clean_cjk_word(word):
                continue
            raw_py = parts[1].strip()
            if ";" in raw_py:
                # Some wanxiang rows attach aux code to each syllable (`ni;aa hao;bb`).
                raw_syls = [tok.split(";", 1)[0] for tok in raw_py.split()]
            else:
                raw_syls = raw_py.split()
            if len(raw_syls) != len(word):
                continue
            syl_ids = []
            valid = True
            for rs in raw_syls:
                norm = normalize_syllable(rs)
                if norm is None:
                    valid = False
                    break
                syl_ids.append(SYL_TO_ID[norm])
            if not valid:
                continue
            weight = 100
            if len(parts) >= 3:
                w_str = parts[2].strip()
                if w_str.isdigit():
                    weight = max(1, int(w_str))
            rows.append((word, tuple(syl_ids), weight))
            if max_entries and len(rows) >= max_entries:
                break
    return rows


def parse_pinyin_data(path: pathlib.Path) -> list[tuple[str, tuple[int, ...], int]]:
    rows: list[tuple[str, tuple[int, ...], int]] = []
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if not line.startswith("U+"):
                continue
            left, _, _ = line.partition("#")
            if ":" not in left:
                continue
            cp_hex, pinyins = left.split(":", 1)
            try:
                ch = chr(int(cp_hex.strip()[2:], 16))
            except ValueError:
                continue
            if not is_clean_cjk_char(ch):
                continue
            for rank, raw in enumerate(pinyins.strip().split(",")):
                norm = normalize_syllable(raw)
                if norm is not None:
                    rows.append((ch, (SYL_TO_ID[norm],), max(1, 5 - rank)))
    return rows


def parse_english_dict(path: pathlib.Path, max_words: int = 45000) -> list[tuple[str, str, int]]:
    best: dict[str, tuple[str, int]] = {}
    in_body = False
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not in_body:
                if line == "...":
                    in_body = True
                continue
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            disp = parts[0].strip()
            if not disp or len(disp) > 32 or not all(32 <= ord(c) <= 126 for c in disp):
                continue
            key = "".join(c.lower() for c in disp if c.isascii() and c.isalnum())
            if len(key) < 2 or len(key) > 24:
                continue
            weight = 10
            if len(parts) >= 3 and parts[2].strip().isdigit():
                weight = max(1, int(parts[2].strip()))
            prev = best.get(key)
            if prev is None or weight > prev[1]:
                best[key] = (disp, weight)
    items = sorted(best.items(), key=lambda kv: -kv[1][1])[:max_words]
    return [(k, v[0], v[1]) for k, v in items]


def parse_mixed_dict(path: pathlib.Path) -> list[tuple[str, str, int]]:
    out: list[tuple[str, str, int]] = []
    in_body = False
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not in_body:
                if line == "...":
                    in_body = True
                continue
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            disp = parts[0].strip()
            raw_code = parts[1].strip().replace("'", " ")
            syl_parts = raw_code.split()
            key_chunks: list[str] = []
            for tok in syl_parts:
                norm = normalize_syllable(tok)
                if norm is not None:
                    key_chunks.append(norm)
                else:
                    clean = "".join(c.lower() for c in tok if c.isascii() and c.isalnum())
                    if clean:
                        key_chunks.append(clean)
            key = "".join(key_chunks)
            if not disp or len(key) < 2 or len(key) > 24 or len(disp.encode("utf-8")) > 48:
                continue
            out.append((key, disp, 4000))
    return out


class StringPool:
    def __init__(self) -> None:
        self.buf = bytearray()
        self.map: dict[bytes, tuple[int, int]] = {}

    def intern(self, text: str) -> tuple[int, int]:
        b = text.encode("utf-8")
        hit = self.map.get(b)
        if hit is not None:
            return hit
        off = len(self.buf)
        self.buf.extend(b)
        self.map[b] = (off, len(b))
        return off, len(b)


def build_dictionaries(cache_dir: pathlib.Path, out_dir: pathlib.Path, offline: bool) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    cached: dict[str, pathlib.Path] = {}
    for name, url in SOURCES.items():
        p = fetch_cached(cache_dir, name, url, offline)
        if p is not None:
            cached[name] = p

    using_wanxiang = "zi.dict.yaml" in cached and "jichu.dict.yaml" in cached
    # key_map: tuple[SylId,...] -> dict[word_str, (max_weight, origin)]
    # origin: 1 = core wanxiang, 2 = cuoyin/duoyin, 3 = diming, 4 = pinyin-data tail
    key_map: dict[tuple[int, ...], dict[str, tuple[int, int]]] = collections.defaultdict(dict)

    def add_row(word: str, syls: tuple[int, ...], weight: int, origin: int) -> None:
        slot = key_map[syls].get(word)
        if slot is None or weight > slot[0]:
            key_map[syls][word] = (weight, origin)

    if using_wanxiang:
        print("[spime-dict] parsing core Chinese dictionaries ...", flush=True)
        for word, syls, w in parse_rime_dict(cached["zi.dict.yaml"]):
            add_row(word, syls, max(w, 2), 1)
        if "pinyin.txt" in cached:
            for word, syls, w in parse_pinyin_data(cached["pinyin.txt"]):
                if (word, ) not in key_map[syls]:
                    add_row(word, syls, w, 4)

        jichu_rows = parse_rime_dict(cached["jichu.dict.yaml"])
        # Keep top ~850k highest-weight phrases (keeping everyday 2-4 char words well covered).
        jichu_rows.sort(key=lambda r: -r[2])
        for word, syls, w in jichu_rows[:850000]:
            add_row(word, syls, w, 1)

        for opt_name, cap, origin in (
            ("duoyin.dict.yaml", None, 2),
            ("cuoyin.dict.yaml", None, 2),
            ("diming.dict.yaml", 120000, 3),
        ):
            if opt_name in cached:
                rows = parse_rime_dict(cached[opt_name])
                rows.sort(key=lambda r: -r[2])
                if cap:
                    rows = rows[:cap]
                for word, syls, w in rows:
                    add_row(word, syls, w, origin)
    else:
        print("[spime-dict] offline/fallback mode: compiling built-in seed lexicon", flush=True)
        for word, py_str, w in FALLBACK_ENTRIES:
            syls = tuple(SYL_TO_ID[normalize_syllable(p)] for p in py_str.split())
            add_row(word, syls, w, 1)

    # Track per-word max frequency across readings (for unigram normalization & bigram synthesis).
    word_best_freq: dict[str, int] = {}
    total_freq = 0.0
    for entries in key_map.values():
        for word, (w, _) in entries.items():
            prev = word_best_freq.get(word, 0)
            if w > prev:
                word_best_freq[word] = w
            total_freq += float(w)
    total_freq = max(total_freq, 1_000_000.0)

    # Build English & Mixed entries.
    en_entries: list[tuple[str, str, int]] = []
    if "en.dict.yaml" in cached:
        en_entries.extend(parse_english_dict(cached["en.dict.yaml"], 45000))
    if "mixed.dict.yaml" in cached:
        existing_keys = {k for k, _, _ in en_entries}
        for k, disp, w in parse_mixed_dict(cached["mixed.dict.yaml"]):
            if k not in existing_keys:
                en_entries.append((k, disp, w))
                existing_keys.add(k)
    if not en_entries:
        en_entries = [
            ("linux", "Linux", 50000),
            ("spaceos", "SpaceOS", 50000),
            ("hello", "hello", 40000),
            ("world", "world", 38000),
            ("wifi", "Wi-Fi", 35000),
        ]
    en_total = sum(float(w) for _, _, w in en_entries) + total_freq * 0.05

    # Build Jianpin index: first letter of each syllable for 2..=5 char high-frequency words.
    jp_map: dict[bytes, list[tuple[str, int, int]]] = collections.defaultdict(list)
    for syls, entries in key_map.items():
        if not (2 <= len(syls) <= 5):
            continue
        jp_key = bytes(MANDARIN_SYLLABLES[sid][0].encode("ascii")[0] for sid in syls)
        for word, (w, origin) in entries.items():
            if w >= (30 if using_wanxiang else 1):
                jp_map[jp_key].append((word, w, origin))

    jp_sorted_keys = sorted(jp_map.keys())
    jp_records: list[tuple[bytes, list[tuple[str, int, int]]]] = []
    for k in jp_sorted_keys:
        cands = jp_map[k]
        # Deduplicate preserving highest frequency per word
        dedup: dict[str, tuple[int, int]] = {}
        for word, w, origin in cands:
            prev = dedup.get(word)
            if prev is None or w > prev[0]:
                dedup[word] = (w, origin)
        top_cands = sorted(((word, w, o) for word, (w, o) in dedup.items()), key=lambda x: -x[1])[:12]
        if top_cands:
            jp_records.append((k, top_cands))

    # Synthesize Bigram Language Model from valid multi-word decompositions + curated pairs.
    print("[spime-dict] synthesizing Bigram language model ...", flush=True)
    bigram_counts: dict[tuple[str, str], float] = collections.defaultdict(float)
    common_words = {w for w, f in word_best_freq.items() if f >= (120 if using_wanxiang else 1)}
    for word, freq in word_best_freq.items():
        n = len(word)
        if n < 3 or n > 8 or freq < (80 if using_wanxiang else 1):
            continue
        base_score = math.sqrt(float(freq))
        for split in range(1, n):
            left = word[:split]
            right = word[split:]
            if left in common_words and right in common_words:
                bigram_counts[(left, right)] += base_score

    for w1, w2, boost in CURATED_BIGRAMS:
        bigram_counts[(w1, w2)] += float(boost)

    # Keep top 550,000 bigrams cleanly inside 1M-slot hash table (~10 MiB).
    sorted_bg = sorted(bigram_counts.items(), key=lambda kv: -kv[1])[:550000]
    bg_total = sum(v for _, v in sorted_bg) + 1.0

    # Pack SPIMEDIC binary file.
    pool = StringPool()
    syl_names_bytes = b"\0".join(s.encode("ascii") for s in MANDARIN_SYLLABLES) + b"\0"

    sorted_keys = sorted(key_map.keys())
    # Build buckets for syl_count (first syllable ID).
    buckets = [[0, 0] for _ in range(len(MANDARIN_SYLLABLES))]
    key_recs = bytearray()
    key_syls = bytearray()
    entry_recs = bytearray()
    entry_idx = 0

    for k_idx, syls in enumerate(sorted_keys):
        first_sid = syls[0]
        if buckets[first_sid][1] == 0:
            buckets[first_sid][0] = k_idx
        buckets[first_sid][1] += 1

        cands = sorted(
            ((word, w, orig) for word, (w, orig) in key_map[syls].items()),
            key=lambda item: -item[1],
        )
        if len(syls) == 1:
            cands = cands[:180]
        else:
            cands = cands[:24]

        syl_off = len(key_syls) // 2
        for sid in syls:
            key_syls.extend(struct.pack("<H", sid))
        rec_start = entry_idx
        for word, w, orig in cands:
            w_off, w_len = pool.intern(word)
            if w_len > 255:
                continue
            qlogp = freq_to_qlogp(w, total_freq)
            entry_recs.extend(struct.pack("<IBBh", w_off, w_len, orig, qlogp))
            entry_idx += 1
        rec_count = entry_idx - rec_start
        key_recs.extend(struct.pack("<BBHII", len(syls), 0, rec_count, syl_off, rec_start))

    bucket_bytes = bytearray()
    for start_k, count_k in buckets:
        bucket_bytes.extend(struct.pack("<II", start_k, count_k))

    # Jianpin table
    jp_key_recs = bytearray()
    jp_entry_recs = bytearray()
    jp_entry_idx = 0
    for k_bytes, cands in jp_records:
        k_str = k_bytes.decode("ascii")
        k_off, k_len = pool.intern(k_str)
        rec_start = jp_entry_idx
        for word, w, orig in cands:
            w_off, w_len = pool.intern(word)
            if w_len > 255:
                continue
            qlogp = freq_to_qlogp(w, total_freq)
            jp_entry_recs.extend(struct.pack("<IBBh", w_off, w_len, orig, qlogp))
            jp_entry_idx += 1
        rec_count = jp_entry_idx - rec_start
        jp_key_recs.extend(struct.pack("<IBBHI", k_off, k_len, 0, rec_count, rec_start))

    # English & mixed keys sorted by ASCII lowercase key.
    en_sorted = sorted(en_entries, key=lambda x: x[0].encode("ascii"))
    en_key_recs = bytearray()
    for k_str, disp_str, w in en_sorted:
        k_off, k_len = pool.intern(k_str)
        d_off, d_len = pool.intern(disp_str)
        if k_len > 255 or d_len > 255:
            continue
        qlogp = freq_to_qlogp(w, en_total)
        en_key_recs.extend(struct.pack("<IIBBh", k_off, d_off, k_len, d_len, qlogp))

    def align4(n: int) -> int:
        return (n + 3) & ~3

    # Assemble SPIMEDIC v1 with 128-byte header.
    HEADER_SIZE = 128
    off_syl_names = HEADER_SIZE
    off_buckets = align4(off_syl_names + len(syl_names_bytes))
    off_keys = align4(off_buckets + len(bucket_bytes))
    off_key_syls = align4(off_keys + len(key_recs))
    off_entries = align4(off_key_syls + len(key_syls))
    off_jp_keys = align4(off_entries + len(entry_recs))
    off_jp_entries = align4(off_jp_keys + len(jp_key_recs))
    off_en_keys = align4(off_jp_entries + len(jp_entry_recs))
    off_str_pool = align4(off_en_keys + len(en_key_recs))
    total_size = off_str_pool + len(pool.buf)

    header = bytearray(HEADER_SIZE)
    struct.pack_into(
        "<8s21I",
        header,
        0,
        b"SPIMEDIC",
        1,  # version
        0,  # flags
        len(MANDARIN_SYLLABLES),
        off_syl_names,
        len(syl_names_bytes),
        off_buckets,
        len(MANDARIN_SYLLABLES),
        off_keys,
        len(sorted_keys),
        off_key_syls,
        len(key_syls) // 2,
        off_entries,
        entry_idx,
        off_jp_keys,
        len(jp_records),
        off_jp_entries,
        jp_entry_idx,
        off_en_keys,
        len(en_key_recs) // 12,
        off_str_pool,
        len(pool.buf),
    )

    dict_path = out_dir / "spime.dict"
    out_bytes = bytearray(total_size)
    out_bytes[0:HEADER_SIZE] = header
    out_bytes[off_syl_names : off_syl_names + len(syl_names_bytes)] = syl_names_bytes
    out_bytes[off_buckets : off_buckets + len(bucket_bytes)] = bucket_bytes
    out_bytes[off_keys : off_keys + len(key_recs)] = key_recs
    out_bytes[off_key_syls : off_key_syls + len(key_syls)] = key_syls
    out_bytes[off_entries : off_entries + len(entry_recs)] = entry_recs
    out_bytes[off_jp_keys : off_jp_keys + len(jp_key_recs)] = jp_key_recs
    out_bytes[off_jp_entries : off_jp_entries + len(jp_entry_recs)] = jp_entry_recs
    out_bytes[off_en_keys : off_en_keys + len(en_key_recs)] = en_key_recs
    out_bytes[off_str_pool : off_str_pool + len(pool.buf)] = pool.buf
    dict_path.write_bytes(out_bytes)

    # Assemble SPIMELM v1 hash table.
    n_bg = len(sorted_bg)
    cap = 1024
    while cap < max(n_bg * 10 // 7, 1024):
        cap *= 2
    mask = cap - 1
    lm_keys = [0] * cap
    lm_vals = [0] * cap
    placed = 0
    for (w1, w2), score in sorted_bg:
        sig = bigram_sig(w1.encode("utf-8"), w2.encode("utf-8"))
        qlogp = freq_to_qlogp(int(max(1.0, score)), bg_total)
        slot = mix64(sig) & mask
        while lm_keys[slot] != 0 and lm_keys[slot] != sig:
            slot = (slot + 1) & mask
        if lm_keys[slot] == 0:
            placed += 1
        if lm_keys[slot] == 0 or qlogp > lm_vals[slot]:
            lm_keys[slot] = sig
            lm_vals[slot] = qlogp

    lm_keys_bytes = struct.pack(f"<{cap}Q", *lm_keys)
    lm_vals_bytes = struct.pack(f"<{cap}h", *lm_vals)
    lm_header = bytearray(64)
    keys_off = 64
    vals_off = keys_off + len(lm_keys_bytes)
    struct.pack_into(
        "<8sIIIhHhHII",
        lm_header,
        0,
        b"SPIMELM\0",
        1,
        cap,
        placed,
        -80,   # backoff penalty (-0.8 log10)
        0,
        -750,  # default unseen unigram (-7.5 log10)
        0,
        keys_off,
        vals_off,
    )
    lm_path = out_dir / "spime.lm"
    with open(lm_path, "wb") as f:
        f.write(lm_header)
        f.write(lm_keys_bytes)
        f.write(lm_vals_bytes)

    notice_path = out_dir / "NOTICE.txt"
    notice_path.write_text(
        "SPIME Dictionary & Bigram Language Model\n"
        "- Chinese word & pronunciation lexicons derived from amzxyz/rime_wanxiang (CC-BY-4.0)\n"
        "- Additional character pinyin table from mozillazg/pinyin-data (MIT)\n",
        encoding="utf-8",
    )

    print(
        f"[spime-dict] built {dict_path} ({dict_path.stat().st_size / 1048576:.2f} MiB, "
        f"{len(sorted_keys)} pinyin keys, {entry_idx} entries, {len(jp_records)} jianpin keys, "
        f"{len(en_key_recs) // 12} en/mixed keys)"
    )
    print(
        f"[spime-dict] built {lm_path} ({lm_path.stat().st_size / 1048576:.2f} MiB, "
        f"{placed} bigrams in {cap} slots)"
    )


def verify_compiled(out_dir: pathlib.Path) -> None:
    dict_data = (out_dir / "spime.dict").read_bytes()
    magic, ver, _, syl_cnt, syl_off, syl_len, b_off, b_cnt, k_off, k_cnt, ks_off, ks_len, e_off, e_cnt, jpk_off, jpk_cnt, jpe_off, jpe_cnt, en_off, en_cnt, sp_off, sp_len = struct.unpack_from(
        "<8s21I", dict_data, 0
    )
    assert magic == b"SPIMEDIC" and ver == 1
    syl_list = dict_data[syl_off : syl_off + syl_len].rstrip(b"\0").split(b"\0")
    assert len(syl_list) == syl_cnt == len(MANDARIN_SYLLABLES)
    syl_idx = {s.decode("ascii"): i for i, s in enumerate(syl_list)}
    str_pool = dict_data[sp_off : sp_off + sp_len]

    def lookup_py(syls: list[str]) -> list[tuple[str, int]]:
        ids = [syl_idx[s] for s in syls]
        b_start, b_count = struct.unpack_from("<II", dict_data, b_off + ids[0] * 8)
        lo, hi = b_start, b_start + b_count
        while lo < hi:
            mid = (lo + hi) // 2
            kl, _, ecnt, soff, estart = struct.unpack_from("<BBHII", dict_data, k_off + mid * 12)
            mid_ids = list(struct.unpack_from(f"<{kl}H", dict_data, ks_off + soff * 2))
            if mid_ids < ids:
                lo = mid + 1
            elif mid_ids > ids:
                hi = mid
            else:
                res = []
                for idx in range(estart, estart + ecnt):
                    woff, wlen, _, ql = struct.unpack_from("<IBBh", dict_data, e_off + idx * 8)
                    res.append((str_pool[woff : woff + wlen].decode("utf-8"), ql))
                return res
        return []

    for query, expect in [
        (["ni", "hao"], "你好"),
        (["la", "ji"], "垃圾"),
        (["zhong", "guo"], "中国"),
        (["bei", "jing"], "北京"),
        (["wo", "men"], "我们"),
        (["shu", "ru", "fa"], "输入法"),
    ]:
        cands = lookup_py(query)
        assert cands and any(w == expect for w, _ in cands), f"missing {expect} for {query}: {cands}"

    lm_data = (out_dir / "spime.lm").read_bytes()
    lm_magic, lm_ver, cap, placed, _, _, _, _, lk_off, lv_off = struct.unpack_from("<8sIIIhHhHII", lm_data, 0)
    assert lm_magic == b"SPIMELM\0" and lm_ver == 1 and placed > 0

    def lookup_bg(w1: str, w2: str) -> int | None:
        sig = bigram_sig(w1.encode("utf-8"), w2.encode("utf-8"))
        mask = cap - 1
        slot = mix64(sig) & mask
        while True:
            k = struct.unpack_from("<Q", lm_data, lk_off + slot * 8)[0]
            if k == 0:
                return None
            if k == sig:
                return struct.unpack_from("<h", lm_data, lv_off + slot * 2)[0]
            slot = (slot + 1) & mask

    assert lookup_bg("今天", "天气") is not None
    assert lookup_bg("你好", "世界") is not None
    print("[spime-dict] verification PASSED!")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--cache-dir", type=pathlib.Path, default=DEFAULT_CACHE)
    ap.add_argument("--out-dir", type=pathlib.Path, default=DEFAULT_OUT)
    ap.add_argument("--offline", action="store_true", help="Only use cached dicts or built-in fallback")
    ap.add_argument("--verify", action="store_true", help="Run roundtrip lookup verification")
    args = ap.parse_args()
    build_dictionaries(args.cache_dir, args.out_dir, args.offline)
    if args.verify:
        verify_compiled(args.out_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main())
