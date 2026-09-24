//! Binary framing of MTGO's `Match_GameLog_<guid>.dat` / `Match_GameChat_<guid>.dat` files.
//!
//! Layout (reverse-engineered, validated against every file on disk):
//!
//! ```text
//! u16 LE version (1)
//! str  match guid            ; str = 7-bit-encoded length (.NET BinaryWriter) + UTF-8 bytes
//! u16 LE kind                ; 4 = game log, 3 = game chat
//! str  match guid (again)
//! record*                    ; until EOF
//!   i64 LE .NET DateTime ticks (100ns since 0001-01-01)
//!   str  sender              ; "" for system lines, a username for chat lines
//!   str  text
//! ```

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use std::fmt;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum FormatError {
    Io(std::io::Error),
    Truncated { offset: usize, what: &'static str },
    Utf8 { offset: usize },
    BadVersion(u16),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Io(e) => write!(f, "io: {e}"),
            FormatError::Truncated { offset, what } => write!(f, "truncated reading {what} at {offset}"),
            FormatError::Utf8 { offset } => write!(f, "invalid utf-8 at {offset}"),
            FormatError::BadVersion(v) => write!(f, "unexpected version {v}"),
        }
    }
}

impl From<std::io::Error> for FormatError {
    fn from(e: std::io::Error) -> Self {
        FormatError::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    GameLog,
    GameChat,
    Other(u16),
}

#[derive(Debug, Clone)]
pub struct RawRecord {
    pub ts: NaiveDateTime,
    pub sender: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RawLog {
    pub version: u16,
    pub guid: String,
    pub kind: LogKind,
    pub records: Vec<RawRecord>,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u16(&mut self) -> Result<u16, FormatError> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i64(&mut self) -> Result<i64, FormatError> {
        let b = self.take(8, "i64")?;
        Ok(i64::from_le_bytes(b.try_into().unwrap()))
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], FormatError> {
        if self.pos + n > self.buf.len() {
            return Err(FormatError::Truncated { offset: self.pos, what });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// .NET BinaryWriter.Write(string): 7-bit encoded length prefix, then UTF-8.
    fn str(&mut self) -> Result<String, FormatError> {
        let mut len: usize = 0;
        let mut shift = 0;
        loop {
            let b = self.take(1, "string length")?[0];
            len |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        let offset = self.pos;
        let bytes = self.take(len, "string body")?;
        String::from_utf8(bytes.to_vec()).map_err(|_| FormatError::Utf8 { offset })
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }
}

/// .NET DateTime ticks (100ns since 0001-01-01T00:00:00) → NaiveDateTime.
pub fn ticks_to_datetime(ticks: i64) -> NaiveDateTime {
    let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    epoch + TimeDelta::microseconds(ticks / 10)
}

pub fn parse_bytes(buf: &[u8]) -> Result<RawLog, FormatError> {
    let mut c = Cursor { buf, pos: 0 };
    let version = c.u16()?;
    if version != 1 {
        return Err(FormatError::BadVersion(version));
    }
    let guid = c.str()?;
    let kind = match c.u16()? {
        4 => LogKind::GameLog,
        3 => LogKind::GameChat,
        k => LogKind::Other(k),
    };
    let _guid2 = c.str()?;
    let mut records = Vec::new();
    while !c.at_end() {
        let ts = ticks_to_datetime(c.i64()?);
        let sender = c.str()?;
        let text = c.str()?;
        records.push(RawRecord { ts, sender, text });
    }
    Ok(RawLog { version, guid, kind, records })
}

/// A game log with every chat record removed. Chat records carry a sender; live game logs carry none
/// and replays carry the acting player on every record, so only a mix is chat. Returns None when nothing
/// had to go, Err for anything that is not a game log (a chat file has kind 3). A truncated tail (a log
/// still being written) is kept as it is.
pub fn strip_chat(buf: &[u8]) -> Result<Option<Vec<u8>>, FormatError> {
    let mut c = Cursor { buf, pos: 0 };
    let version = c.u16()?;
    if version != 1 {
        return Err(FormatError::BadVersion(version));
    }
    c.str()?;
    if c.u16()? != 4 {
        return Err(FormatError::BadVersion(3));
    }
    c.str()?;
    let head = c.pos;
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    let mut tail = buf.len();
    while !c.at_end() {
        let start = c.pos;
        let rec = (|| -> Result<bool, FormatError> {
            c.i64()?;
            let sender = c.str()?;
            c.str()?;
            Ok(!sender.is_empty())
        })();
        match rec {
            Ok(has_sender) => spans.push((start, c.pos, has_sender)),
            Err(_) => {
                tail = start;
                break;
            }
        }
    }
    let with = spans.iter().filter(|s| s.2).count();
    if with == 0 || with == spans.len() {
        return Ok(None);
    }
    let mut out = buf[..head].to_vec();
    for (a, b, has_sender) in &spans {
        if !has_sender {
            out.extend_from_slice(&buf[*a..*b]);
        }
    }
    out.extend_from_slice(&buf[tail..]);
    Ok(Some(out))
}

pub fn read_log(path: &Path) -> Result<RawLog, FormatError> {
    let buf = fs::read(path)?;
    parse_bytes(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(bytes: &mut Vec<u8>, text: &str) {
        let mut len = text.len();
        loop {
            let b = (len & 0x7f) as u8;
            len >>= 7;
            if len == 0 {
                bytes.push(b);
                break;
            }
            bytes.push(b | 0x80);
        }
        bytes.extend_from_slice(text.as_bytes());
    }

    #[test]
    fn roundtrip_minimal_log() {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_le_bytes());
        s(&mut b, "abc-guid");
        b.extend_from_slice(&4u16.to_le_bytes());
        s(&mut b, "abc-guid");
        // 2026-01-01 00:00:00 in ticks
        let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let ticks = (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap() - epoch).num_microseconds().unwrap() * 10;
        b.extend_from_slice(&ticks.to_le_bytes());
        s(&mut b, "");
        s(&mut b, &"@PTurn 1: boryas".repeat(1)); 
        b.extend_from_slice(&(ticks + 10_000_000).to_le_bytes());
        s(&mut b, "boryas");
        s(&mut b, &"x".repeat(200)); // exercises 2-byte length
        let log = parse_bytes(&b).unwrap();
        assert_eq!(log.guid, "abc-guid");
        assert_eq!(log.kind, LogKind::GameLog);
        assert_eq!(log.records.len(), 2);
        assert_eq!(log.records[0].text, "@PTurn 1: boryas");
        assert_eq!(log.records[0].ts.to_string(), "2026-01-01 00:00:00");
        assert_eq!(log.records[1].sender, "boryas");
        assert_eq!(log.records[1].text.len(), 200);
        assert_eq!(log.records[1].ts.to_string(), "2026-01-01 00:00:01");
    }

    fn w7(out: &mut Vec<u8>, s: &str) {
        let mut n = s.len();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
        out.extend_from_slice(s.as_bytes());
    }
    fn frame(kind: u16, recs: &[(&str, &str)]) -> Vec<u8> {
        let mut b = vec![1, 0];
        w7(&mut b, "g");
        b.extend_from_slice(&kind.to_le_bytes());
        w7(&mut b, "g");
        for (s, t) in recs {
            b.extend_from_slice(&0i64.to_le_bytes());
            w7(&mut b, s);
            w7(&mut b, t);
        }
        b
    }

    #[test]
    fn strips_only_chat_records() {
        let live = frame(4, &[("", "@PTurn 1: a"), ("", "@Pa draws a card.")]);
        assert!(strip_chat(&live).unwrap().is_none());
        let replay = frame(4, &[("a", "Turn 1"), ("b", "draws")]);
        assert!(strip_chat(&replay).unwrap().is_none());
        let mixed = frame(4, &[("", "@PTurn 1: a"), ("b", "gl hf"), ("", "@Pa draws a card.")]);
        let out = strip_chat(&mixed).unwrap().unwrap();
        assert_eq!(out, frame(4, &[("", "@PTurn 1: a"), ("", "@Pa draws a card.")]));
        assert!(strip_chat(&frame(3, &[("b", "hello")])).is_err());
    }
}
