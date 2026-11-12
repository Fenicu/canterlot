//! Крошечная база MMDB для тестов гео, собранная здесь же по спецификации MaxMind DB 2.0:
//! дерево IPv6 с 32-битными записями, IPv4 лежит в `::a.b.c.d`. Чужих файлов нет.

use std::net::IpAddr;

use serde_json::Value;

#[derive(Clone, Copy)]
enum Record {
    Empty,
    Node(u32),
    Data(u32),
}

pub struct Mmdb {
    nodes: Vec<[Record; 2]>,
    data: Vec<u8>,
}

impl Default for Mmdb {
    fn default() -> Self {
        Self {
            nodes: vec![[Record::Empty; 2]],
            data: Vec::new(),
        }
    }
}

impl Mmdb {
    /// Запись `record` для сети `a.b.c.d/n` или `x::/n`.
    pub fn insert(mut self, network: &str, record: &Value) -> Self {
        let (ip, prefix) = network.split_once('/').expect("network/prefix");
        let prefix: u32 = prefix.parse().unwrap();
        let (bits, depth) = match ip.parse::<IpAddr>().unwrap() {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), 96 + prefix),
            IpAddr::V6(v6) => (u128::from(v6), prefix),
        };
        let offset = self.data.len() as u32;
        encode(record, &mut self.data);
        let mut node = 0usize;
        for depth_index in 0..depth {
            let bit = ((bits >> (127 - depth_index)) & 1) as usize;
            if depth_index + 1 == depth {
                self.nodes[node][bit] = Record::Data(offset);
                break;
            }
            node = match self.nodes[node][bit] {
                Record::Node(next) => next as usize,
                _ => {
                    let next = self.nodes.len();
                    self.nodes.push([Record::Empty; 2]);
                    self.nodes[node][bit] = Record::Node(next as u32);
                    next
                }
            };
        }
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let count = self.nodes.len() as u32;
        let mut out = Vec::new();
        for node in &self.nodes {
            for record in node {
                let value = match record {
                    Record::Empty => count,
                    Record::Node(next) => *next,
                    Record::Data(offset) => count + 16 + offset,
                };
                out.extend(value.to_be_bytes());
            }
        }
        out.extend([0u8; 16]);
        out.extend(&self.data);
        out.extend(b"\xAB\xCD\xEFMaxMind.com");
        control(&mut out, 7, 9);
        let entries: [(&str, Meta); 9] = [
            ("binary_format_major_version", Meta::U16(2)),
            ("binary_format_minor_version", Meta::U16(0)),
            ("build_epoch", Meta::U64(1_762_905_600)),
            ("database_type", Meta::Text("DBIP-City-Lite")),
            ("description", Meta::Map("en", "canterlot test")),
            ("ip_version", Meta::U16(6)),
            ("languages", Meta::Languages),
            ("node_count", Meta::U32(count)),
            ("record_size", Meta::U16(32)),
        ];
        for (key, value) in entries {
            text(&mut out, key);
            match value {
                Meta::U16(v) => unsigned(&mut out, 5, u64::from(v)),
                Meta::U32(v) => unsigned(&mut out, 6, u64::from(v)),
                Meta::U64(v) => unsigned(&mut out, 9, v),
                Meta::Text(v) => text(&mut out, v),
                Meta::Map(k, v) => {
                    control(&mut out, 7, 1);
                    text(&mut out, k);
                    text(&mut out, v);
                }
                Meta::Languages => {
                    control(&mut out, 11, 2);
                    text(&mut out, "en");
                    text(&mut out, "ru");
                }
            }
        }
        out
    }

    pub fn write(&self, path: &std::path::Path) {
        std::fs::write(path, self.bytes()).unwrap();
    }
}

enum Meta {
    U16(u16),
    U32(u32),
    U64(u64),
    Text(&'static str),
    Map(&'static str, &'static str),
    Languages,
}

fn control(out: &mut Vec<u8>, kind: u8, size: usize) {
    let (short, extra): (u8, Vec<u8>) = if size < 29 {
        (size as u8, vec![])
    } else if size < 285 {
        (29, vec![(size - 29) as u8])
    } else {
        (30, ((size - 285) as u16).to_be_bytes().to_vec())
    };
    if kind <= 7 {
        out.push((kind << 5) | short);
    } else {
        out.push(short);
        out.push(kind - 7);
    }
    out.extend(extra);
}

fn text(out: &mut Vec<u8>, value: &str) {
    control(out, 2, value.len());
    out.extend(value.as_bytes());
}

fn unsigned(out: &mut Vec<u8>, kind: u8, value: u64) {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count();
    control(out, kind, 8 - skip);
    out.extend(&bytes[skip..]);
}

fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::String(value) => text(out, value),
        Value::Number(number) => unsigned(out, 6, number.as_u64().expect("unsigned")),
        Value::Bool(flag) => control(out, 14, usize::from(*flag)),
        Value::Array(items) => {
            control(out, 11, items.len());
            for item in items {
                encode(item, out);
            }
        }
        Value::Object(map) => {
            control(out, 7, map.len());
            for (key, item) in map {
                text(out, key);
                encode(item, out);
            }
        }
        Value::Null => panic!("MMDB has no null"),
    }
}

/// Запись в духе DB-IP City Lite: город, регион, страна.
pub fn city(
    city: &[(&str, &str)],
    region: &[(&str, &str)],
    country: &[(&str, &str)],
    iso: &str,
) -> Value {
    let names = |pairs: &[(&str, &str)]| {
        Value::Object(
            pairs
                .iter()
                .map(|(lang, name)| ((*lang).to_owned(), Value::String((*name).to_owned())))
                .collect(),
        )
    };
    serde_json::json!({
        "city": {"names": names(city)},
        "subdivisions": [{"names": names(region)}],
        "country": {"iso_code": iso, "names": names(country)},
    })
}

/// Две сети: Москва (с русскими названиями) и Берлин (только английские).
pub fn sample() -> Mmdb {
    Mmdb::default()
        .insert(
            "203.0.113.0/24",
            &city(
                &[("en", "Moscow"), ("ru", "Москва")],
                &[("en", "Moscow")],
                &[("en", "Russia"), ("ru", "Россия")],
                "RU",
            ),
        )
        .insert(
            "198.18.0.0/24",
            &city(
                &[("en", "Berlin")],
                &[("en", "Land Berlin")],
                &[("en", "Germany")],
                "DE",
            ),
        )
        .insert(
            "2001:db8:aaaa::/48",
            &city(
                &[("en", "Amsterdam")],
                &[("en", "North Holland")],
                &[("en", "Netherlands")],
                "NL",
            ),
        )
}
