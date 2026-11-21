use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use haystack_core::graph::{EntityGraph, GraphImage, GraphState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_BACKUP_BYTES: usize = 128 * 1024 * 1024;

/// Exact source cut. History completeness means every retained sample and every
/// retained empty series/eviction watermark, never an archive of evicted data.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub schema_format: u32,
    pub typed_payload_format: u32,
    pub operation: String,
    pub source_schema: String,
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
    pub revision: u64,
    pub catalog_generation: u64,
    pub catalog: String,
    pub history_authority: [u8; 16],
    pub history_sequence: u64,
    pub source_epoch: i64,
    pub entities: usize,
    pub history_points: usize,
    pub history_samples: usize,
    pub history_completeness: String,
    pub continuation_policy: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entity {
    pub id: String,
    pub payload: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Point {
    pub id: String,
    pub incarnation: [u8; 16],
    pub generation: u64,
    pub evicted_through: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Sample {
    pub point: String,
    pub seconds: i64,
    pub nanos: u32,
    pub payload: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Body {
    pub manifest: Manifest,
    pub entities: Vec<Entity>,
    pub points: Vec<Point>,
    pub samples: Vec<Sample>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    body: Body,
    sha256: String,
}
/// A fully validated canonical package. Its digest is an integrity check, not a
/// signature or proof that an untrusted producer is authorized to restore it.
pub struct Backup {
    pub(super) body: Body,
    pub(super) digest: String,
    bytes: Vec<u8>,
}
impl Backup {
    pub fn manifest(&self) -> &Manifest {
        &self.body.manifest
    }
    pub fn sha256(&self) -> &str {
        &self.digest
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn parse(
        bytes: &[u8],
        catalog: &Arc<ActivatedCatalog>,
        limits: &PostgresLimits,
    ) -> Result<Self, PostgresError> {
        if bytes.is_empty() || bytes.len() > MAX_BACKUP_BYTES {
            return Err(PostgresError::Limit);
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| PostgresError::Incompatible)?;
        if serde_json::to_vec(&envelope).map_err(|_| PostgresError::Incompatible)? != bytes
            || digest(&envelope.body)? != envelope.sha256
        {
            return Err(PostgresError::Incompatible);
        }
        validate(&envelope.body, catalog, limits)?;
        Ok(Self {
            body: envelope.body,
            digest: envelope.sha256,
            bytes: bytes.to_vec(),
        })
    }
    pub(super) fn create(
        body: Body,
        catalog: &Arc<ActivatedCatalog>,
        limits: &PostgresLimits,
    ) -> Result<Self, PostgresError> {
        validate(&body, catalog, limits)?;
        let sha256 = digest(&body)?;
        let bytes = serde_json::to_vec(&Envelope {
            body: body.clone(),
            sha256: sha256.clone(),
        })
        .map_err(|_| PostgresError::Incompatible)?;
        if bytes.len() > MAX_BACKUP_BYTES {
            return Err(PostgresError::Limit);
        }
        Ok(Self {
            body,
            digest: sha256,
            bytes,
        })
    }
}
fn digest(body: &Body) -> Result<String, PostgresError> {
    let bytes = serde_json::to_vec(body).map_err(|_| PostgresError::Incompatible)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub(super) fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}
pub(super) fn decode(text: &str) -> Result<Vec<u8>, PostgresError> {
    let bytes = STANDARD
        .decode(text)
        .map_err(|_| PostgresError::Incompatible)?;
    if encode(&bytes) != text {
        return Err(PostgresError::Incompatible);
    }
    Ok(bytes)
}
fn validate(
    body: &Body,
    catalog: &Arc<ActivatedCatalog>,
    limits: &PostgresLimits,
) -> Result<(), PostgresError> {
    let m = &body.manifest;
    identifier(&m.source_schema)?;
    operation_id(&m.operation)?;
    if m.format != 1
        || m.schema_format != 3
        || m.typed_payload_format != 1
        || m.catalog != catalog.selection_identity()
        || m.source_epoch <= 0
        || m.dataset == [0; 16]
        || m.incarnation == [0; 16]
        || m.history_authority == [0; 16]
        || m.catalog_generation == 0
        || m.entities != body.entities.len()
        || m.history_points != body.points.len()
        || m.history_samples != body.samples.len()
        || m.history_completeness != "all-retained-bounded-live"
        || m.continuation_policy != "rotate-identities-reset-feeds-and-receipts"
    {
        return Err(PostgresError::Incompatible);
    }
    if m.entities > limits.recovery.max_rows
        || m.history_points > crate::HistoryStoreLimits::default().max_points
        || m.history_samples > limits.max_history_samples
    {
        return Err(PostgresError::Limit);
    }
    let mut previous = None;
    let mut rows = Vec::new();
    let mut total = 0usize;
    for entity in &body.entities {
        if previous.is_some_and(|id: &str| id >= entity.id.as_str()) {
            return Err(PostgresError::Incompatible);
        }
        previous = Some(entity.id.as_str());
        let bytes = decode(&entity.payload)?;
        total = total
            .saturating_add(bytes.len())
            .saturating_add(entity.id.len());
        let row = super::super::value::decode_dict(&bytes)?;
        if row.id().is_none_or(|id| id.val != entity.id) {
            return Err(PostgresError::Incompatible);
        }
        rows.push(row);
    }
    EntityGraph::restore(
        GraphImage {
            state: GraphState {
                incarnation: m.incarnation,
                revision: m.revision,
                catalog_generation: m.catalog_generation,
            },
            rows,
            changes: vec![],
            floor: m.revision,
        },
        catalog.clone(),
        limits.recovery,
    )
    .map_err(|_| PostgresError::Incompatible)?;
    let mut points = BTreeMap::new();
    let mut previous = None;
    for point in &body.points {
        if point.id.is_empty()
            || point.id.len() > 256
            || point.incarnation == [0; 16]
            || previous.is_some_and(|id: &str| id >= point.id.as_str())
        {
            return Err(PostgresError::Incompatible);
        }
        previous = Some(point.id.as_str());
        if let Some(evicted) = &point.evicted_through {
            super::super::history_value::parse_timestamp(evicted)?;
        }
        total = total
            .saturating_add(point.id.len())
            .saturating_add(point.evicted_through.as_ref().map_or(0, String::len));
        points.insert(point.id.as_str(), 0usize);
    }
    let mut previous = None;
    for sample in &body.samples {
        let key = (sample.point.as_str(), sample.seconds, sample.nanos);
        if previous.is_some_and(|old| old >= key) || sample.nanos >= 1_000_000_000 {
            return Err(PostgresError::Incompatible);
        }
        previous = Some(key);
        let n = points
            .get_mut(sample.point.as_str())
            .ok_or(PostgresError::Incompatible)?;
        *n += 1;
        if *n > crate::HistoryStoreLimits::default().max_items_per_point {
            return Err(PostgresError::Limit);
        }
        let bytes = decode(&sample.payload)?;
        total = total
            .saturating_add(bytes.len())
            .saturating_add(sample.point.len());
        let item = super::super::history_value::decode_sample(&bytes)?;
        if item.ts.timestamp() != sample.seconds || item.ts.timestamp_subsec_nanos() != sample.nanos
        {
            return Err(PostgresError::Incompatible);
        }
    }
    if total > limits.recovery.max_bytes {
        return Err(PostgresError::Limit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::{
        data::HDict,
        kinds::{Float, HRef, Kind, Number},
    };
    fn body() -> (Body, Arc<ActivatedCatalog>) {
        let catalog =
            Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("point")));
        row.set("empty", Kind::Null);
        row.set("missing", Kind::None);
        row.set("na", Kind::NA);
        row.set("integer", Kind::Int(i64::MAX));
        row.set("bits", Kind::Float(Float::from_bits(0x7ff8_0000_0000_0042)));
        row.set(
            "unit",
            Kind::Number(Number {
                val: -0.0,
                unit: Some("°F".into()),
            }),
        );
        row.set(
            "ref",
            Kind::Ref(HRef {
                val: "point".into(),
                dis: Some("Original display".into()),
            }),
        );
        let ts =
            chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00.123456789+00:00").unwrap();
        let sample = crate::HisItem {
            ts,
            val: Kind::Number(Number::unitless(1.25)),
        };
        let body = Body {
            manifest: Manifest {
                format: 1,
                schema_format: 3,
                typed_payload_format: 1,
                operation: "archive".into(),
                source_schema: "source".into(),
                dataset: [1; 16],
                incarnation: [2; 16],
                revision: 4,
                catalog_generation: 1,
                catalog: catalog.selection_identity().into(),
                history_authority: [3; 16],
                history_sequence: 9,
                source_epoch: 2,
                entities: 1,
                history_points: 2,
                history_samples: 1,
                history_completeness: "all-retained-bounded-live".into(),
                continuation_policy: "rotate-identities-reset-feeds-and-receipts".into(),
            },
            entities: vec![Entity {
                id: "point".into(),
                payload: encode(&value::encode_dict(&row).unwrap()),
            }],
            points: vec![
                Point {
                    id: "empty".into(),
                    incarnation: [4; 16],
                    generation: 1,
                    evicted_through: Some("2023-01-01T00:00:00.000000000+00:00".into()),
                },
                Point {
                    id: "point".into(),
                    incarnation: [5; 16],
                    generation: 9,
                    evicted_through: None,
                },
            ],
            samples: vec![Sample {
                point: "point".into(),
                seconds: ts.timestamp(),
                nanos: ts.timestamp_subsec_nanos(),
                payload: encode(
                    &super::super::super::history_value::encode_sample(&sample).unwrap(),
                ),
            }],
        };
        (body, catalog)
    }
    #[test]
    fn canonical_archive_keeps_exact_typed_bytes_and_empty_evicted_series() {
        let (body, catalog) = body();
        let original = body.entities[0].payload.clone();
        let archive = Backup::create(body, &catalog, &PostgresLimits::default()).unwrap();
        let parsed = Backup::parse(archive.bytes(), &catalog, &PostgresLimits::default()).unwrap();
        assert_eq!(parsed.body.entities[0].payload, original);
        assert_eq!(
            parsed.body.points[0].evicted_through.as_deref(),
            Some("2023-01-01T00:00:00.000000000+00:00")
        );
        assert_eq!(parsed.sha256(), archive.sha256());
        assert_eq!(parsed.manifest().history_sequence, 9);
    }
    #[test]
    fn rejects_corruption_truncation_noncanonical_and_semantic_mismatch() {
        let (body, catalog) = body();
        let valid = Backup::create(body.clone(), &catalog, &PostgresLimits::default()).unwrap();
        let mut corrupt = valid.bytes().to_vec();
        let last = corrupt.len() - 4;
        corrupt[last] ^= 1;
        assert!(Backup::parse(&corrupt, &catalog, &PostgresLimits::default()).is_err());
        assert!(
            Backup::parse(
                &valid.bytes()[..valid.bytes().len() - 1],
                &catalog,
                &PostgresLimits::default()
            )
            .is_err()
        );
        let mut spaced = valid.bytes().to_vec();
        spaced.push(b' ');
        assert!(Backup::parse(&spaced, &catalog, &PostgresLimits::default()).is_err());
        let mutations: [fn(&mut Body); 9] = [
            |b| b.manifest.schema_format = 99,
            |b| b.manifest.typed_payload_format = 99,
            |b| b.manifest.catalog.push_str("different"),
            |b| b.manifest.entities += 1,
            |b| {
                b.entities.push(b.entities[0].clone());
                b.manifest.entities += 1;
            },
            |b| b.entities[0].id = "mismatched".into(),
            |b| b.samples[0].nanos += 1,
            |b| {
                b.samples.push(b.samples[0].clone());
                b.manifest.history_samples += 1;
            },
            |b| b.entities[0].payload = encode(br#"{"format":1,"value":{"type":"unsupported"}}"#),
        ];
        for change in mutations {
            let mut changed = body.clone();
            change(&mut changed);
            let sha256 = digest(&changed).unwrap();
            let bytes = serde_json::to_vec(&Envelope {
                body: changed,
                sha256,
            })
            .unwrap();
            assert!(Backup::parse(&bytes, &catalog, &PostgresLimits::default()).is_err());
        }
    }
}
