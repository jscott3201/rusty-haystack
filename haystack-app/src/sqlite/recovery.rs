use super::{
    SqliteError,
    storage::{Backend, Stop},
};
use crate::storage_value as value;
use haystack_core::{
    graph::{EntityGraph, GraphImage, GraphState},
    xeto::catalog::ActivatedCatalog,
};
use rusqlite::{Connection, params};
use std::sync::Arc;

const APPLICATION_ID: i64 = 0x48595345;
const SCHEMA: &[(&str, &str)] = &[
    (
        "metadata",
        "CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), format INTEGER NOT NULL CHECK(format=1), dataset BLOB NOT NULL CHECK(length(dataset)=16), incarnation BLOB NOT NULL CHECK(length(incarnation)=16), revision BLOB NOT NULL CHECK(length(revision)=8), catalog_generation BLOB NOT NULL CHECK(length(catalog_generation)=8), floor BLOB NOT NULL CHECK(length(floor)=8), catalog TEXT NOT NULL CHECK(length(CAST(catalog AS BLOB))<=1048576), cursor_key BLOB NOT NULL CHECK(length(cursor_key)=32)) STRICT",
    ),
    (
        "entities",
        "CREATE TABLE entities(id TEXT PRIMARY KEY, payload BLOB NOT NULL CHECK(length(payload)<=1048576)) STRICT, WITHOUT ROWID",
    ),
    (
        "entity_changes",
        "CREATE TABLE entity_changes(revision BLOB PRIMARY KEY CHECK(length(revision)=8), payload BLOB NOT NULL CHECK(length(payload)<=8388608)) STRICT, WITHOUT ROWID",
    ),
    (
        "receipts",
        "CREATE TABLE receipts(binding BLOB PRIMARY KEY CHECK(length(binding)<=1048576), request BLOB NOT NULL CHECK(length(request)<=1048576), outcome BLOB NOT NULL CHECK(length(outcome)<=1048576)) STRICT, WITHOUT ROWID",
    ),
];

pub(crate) fn validate_existing_schema(connection: &Connection) -> Result<bool, SqliteError> {
    let (count, bytes): (i64, i64) = connection.query_row("SELECT count(*),coalesce(sum(length(CAST(type AS BLOB))+length(CAST(name AS BLOB))+length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'", [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|_| SqliteError::Incompatible)?;
    if count > SCHEMA.len() as i64 || bytes > 16384 {
        return Err(SqliteError::Incompatible);
    }
    let objects: Vec<(String, String, String)> = connection.prepare("SELECT type,name,coalesce(sql,'') FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name")
        .map_err(|_| SqliteError::Incompatible)?.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|_| SqliteError::Incompatible)?.collect::<Result<_, _>>().map_err(|_| SqliteError::Incompatible)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| SqliteError::Incompatible)?;
    let application: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|_| SqliteError::Incompatible)?;
    if objects.is_empty() {
        return if version == 0 && application == 0 {
            Ok(false)
        } else {
            Err(SqliteError::Incompatible)
        };
    }
    if version != 1
        || application != APPLICATION_ID
        || objects.len() != SCHEMA.len()
        || objects.iter().any(|(kind, name, sql)| {
            kind != "table"
                || !SCHEMA.iter().any(|(expected_name, expected_sql)| {
                    name == expected_name && sql == expected_sql
                })
        })
    {
        return Err(SqliteError::Incompatible);
    }
    Ok(true)
}
pub(crate) fn schema_version(connection: &Connection) -> Result<i64, SqliteError> {
    connection
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .map_err(|_| SqliteError::Incompatible)
}
pub(crate) fn revision(bytes: Vec<u8>) -> Result<u64, SqliteError> {
    Ok(u64::from_be_bytes(value::fixed(bytes)?))
}

pub(crate) struct Metadata {
    pub dataset: [u8; 16],
    pub state: GraphState,
    pub floor: u64,
    pub catalog: String,
    pub cursor_key: [u8; 32],
}
pub(crate) fn metadata(connection: &Connection) -> Result<Metadata, SqliteError> {
    let valid: bool = connection.query_row("SELECT count(*)=1 AND coalesce(min(singleton=1 AND format=1 AND length(dataset)=16 AND length(incarnation)=16 AND length(revision)=8 AND length(catalog_generation)=8 AND length(floor)=8 AND length(CAST(catalog AS BLOB))<=1048576 AND length(cursor_key)=32),0) FROM metadata", [], |row| row.get(0)).map_err(|_| SqliteError::Incompatible)?;
    if !valid {
        return Err(SqliteError::Incompatible);
    }
    let (dataset, incarnation, rev, catalog_generation, floor, catalog, cursor_key) = connection.query_row(
        "SELECT dataset,incarnation,revision,catalog_generation,floor,catalog,cursor_key FROM metadata WHERE singleton=1", [],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, String>(5)?, row.get::<_, Vec<u8>>(6)?)))
        .map_err(|_| SqliteError::Incompatible)?;
    Ok(Metadata {
        dataset: value::fixed(dataset)?,
        state: GraphState {
            incarnation: value::fixed(incarnation)?,
            revision: revision(rev)?,
            catalog_generation: revision(catalog_generation)?,
        },
        floor: revision(floor)?,
        catalog,
        cursor_key: value::fixed(cursor_key)?,
    })
}
pub(crate) struct StoredReceipt {
    pub request: Vec<u8>,
    pub outcome: Vec<u8>,
}
pub(crate) struct Recovered {
    pub graph: EntityGraph,
    pub dataset: [u8; 16],
    pub cursor_key: [u8; 32],
    pub receipts: Vec<(Vec<u8>, StoredReceipt)>,
    pub schema_version: i64,
}
impl Backend {
    pub(crate) fn initialize_and_recover(
        &self,
        catalog: Arc<ActivatedCatalog>,
        stop: &Stop,
    ) -> Result<Recovered, SqliteError> {
        self.transaction(stop, |connection| {
            if !validate_existing_schema(connection)? {
                for (_, statement) in SCHEMA {
                    connection
                        .execute_batch(statement)
                        .map_err(|_| SqliteError::Incompatible)?;
                }
                connection
                    .pragma_update(None, "user_version", 1)
                    .map_err(|_| SqliteError::Incompatible)?;
                connection
                    .pragma_update(None, "application_id", APPLICATION_ID)
                    .map_err(|_| SqliteError::Incompatible)?;
                let dataset: [u8; 16] = rand::random();
                let incarnation: [u8; 16] = rand::random();
                let cursor: [u8; 32] = rand::random();
                connection
                    .execute(
                        "INSERT INTO metadata VALUES(1,1,?1,?2,?3,?4,?3,?5,?6)",
                        params![
                            dataset.as_slice(),
                            incarnation.as_slice(),
                            0u64.to_be_bytes().as_slice(),
                            1u64.to_be_bytes().as_slice(),
                            catalog.selection_identity(),
                            cursor.as_slice()
                        ],
                    )
                    .map_err(|_| SqliteError::Incompatible)?;
            }
            // Existing empty metadata is corruption, never permission to reset.
            metadata(connection)?;
            Ok(())
        })?;
        self.read(stop, |connection| self.recover(connection, catalog, stop))
    }
    pub(crate) fn recover(
        &self,
        connection: &Connection,
        catalog: Arc<ActivatedCatalog>,
        stop: &Stop,
    ) -> Result<Recovered, SqliteError> {
        stop.check()?;
        if !validate_existing_schema(connection)? {
            return Err(SqliteError::Incompatible);
        }
        let meta = metadata(connection)?;
        if meta.catalog != catalog.selection_identity() {
            return Err(SqliteError::Incompatible);
        }
        let (rows, row_bytes, changes, change_bytes, receipts, receipt_bytes): (i64, i64, i64, i64, i64, i64) = connection.query_row(
            "SELECT (SELECT count(*) FROM entities),(SELECT coalesce(sum(length(CAST(id AS BLOB))+length(payload)),0) FROM entities),(SELECT count(*) FROM entity_changes),(SELECT coalesce(sum(length(payload)),0) FROM entity_changes),(SELECT count(*) FROM receipts),(SELECT coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM receipts)", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .map_err(|_| SqliteError::Incompatible)?;
        let size = |value| usize::try_from(value).map_err(|_| SqliteError::Limit);
        if size(rows)? > self.limits.recovery.max_rows
            || size(changes)? > self.limits.recovery.max_changes
            || size(row_bytes)?.saturating_add(size(change_bytes)?) > self.limits.recovery.max_bytes
            || size(receipts)? > self.limits.max_receipts
            || size(receipt_bytes)? > self.limits.max_receipt_bytes
        {
            return Err(SqliteError::Limit);
        }
        let mut rows = Vec::with_capacity(size(rows)?);
        let mut statement = connection
            .prepare("SELECT id,payload FROM entities ORDER BY id")
            .map_err(|_| SqliteError::Incompatible)?;
        let mut query = statement.query([]).map_err(|_| SqliteError::Incompatible)?;
        while let Some(row) = query.next().map_err(|_| SqliteError::Incompatible)? {
            stop.check()?;
            let id: String = row.get(0).map_err(|_| SqliteError::Incompatible)?;
            let payload: Vec<u8> = row.get(1).map_err(|_| SqliteError::Incompatible)?;
            let value = value::decode_dict(&payload)?;
            stop.check()?;
            if value.id().is_none_or(|reference| reference.val != id) {
                return Err(SqliteError::Incompatible);
            }
            rows.push(value);
        }
        let mut changes = Vec::with_capacity(size(changes)?);
        let mut statement = connection
            .prepare("SELECT revision,payload FROM entity_changes ORDER BY revision")
            .map_err(|_| SqliteError::Incompatible)?;
        let mut query = statement.query([]).map_err(|_| SqliteError::Incompatible)?;
        while let Some(row) = query.next().map_err(|_| SqliteError::Incompatible)? {
            stop.check()?;
            let position = revision(row.get(0).map_err(|_| SqliteError::Incompatible)?)?;
            let payload: Vec<u8> = row.get(1).map_err(|_| SqliteError::Incompatible)?;
            let change = value::decode_change(&payload)?;
            stop.check()?;
            if change.version != position {
                return Err(SqliteError::Incompatible);
            }
            changes.push(change);
        }
        let mut receipts = Vec::with_capacity(size(receipts)?);
        let mut statement = connection
            .prepare("SELECT binding,request,outcome FROM receipts ORDER BY binding")
            .map_err(|_| SqliteError::Incompatible)?;
        let mut query = statement.query([]).map_err(|_| SqliteError::Incompatible)?;
        while let Some(row) = query.next().map_err(|_| SqliteError::Incompatible)? {
            stop.check()?;
            receipts.push((
                row.get(0).map_err(|_| SqliteError::Incompatible)?,
                StoredReceipt {
                    request: row.get(1).map_err(|_| SqliteError::Incompatible)?,
                    outcome: row.get(2).map_err(|_| SqliteError::Incompatible)?,
                },
            ));
        }
        stop.check()?;
        let graph = EntityGraph::restore(
            GraphImage {
                state: meta.state,
                rows,
                floor: meta.floor,
                changes,
            },
            catalog,
            self.limits.recovery,
        )
        .map_err(|_| SqliteError::Incompatible)?;
        stop.check()?;
        Ok(Recovered {
            graph,
            dataset: meta.dataset,
            cursor_key: meta.cursor_key,
            receipts,
            schema_version: schema_version(connection)?,
        })
    }
}
