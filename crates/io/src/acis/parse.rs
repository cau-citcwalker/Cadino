//! A reader for the ACIS data [`super::Body`] writes (tests only): SAT text or SAB binary into
//! records with their pointers resolved to indices, and the checks a body must pass.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum V {
    Ptr(i64),
    Num(f64),
    /// A SAT keyword (`forward`, `I`, …).
    Word(String),
    /// A SAB logical: true is the first SAT word (`reversed`, `double`, `F`, …).
    Bool(bool),
    Str(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Rec {
    pub name: String,
    /// The values after the attribute pointer and id; a SAB vector is three numbers.
    pub v: Vec<V>,
}

#[derive(Debug)]
pub(crate) struct Model {
    pub version: i64,
    pub entities: i64,
    pub recs: Vec<Rec>,
}

/// SAT text.
pub(crate) fn sat(text: &str) -> Model {
    let mut lines = text.lines();
    let head: Vec<i64> = lines.next().unwrap().split_whitespace().map(|t| t.parse().unwrap()).collect();
    lines.next().unwrap();
    lines.next().unwrap();
    let body: String = lines.take_while(|l| !l.starts_with("End-of-")).collect::<Vec<_>>().join(" ");
    let mut recs = Vec::new();
    for r in body.split('#').map(str::trim).filter(|r| !r.is_empty()) {
        let mut toks = Vec::new();
        let mut rest = r;
        while let Some(t) = rest.split_whitespace().next() {
            let at = rest.find(t).unwrap() + t.len();
            rest = &rest[at..];
            if let Some(n) = t.strip_prefix('@') {
                let n: usize = n.parse().unwrap();
                let s = &rest[1..1 + n];
                rest = &rest[1 + n..];
                toks.push(V::Str(s.to_string()));
            } else if let Some(p) = t.strip_prefix('$') {
                toks.push(V::Ptr(p.parse().unwrap()));
            } else if let Ok(f) = t.parse::<f64>() {
                toks.push(V::Num(f));
            } else {
                toks.push(V::Word(t.to_string()));
            }
        }
        let name = match toks.remove(0) {
            V::Word(w) => w,
            other => panic!("record name {other:?}"),
        };
        assert_eq!(toks.remove(0), V::Ptr(-1), "{name}: attribute");
        assert_eq!(toks.remove(0), V::Num(-1.0), "{name}: id");
        recs.push(Rec { name, v: toks });
    }
    Model { version: head[0], entities: head[2], recs }
}

/// SAB binary.
pub(crate) fn sab(b: &[u8]) -> Model {
    let mut p = b"ACIS BinaryFile".len();
    assert!(b.starts_with(b"ACIS BinaryFile"));
    let i32_at = |p: &mut usize| {
        let v = i32::from_le_bytes(b[*p..*p + 4].try_into().unwrap());
        *p += 4;
        i64::from(v)
    };
    let head: Vec<i64> = (0..4).map(|_| i32_at(&mut p)).collect();
    // Product, ACIS version, date; units and tolerances.
    for _ in 0..3 {
        assert_eq!(b[p], 0x07);
        p += 2 + usize::from(b[p + 1]);
    }
    for _ in 0..3 {
        assert_eq!(b[p], 0x06);
        p += 9;
    }
    let f64_at = |p: usize| f64::from_le_bytes(b[p..p + 8].try_into().unwrap());
    let mut recs = Vec::new();
    let mut name = String::new();
    let mut toks: Vec<V> = Vec::new();
    loop {
        let tag = b[p];
        p += 1;
        match tag {
            0x04 | 0x0C => {
                let v = i32_at(&mut p);
                toks.push(if tag == 0x04 { V::Num(v as f64) } else { V::Ptr(v) });
            }
            0x06 => {
                toks.push(V::Num(f64_at(p)));
                p += 8;
            }
            0x07 | 0x0D | 0x0E => {
                let n = usize::from(b[p]);
                let s = String::from_utf8(b[p + 1..p + 1 + n].to_vec()).unwrap();
                p += 1 + n;
                match tag {
                    0x07 => toks.push(V::Str(s)),
                    0x0E => name.push_str(&format!("{s}-")),
                    _ => {
                        name.push_str(&s);
                        if name == "End-of-ASM-data" {
                            assert_eq!(p, b.len(), "nothing after the end marker");
                            break;
                        }
                    }
                }
            }
            0x0A | 0x0B => toks.push(V::Bool(tag == 0x0A)),
            0x13 | 0x14 => {
                toks.extend((0..3).map(|k| V::Num(f64_at(p + 8 * k))));
                p += 24;
            }
            0x11 => {
                assert_eq!(toks.remove(0), V::Ptr(-1), "{name}: attribute");
                assert_eq!(toks.remove(0), V::Num(-1.0), "{name}: id");
                recs.push(Rec { name: std::mem::take(&mut name), v: std::mem::take(&mut toks) });
            }
            t => panic!("unknown SAB tag {t:#x} at {}", p - 1),
        }
    }
    Model { version: head[0], entities: head[2], recs }
}

impl Model {
    pub fn count(&self, name: &str) -> usize {
        self.recs.iter().filter(|r| r.name == name).count()
    }

    /// Pointer `k` of record `i` (its `k`th value), `None` for `$-1`.
    pub fn ptr(&self, i: usize, k: usize) -> Option<usize> {
        match &self.recs[i].v[k] {
            V::Ptr(-1) => None,
            V::Ptr(p) => Some(usize::try_from(*p).unwrap()),
            other => panic!("{} value {k}: {other:?} isn't a pointer", self.recs[i].name),
        }
    }

    fn num(&self, i: usize, k: usize) -> f64 {
        match &self.recs[i].v[k] {
            V::Num(f) => *f,
            other => panic!("{} value {k}: {other:?} isn't a number", self.recs[i].name),
        }
    }

    fn vec(&self, i: usize, k: usize) -> [f64; 3] {
        [self.num(i, k), self.num(i, k + 1), self.num(i, k + 2)]
    }

    /// Whether logical `k` of record `i` is its first word (`reversed`, `double`, …).
    pub fn yes(&self, i: usize, k: usize) -> bool {
        match &self.recs[i].v[k] {
            V::Bool(b) => *b,
            V::Word(w) => matches!(w.as_str(), "reversed" | "double" | "reverse_v" | "F" | "in"),
            other => panic!("{} value {k}: {other:?} isn't a logical", self.recs[i].name),
        }
    }

    fn of(&self, i: usize, name: &str) -> usize {
        assert_eq!(self.recs[i].name, name, "record {i}");
        i
    }

    /// Check the body's links (every pointer to a record of the right type, coedge rings closed,
    /// partners mutual, an edge's coedges running along it either way) and that every vertex lies
    /// on the planes of its faces. Returns the (start, end) points of each face's coedges.
    pub fn check(&self) -> Vec<Vec<([f64; 3], [f64; 3])>> {
        let asm = self.version >= 21800;
        let body = usize::from(asm);
        if asm {
            self.of(0, "asmheader");
        }
        self.of(body, "body");
        let lump = self.of(self.ptr(body, 1).unwrap(), "lump");
        assert_eq!(self.ptr(lump, 3), Some(body));
        let shell = self.of(self.ptr(lump, 2).unwrap(), "shell");
        assert_eq!(self.ptr(shell, 5), Some(lump));
        let (pcurve, vpoint) = if asm { (8, 3) } else { (7, 2) };
        let point = |v: usize| self.vec(self.of(self.ptr(self.of(v, "vertex"), vpoint).unwrap(), "point"), 1);
        let mut faces = Vec::new();
        let mut face = self.ptr(shell, 3);
        let mut seen: HashMap<usize, usize> = HashMap::new();
        while let Some(f) = face {
            self.of(f, "face");
            assert_eq!(self.ptr(f, 3), Some(shell));
            let surf = self.of(self.ptr(f, 5).unwrap(), "plane-surface");
            let (o, n) = (self.vec(surf, 1), self.vec(surf, 4));
            let dist = |p: [f64; 3]| (0..3).map(|k| (p[k] - o[k]) * n[k]).sum::<f64>().abs();
            let mut sides = Vec::new();
            let mut lp = self.ptr(f, 2);
            while let Some(l) = lp {
                self.of(l, "loop");
                assert_eq!(self.ptr(l, 3), Some(f));
                let first = self.ptr(l, 2).unwrap();
                let mut ring = Vec::new();
                let mut c = first;
                loop {
                    self.of(c, "coedge");
                    *seen.entry(c).or_default() += 1;
                    assert_eq!(self.ptr(c, 6), Some(l), "coedge {c}'s loop");
                    assert_eq!(self.ptr(c, pcurve), None);
                    let next = self.ptr(c, 1).unwrap();
                    assert_eq!(self.ptr(next, 2), Some(c), "coedge {c}'s next's prev");
                    if let Some(p) = self.ptr(c, 3) {
                        assert_eq!(self.ptr(p, 3), Some(c), "partners");
                        assert_eq!(self.ptr(p, 4), self.ptr(c, 4), "partners share the edge");
                        assert_ne!(self.yes(p, 5), self.yes(c, 5), "partners run opposite ways");
                    }
                    let e = self.of(self.ptr(c, 4).unwrap(), "edge");
                    let curve = self.of(self.ptr(e, 6).unwrap(), "straight-curve");
                    let (s, t) = (point(self.ptr(e, 1).unwrap()), point(self.ptr(e, 3).unwrap()));
                    let (co, cd) = (self.vec(curve, 1), self.vec(curve, 4));
                    for (p, param) in [(s, self.num(e, 2)), (t, self.num(e, 4))] {
                        for k in 0..3 {
                            assert!((co[k] + cd[k] * param - p[k]).abs() < 1e-9, "edge {e}: its curve at its parameter is its vertex");
                        }
                    }
                    assert!(dist(s) < 1e-6 && dist(t) < 1e-6, "edge {e} off face {f}'s plane");
                    ring.push(if self.yes(c, 5) { (t, s) } else { (s, t) });
                    c = next;
                    if c == first {
                        break;
                    }
                }
                for (k, (_, end)) in ring.iter().enumerate() {
                    assert_eq!(*end, ring[(k + 1) % ring.len()].0, "loop {l}: each coedge ends where the next starts");
                }
                sides.extend(ring);
                lp = self.ptr(l, 1);
            }
            faces.push(sides);
            face = self.ptr(f, 1);
        }
        assert!(seen.values().all(|n| *n == 1), "each coedge in one loop once");
        assert_eq!(seen.len(), self.count("coedge"), "every coedge in a loop");
        faces
    }
}
