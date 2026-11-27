//! Minimal MS-NRBF (.NET `BinaryFormatter`) record walker.
//!
//! Specification: <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nrbf/>
//!
//! The walker does not build an object graph. It traverses every record to
//! keep the stream position correct, produces an indented text listing of the
//! records, and reports the byte locations of the two money values stored in
//! a Wobbly Life `PlayerData_N.sav` file:
//!
//! * `PlayerMiscData.money`: an inline Int32 member (Wobbly Island money).
//! * `PlayerActiveExternalData.data`: a JSON string; the Space data entry
//!   contains a top-level `"money":<integer>` key (Space money).
//!
//! NRBF contains no absolute byte offsets, so replacing a length-prefixed
//! string with one of a different length leaves the rest of the stream valid.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::ops::Range;
use std::rc::Rc;

use anyhow::{Context, Result, bail, ensure};

const MAX_DEPTH: u32 = 1024;

/// Primitive arrays longer than this are listed in truncated form.
const ARRAY_PREVIEW: usize = 32;

const PRIM_INT32: u8 = 8;
const PRIM_INT16: u8 = 7;
const PRIM_BYTE: u8 = 2;

const ISLAND_CLASS: &str = "PlayerMiscData";
const ISLAND_MEMBER: &str = "money";
const EXTERNAL_CLASS: &str = "PlayerActiveExternalData";
const EXTERNAL_MEMBER: &str = "data";
const JSON_MONEY_KEY: &str = "\"money\":";

/// A located money value.
#[derive(Debug)]
pub enum Finding<'a> {
    /// Int32 at `offset..offset + 4`.
    Island { offset: usize, value: i32 },
    /// Length-prefixed string occupying `span` (prefix included). `digits` is
    /// the range of the integer inside `text`.
    Space {
        span: Range<usize>,
        text: &'a str,
        digits: Range<usize>,
        value: i64,
    },
}

/// Result of walking a stream.
pub struct Parsed<'a> {
    /// Money values in stream order.
    pub findings: Vec<Finding<'a>>,
    /// Indented text listing of every record.
    pub listing: String,
}

#[derive(Clone, Copy)]
enum MemberType {
    Primitive(u8),
    /// Member value is encoded as a separate record.
    Record,
}

struct Member<'a> {
    name: &'a str,
    ty: MemberType,
}

struct ClassMeta<'a> {
    full_name: &'a str,
    /// `full_name` without namespace or enclosing type.
    name: &'a str,
    members: Vec<Member<'a>>,
    /// `System.Guid` with its standard member layout; listed as one value.
    is_guid: bool,
}

enum Rec {
    Value,
    /// `ObjectNullMultiple*`: occupies the given number of element slots.
    Nulls(usize),
    /// `BinaryLibrary`: precedes the record that occupies the slot.
    Library,
    End,
}

/// A decoded primitive value.
enum Prim<'a> {
    Bool(bool),
    Byte(u8),
    SByte(i8),
    Char(char),
    Int16(i16),
    UInt16(u16),
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    Single(f32),
    Double(f64),
    /// Decimal is encoded as a string.
    Decimal(&'a str),
    TimeSpan(i64),
    DateTime(i64),
    Null,
    Str(&'a str),
}

impl fmt::Display for Prim<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::Byte(v) => write!(f, "{v}"),
            Self::SByte(v) => write!(f, "{v}"),
            Self::Char(v) => write!(f, "{v:?}"),
            Self::Int16(v) => write!(f, "{v}"),
            Self::UInt16(v) => write!(f, "{v}"),
            Self::Int32(v) => write!(f, "{v}"),
            Self::UInt32(v) => write!(f, "{v}"),
            Self::Int64(v) => write!(f, "{v}"),
            Self::UInt64(v) => write!(f, "{v}"),
            Self::Single(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::Decimal(v) => write!(f, "{v}m"),
            Self::TimeSpan(v) => write!(f, "TimeSpan({v})"),
            Self::DateTime(v) => write!(f, "DateTime(0x{v:016x})"),
            Self::Null => f.write_str("null"),
            Self::Str(v) => write!(f, "{v:?}"),
        }
    }
}

fn prim_name(t: u8) -> &'static str {
    match t {
        1 => "Boolean",
        2 => "Byte",
        3 => "Char",
        5 => "Decimal",
        6 => "Double",
        7 => "Int16",
        8 => "Int32",
        9 => "Int64",
        10 => "SByte",
        11 => "Single",
        12 => "TimeSpan",
        13 => "DateTime",
        14 => "UInt16",
        15 => "UInt32",
        16 => "UInt64",
        17 => "Null",
        18 => "String",
        _ => "?",
    }
}

/// (class short name, member name) of the member whose value is being read.
type Ctx<'a> = Option<(&'a str, &'a str)>;

struct Walker<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Class metadata is shared between the defining record and later
    /// `ClassWithId` records; `Rc` avoids copying the member list.
    classes: HashMap<i32, Rc<ClassMeta<'a>>>,
    findings: Vec<Finding<'a>>,
    listing: String,
}

/// Walks the entire stream; returns the money values and a record listing.
pub fn parse(buf: &[u8]) -> Result<Parsed<'_>> {
    let mut w = Walker {
        buf,
        pos: 0,
        classes: HashMap::new(),
        findings: Vec::new(),
        listing: String::with_capacity(buf.len() * 4),
    };
    ensure!(w.u8()? == 0, "not an NRBF stream (missing SerializationHeader)");
    let h = w.take(16)?;
    let field = |i: usize| i32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    w.line(
        0,
        format_args!(
            "header root=#{} header=#{} version={}.{}",
            field(0),
            field(4),
            field(8),
            field(12)
        ),
    );
    loop {
        if let Rec::End = w.record(0, None, "")? {
            break;
        }
    }
    let trailing = buf.len() - w.pos;
    if trailing > 0 {
        w.line(0, format_args!("({trailing} trailing bytes after MessageEnd)"));
    }
    Ok(Parsed {
        findings: w.findings,
        listing: w.listing,
    })
}

impl<'a> Walker<'a> {
    fn line(&mut self, depth: u32, args: fmt::Arguments<'_>) {
        for _ in 0..depth {
            self.listing.push_str("  ");
        }
        // Writing to a String cannot fail.
        let _ = self.listing.write_fmt(args);
        self.listing.push('\n');
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .with_context(|| format!("unexpected end of data at byte {}", self.pos))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn count(&mut self) -> Result<usize> {
        let at = self.pos;
        let n = self.i32()?;
        usize::try_from(n).with_context(|| format!("negative count {n} at byte {at}"))
    }

    fn string(&mut self) -> Result<&'a str> {
        let at = self.pos;
        let mut len: usize = 0;
        for i in 0..5 {
            let b = self.u8()?;
            len |= usize::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                let bytes = self.take(len)?;
                return std::str::from_utf8(bytes)
                    .with_context(|| format!("invalid UTF-8 string at byte {at}"));
            }
        }
        bail!("string length prefix exceeds 5 bytes at byte {at}")
    }

    fn prim(&mut self, t: u8) -> Result<Prim<'a>> {
        Ok(match t {
            1 => Prim::Bool(self.u8()? != 0),
            2 => Prim::Byte(self.u8()?),
            3 => {
                let at = self.pos;
                let lead = self.buf.get(at).copied().unwrap_or(0);
                let len = match lead.leading_ones() {
                    0 => 1,
                    n @ 2..=4 => n as usize,
                    _ => bail!("invalid UTF-8 char lead byte at byte {at}"),
                };
                let bytes = self.take(len)?;
                let c = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|s| s.chars().next())
                    .with_context(|| format!("invalid UTF-8 char at byte {at}"))?;
                Prim::Char(c)
            }
            5 => Prim::Decimal(self.string()?),
            6 => Prim::Double(f64::from_le_bytes(self.array()?)),
            7 => Prim::Int16(i16::from_le_bytes(self.array()?)),
            8 => Prim::Int32(self.i32()?),
            9 => Prim::Int64(i64::from_le_bytes(self.array()?)),
            10 => Prim::SByte(i8::from_le_bytes(self.array()?)),
            11 => Prim::Single(f32::from_le_bytes(self.array()?)),
            12 => Prim::TimeSpan(i64::from_le_bytes(self.array()?)),
            13 => Prim::DateTime(i64::from_le_bytes(self.array()?)),
            14 => Prim::UInt16(u16::from_le_bytes(self.array()?)),
            15 => Prim::UInt32(u32::from_le_bytes(self.array()?)),
            16 => Prim::UInt64(u64::from_le_bytes(self.array()?)),
            17 => Prim::Null,
            18 => Prim::Str(self.string()?),
            _ => bail!("unknown primitive type {t} at byte {}", self.pos),
        })
    }

    /// Reads `n` primitives of type `t`; lists at most `ARRAY_PREVIEW` of them.
    fn prim_run(&mut self, t: u8, n: usize) -> Result<String> {
        let mut s = String::from("[");
        for i in 0..n {
            let v = self.prim(t)?;
            if i < ARRAY_PREVIEW {
                if i > 0 {
                    s.push_str(", ");
                }
                let _ = write!(s, "{v}");
            }
        }
        if n > ARRAY_PREVIEW {
            let _ = write!(s, ", ... ({} more)", n - ARRAY_PREVIEW);
        }
        s.push(']');
        Ok(s)
    }

    fn type_info(&mut self, binary_type: u8) -> Result<MemberType> {
        match binary_type {
            0 => Ok(MemberType::Primitive(self.u8()?)),
            1 | 2 | 5 | 6 => Ok(MemberType::Record),
            3 => self.string().map(|_| MemberType::Record),
            4 => {
                self.string()?;
                self.i32()?;
                Ok(MemberType::Record)
            }
            7 => self.u8().map(|_| MemberType::Record),
            _ => bail!("unknown binary type {binary_type} at byte {}", self.pos),
        }
    }

    /// Reads `ClassInfo`, plus `MemberTypeInfo` when `typed`, and registers it.
    fn class_def(&mut self, typed: bool, has_library: bool) -> Result<(i32, Rc<ClassMeta<'a>>)> {
        let oid = self.i32()?;
        let full_name = self.string()?;
        let n = self.count()?;
        let mut names = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            names.push(self.string()?);
        }
        let mut members = Vec::with_capacity(names.len());
        if typed {
            let kinds = self.take(n)?;
            for (&name, &kind) in names.iter().zip(kinds) {
                members.push(Member {
                    name,
                    ty: self.type_info(kind)?,
                });
            }
        } else {
            members.extend(names.iter().map(|&name| Member {
                name,
                ty: MemberType::Record,
            }));
        }
        if has_library {
            self.i32()?;
        }
        let name = full_name.rsplit(['.', '+']).next().unwrap_or(full_name);
        let is_guid = full_name == "System.Guid" && is_guid_layout(&members);
        let meta = Rc::new(ClassMeta {
            full_name,
            name,
            members,
            is_guid,
        });
        self.classes.insert(oid, Rc::clone(&meta));
        Ok((oid, meta))
    }

    /// Lists the class instance header and reads its member values.
    fn instance(&mut self, oid: i32, meta: &ClassMeta<'a>, depth: u32, label: &str) -> Result<()> {
        if meta.is_guid {
            let a = u32::from_le_bytes(self.array()?);
            let b = u16::from_le_bytes(self.array()?);
            let c = u16::from_le_bytes(self.array()?);
            let d: [u8; 8] = self.array()?;
            self.line(
                depth,
                format_args!(
                    "{label}#{oid} Guid {a:08x}-{b:04x}-{c:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
                ),
            );
            return Ok(());
        }
        self.line(depth, format_args!("{label}#{oid} {}", meta.full_name));
        for m in &meta.members {
            match m.ty {
                MemberType::Primitive(t) => {
                    let offset = self.pos;
                    let v = self.prim(t)?;
                    let mut note = "";
                    if meta.name == ISLAND_CLASS && m.name == ISLAND_MEMBER {
                        let Prim::Int32(value) = v else {
                            bail!("{ISLAND_CLASS}.{ISLAND_MEMBER} is {}, expected Int32", prim_name(t));
                        };
                        self.findings.push(Finding::Island { offset, value });
                        note = "    <-- Wobbly Island money";
                    }
                    self.line(depth + 1, format_args!("{}: {} = {v}{note}", m.name, prim_name(t)));
                }
                MemberType::Record => {
                    let label = format!("{} = ", m.name);
                    if !matches!(self.value(depth + 1, Some((meta.name, m.name)), &label)?, Rec::Value) {
                        bail!("invalid record for member {}.{} at byte {}", meta.name, m.name, self.pos);
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads one slot, including any `BinaryLibrary` records that precede it.
    fn value(&mut self, depth: u32, ctx: Ctx<'a>, label: &str) -> Result<Rec> {
        loop {
            match self.record(depth, ctx, label)? {
                Rec::Library => {}
                r => return Ok(r),
            }
        }
    }

    fn elements(&mut self, n: usize, depth: u32) -> Result<()> {
        let mut k = 0;
        while k < n {
            let label = format!("[{k}] = ");
            match self.value(depth, None, &label)? {
                Rec::Value => k += 1,
                Rec::Nulls(c) => k = k.saturating_add(c),
                _ => bail!("unexpected record inside array at byte {}", self.pos),
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn record(&mut self, depth: u32, ctx: Ctx<'a>, label: &str) -> Result<Rec> {
        ensure!(depth < MAX_DEPTH, "nesting exceeds {MAX_DEPTH} levels");
        let at = self.pos;
        let rt = self.u8()?;
        match rt {
            // ClassWithId
            1 => {
                let oid = self.i32()?;
                let meta_id = self.i32()?;
                let meta = self
                    .classes
                    .get(&meta_id)
                    .map(Rc::clone)
                    .with_context(|| format!("unknown metadata id {meta_id} at byte {at}"))?;
                self.instance(oid, &meta, depth, label)?;
            }
            // SystemClassWithMembers, ClassWithMembers,
            // SystemClassWithMembersAndTypes, ClassWithMembersAndTypes
            2..=5 => {
                let (oid, meta) = self.class_def(rt >= 4, rt == 3 || rt == 5)?;
                self.instance(oid, &meta, depth, label)?;
            }
            // BinaryObjectString
            6 => {
                let oid = self.i32()?;
                let start = self.pos;
                let text = self.string()?;
                let mut note = "";
                if ctx == Some((EXTERNAL_CLASS, EXTERNAL_MEMBER))
                    && let Some((digits, value)) = json_money(text)
                {
                    self.findings.push(Finding::Space {
                        span: start..self.pos,
                        text,
                        digits,
                        value,
                    });
                    note = "    <-- Space money";
                }
                self.line(depth, format_args!("{label}#{oid} string {text:?}{note}"));
            }
            // BinaryArray
            7 => {
                let oid = self.i32()?;
                let array_type = self.u8()?;
                let rank = self.count()?;
                let mut lengths = Vec::with_capacity(rank.min(32));
                let mut total: usize = 1;
                for _ in 0..rank {
                    let len = self.count()?;
                    total = total.checked_mul(len).context("array size overflow")?;
                    lengths.push(len);
                }
                if (3..=5).contains(&array_type) {
                    self.take(rank.checked_mul(4).context("array rank overflow")?)?;
                }
                let kind = self.u8()?;
                match self.type_info(kind)? {
                    MemberType::Primitive(t) => {
                        let values = self.prim_run(t, total)?;
                        self.line(depth, format_args!("{label}#{oid} {}{lengths:?} {values}", prim_name(t)));
                    }
                    MemberType::Record => {
                        self.line(depth, format_args!("{label}#{oid} array{lengths:?}"));
                        self.elements(total, depth + 1)?;
                    }
                }
            }
            // MemberPrimitiveTyped
            8 => {
                let t = self.u8()?;
                let v = self.prim(t)?;
                self.line(depth, format_args!("{label}{} {v}", prim_name(t)));
            }
            // MemberReference
            9 => {
                let id = self.i32()?;
                self.line(depth, format_args!("{label}ref #{id}"));
            }
            // ObjectNull
            10 => self.line(depth, format_args!("{label}null")),
            11 => {
                self.line(depth, format_args!("end"));
                return Ok(Rec::End);
            }
            // BinaryLibrary
            12 => {
                let id = self.i32()?;
                let name = self.string()?;
                self.line(depth, format_args!("library #{id} {name:?}"));
                return Ok(Rec::Library);
            }
            13 | 14 => {
                let n = if rt == 13 {
                    usize::from(self.u8()?)
                } else {
                    self.count()?
                };
                self.line(depth, format_args!("{label}null x{n}"));
                return Ok(Rec::Nulls(n));
            }
            // ArraySinglePrimitive
            15 => {
                let oid = self.i32()?;
                let n = self.count()?;
                let t = self.u8()?;
                let values = self.prim_run(t, n)?;
                self.line(depth, format_args!("{label}#{oid} {}[{n}] {values}", prim_name(t)));
            }
            // ArraySingleObject, ArraySingleString
            16 | 17 => {
                let oid = self.i32()?;
                let n = self.count()?;
                let kind = if rt == 16 { "object" } else { "string" };
                self.line(depth, format_args!("{label}#{oid} {kind}[{n}]"));
                self.elements(n, depth + 1)?;
            }
            _ => bail!("unsupported record type {rt} at byte {at}"),
        }
        Ok(Rec::Value)
    }
}

fn is_guid_layout(members: &[Member<'_>]) -> bool {
    let expected = [
        PRIM_INT32, PRIM_INT16, PRIM_INT16, PRIM_BYTE, PRIM_BYTE, PRIM_BYTE, PRIM_BYTE, PRIM_BYTE,
        PRIM_BYTE, PRIM_BYTE, PRIM_BYTE,
    ];
    members.len() == expected.len()
        && members
            .iter()
            .zip(expected)
            .all(|(m, e)| matches!(m.ty, MemberType::Primitive(t) if t == e))
}

/// Locates the integer after the single top-level `"money":` key.
fn json_money(text: &str) -> Option<(Range<usize>, i64)> {
    if text.matches(JSON_MONEY_KEY).count() != 1 {
        return None;
    }
    let key = text.find(JSON_MONEY_KEY)?;
    let bytes = text.as_bytes();
    let mut start = key + JSON_MONEY_KEY.len();
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    let mut end = start;
    if bytes.get(end) == Some(&b'-') {
        end += 1;
    }
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let value = text.get(start..end)?.parse().ok()?;
    Some((start..end, value))
}

#[allow(clippy::cast_possible_truncation)]
fn push_length_prefix(out: &mut Vec<u8>, mut len: usize) {
    loop {
        // INVARIANT: masked to 7 bits.
        let b = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A single modification produced by `apply`, for reporting.
pub struct Change {
    pub what: &'static str,
    pub offset: usize,
    pub old: i64,
    pub new: i64,
    /// Old and new byte length of the enclosing string payload, if any.
    pub string_len: Option<(usize, usize)>,
}

/// Returns a copy of `buf` with every finding set to the given values, plus
/// a description of each modification. `findings` must come from
/// `parse(buf)` (stream order, non-overlapping).
pub fn apply(buf: &[u8], findings: &[Finding<'_>], island: i32, space: i64) -> (Vec<u8>, Vec<Change>) {
    let mut out = Vec::with_capacity(buf.len() + 16);
    let mut changes = Vec::with_capacity(findings.len());
    let mut cursor = 0;
    for f in findings {
        match f {
            Finding::Island { offset, value } => {
                out.extend_from_slice(&buf[cursor..*offset]);
                out.extend_from_slice(&island.to_le_bytes());
                cursor = offset + 4;
                changes.push(Change {
                    what: "Wobbly Island money (Int32)",
                    offset: *offset,
                    old: i64::from(*value),
                    new: i64::from(island),
                    string_len: None,
                });
            }
            Finding::Space { span, text, digits, value } => {
                out.extend_from_slice(&buf[cursor..span.start]);
                let replacement = space.to_string();
                let len = text.len() - digits.len() + replacement.len();
                push_length_prefix(&mut out, len);
                out.extend_from_slice(&text.as_bytes()[..digits.start]);
                out.extend_from_slice(replacement.as_bytes());
                out.extend_from_slice(&text.as_bytes()[digits.end..]);
                cursor = span.end;
                changes.push(Change {
                    what: "Space money (JSON string)",
                    offset: span.start,
                    old: *value,
                    new: space,
                    string_len: Some((text.len(), len)),
                });
            }
        }
    }
    out.extend_from_slice(&buf[cursor..]);
    (out, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_money_locates_value() {
        let t = r#"{"money":99982069,"currentHelmetGuid":"x"}"#;
        let (r, v) = json_money(t).unwrap();
        assert_eq!(v, 99_982_069);
        assert_eq!(&t[r], "99982069");
        assert!(json_money(r#"{"presentWorldDatas":[]}"#).is_none());
    }

    #[test]
    fn length_prefix_encoding() {
        let mut v = Vec::new();
        push_length_prefix(&mut v, 127);
        push_length_prefix(&mut v, 128);
        push_length_prefix(&mut v, 300);
        assert_eq!(v, [0x7f, 0x80, 0x01, 0xac, 0x02]);
    }
}
