//! Bundled standard Xeto library sources.

/// A bundled Xeto library with name, source text, and dependencies.
pub struct BundledLib {
    pub name: &'static str,
    pub source: &'static str,
    pub depends: &'static [&'static str],
}

/// Return all bundled Xeto libraries in dependency order.
///
/// Libraries are ordered so that a library's dependencies appear before it.
/// This allows sequential loading without missing dependency errors.
pub fn bundled_libs() -> Vec<BundledLib> {
    vec![
        BundledLib {
            name: "sys",
            source: include_str!("../../data/xeto/sys.xeto"),
            depends: &[],
        },
        BundledLib {
            name: "sys.api",
            source: include_str!("../../data/xeto/sys.api.xeto"),
            depends: &["sys"],
        },
        BundledLib {
            name: "sys.comp",
            source: include_str!("../../data/xeto/sys.comp.xeto"),
            depends: &["sys"],
        },
        BundledLib {
            name: "sys.files",
            source: include_str!("../../data/xeto/sys.files.xeto"),
            depends: &["sys"],
        },
        BundledLib {
            name: "sys.template",
            source: include_str!("../../data/xeto/sys.template.xeto"),
            depends: &["sys"],
        },
        BundledLib {
            name: "ph",
            source: include_str!("../../data/xeto/ph.xeto"),
            depends: &["sys"],
        },
        BundledLib {
            name: "ph.attrs",
            source: include_str!("../../data/xeto/ph.attrs.xeto"),
            depends: &["ph"],
        },
        BundledLib {
            name: "ph.equips",
            source: include_str!("../../data/xeto/ph.equips.xeto"),
            depends: &["ph"],
        },
        BundledLib {
            name: "ph.examples",
            source: include_str!("../../data/xeto/ph.examples.xeto"),
            depends: &["ph"],
        },
        BundledLib {
            name: "ph.points",
            source: include_str!("../../data/xeto/ph.points.xeto"),
            depends: &["ph"],
        },
        BundledLib {
            name: "ph.points.elec",
            source: include_str!("../../data/xeto/ph.points.elec.xeto"),
            depends: &["ph", "ph.points"],
        },
        BundledLib {
            name: "ph.protocols",
            source: include_str!("../../data/xeto/ph.protocols.xeto"),
            depends: &["ph"],
        },
    ]
}
