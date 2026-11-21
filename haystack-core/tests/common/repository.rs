#![allow(dead_code)]
use haystack_core::xeto::repository::{LibraryPackage, LibraryPin};
/// Test-only standard stored-ZIP emitter; production never emits or repacks ZIP.
pub fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    fn w16(out: &mut Vec<u8>, x: u16) {
        out.extend(x.to_le_bytes());
    }
    fn w32(out: &mut Vec<u8>, x: u32) {
        out.extend(x.to_le_bytes());
    }
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32fast::hash(data);
        w32(&mut out, 0x04034b50);
        for x in [20, 0, 0, 0, 0] {
            w16(&mut out, x);
        }
        for x in [crc, data.len() as u32, data.len() as u32] {
            w32(&mut out, x);
        }
        w16(&mut out, name.len() as u16);
        w16(&mut out, 0);
        out.extend(name.as_bytes());
        out.extend(*data);
        w32(&mut directory, 0x02014b50);
        for x in [20, 20, 0, 0, 0, 0] {
            w16(&mut directory, x);
        }
        for x in [crc, data.len() as u32, data.len() as u32] {
            w32(&mut directory, x);
        }
        for x in [name.len() as u16, 0, 0, 0, 0] {
            w16(&mut directory, x);
        }
        w32(&mut directory, 0);
        w32(&mut directory, offset);
        directory.extend(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend(&directory);
    w32(&mut out, 0x06054b50);
    for x in [0, 0, entries.len() as u16, entries.len() as u16] {
        w16(&mut out, x);
    }
    w32(&mut out, directory.len() as u32);
    w32(&mut out, start);
    w16(&mut out, 0);
    out
}
pub fn raw(name: &str, version: &str, deps: &[(&str, &str)], body: &str) -> Vec<u8> {
    let dependencies = deps
        .iter()
        .map(|(n, v)| format!("{{lib:\"{n}\",versions:\"{v}\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "pragma: Lib <version:\"{version}\",maturity:\"stable\",depends:{{{dependencies}}}>\n{body}\n"
    );
    let deps = deps
        .iter()
        .map(|(n, v)| format!("{n} {v}"))
        .collect::<Vec<_>>()
        .join(";");
    let props = format!("name={name}\nversion={version}\nmaturity=stable\ndepends={deps}\ndoc=\n");
    zip(&[
        ("xeto-meta.props", props.as_bytes()),
        ("lib.xeto", source.as_bytes()),
    ])
}
pub fn pin(name: &str, version: &str) -> LibraryPin {
    LibraryPin::new(name, version.parse().unwrap()).unwrap()
}
pub fn package(name: &str, version: &str, deps: &[(&str, &str)], body: &str) -> LibraryPackage {
    LibraryPackage::from_bytes(pin(name, version), raw(name, version, deps, body)).unwrap()
}
