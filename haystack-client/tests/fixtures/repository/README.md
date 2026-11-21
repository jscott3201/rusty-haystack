# Project-owned repository fixture

`downloaded-1.0.0.xetolib` is a deterministic ZIP with two stored entries: `lib.xeto` and `xeto-meta.props`. Its exact-byte digest is `sha256:Qm3aICrF2MHDjamk30dmuQCif1quynjfBEPGq_zMP80`. It contains no upstream source. The integration test extracts only the known bounded stored source entry, then calls explicit admission and activation. This test helper is not a general archive importer.
