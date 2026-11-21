use super::{PostgresError, storage::Opened, value};
use haystack_core::{
    graph::{EntityGraph, GraphImage, GraphState},
    xeto::catalog::ActivatedCatalog,
};
use sqlx::Row;
use std::sync::Arc;

pub(crate) struct Recovered {
    pub(crate) graph: EntityGraph,
    pub(crate) dataset: [u8; 16],
    pub(crate) history_authority: [u8; 16],
    pub(crate) history_sequence: u64,
    pub(crate) cursor_key: [u8; 32],
    pub(crate) generation: i64,
}
impl Opened {
    pub(crate) async fn recover_entities(
        &mut self,
        catalog: Arc<ActivatedCatalog>,
    ) -> Result<Recovered, PostgresError> {
        use sqlx::Connection;
        let mut tx = self
            .connection
            .begin()
            .await
            .map_err(|_| PostgresError::Connection)?;
        let result = async {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metadata)")
            .fetch_one(&mut *tx).await.map_err(crate::postgres::storage::read_error)?;
        if !exists {
            // Initialization is permitted only for an actually empty authority.
            // Missing metadata in a populated schema is corruption, not a new DB.
            let populated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entities UNION ALL SELECT 1 FROM entity_changes UNION ALL SELECT 1 FROM receipts UNION ALL SELECT 1 FROM history_points UNION ALL SELECT 1 FROM history_samples UNION ALL SELECT 1 FROM history_changes)")
                .fetch_one(&mut *tx).await.map_err(crate::postgres::storage::read_error)?;
            if populated { return Err(PostgresError::Incompatible); }
            let dataset: [u8;16] = rand::random();
            let incarnation: [u8;16] = rand::random();
            let history: [u8;16] = rand::random();
            let cursor: [u8;32] = rand::random();
            sqlx::query("INSERT INTO metadata VALUES (1,3,$1,$2,0,1,0,$3,0,1,$4,$5,'active',NULL)")
                .bind(dataset.as_slice()).bind(incarnation.as_slice()).bind(history.as_slice())
                .bind(catalog.selection_identity()).bind(cursor.as_slice()).execute(&mut *tx).await
                .map_err(crate::postgres::storage::read_error)?;
        }
        let active: bool = sqlx::query_scalar("SELECT authority_state='active' FROM metadata WHERE singleton=1").fetch_one(&mut *tx).await.map_err(crate::postgres::storage::read_error)?;
        if !active { return Err(PostgresError::Unavailable); }
        let recovered = read_graph(&mut tx, catalog, self.config.limits.recovery).await?;
        Ok(recovered)
        }.await;
        match result {
            Ok(recovered) => {
                tx.commit().await.map_err(|_| PostgresError::Connection)?;
                Ok(recovered)
            }
            Err(error) => {
                tx.rollback().await.map_err(|_| PostgresError::Connection)?;
                Err(error)
            }
        }
    }
}

pub(crate) async fn read_graph(
    connection: &mut sqlx::PgConnection,
    catalog: Arc<ActivatedCatalog>,
    limits: haystack_core::graph::RecoveryLimits,
) -> Result<Recovered, PostgresError> {
    let meta = sqlx::query("SELECT format, CASE WHEN octet_length(dataset)=16 THEN dataset END AS dataset, CASE WHEN octet_length(incarnation)=16 THEN incarnation END AS incarnation, revision::text, catalog_generation::text, floor::text, CASE WHEN octet_length(history_authority)=16 THEN history_authority END AS history_authority, history_sequence::text, owner_generation, CASE WHEN octet_length(catalog)<=1048576 THEN catalog END AS catalog, CASE WHEN octet_length(cursor_key)=32 THEN cursor_key END AS cursor_key FROM metadata WHERE singleton=1 FOR SHARE")
            .fetch_one(&mut *connection).await.map_err(crate::postgres::storage::read_error)?;
    let get = |name| {
        meta.try_get::<String, _>(name)
            .map_err(|_| PostgresError::Incompatible)
    };
    if meta
        .try_get::<i32, _>("format")
        .map_err(|_| PostgresError::Incompatible)?
        != 3
        || get("catalog")? != catalog.selection_identity()
    {
        return Err(PostgresError::Incompatible);
    }
    let state = GraphState {
        incarnation: value::fixed(
            meta.try_get("incarnation")
                .map_err(|_| PostgresError::Incompatible)?,
        )?,
        revision: value::number(&get("revision")?)?,
        catalog_generation: value::number(&get("catalog_generation")?)?,
    };

    let counts = sqlx::query("SELECT (SELECT count(*) FROM entities) AS rows, (SELECT coalesce(sum(octet_length(payload)+octet_length(id)),0)::bigint FROM entities) AS bytes, (SELECT count(*) FROM entity_changes) AS changes, (SELECT coalesce(sum(octet_length(payload)),0)::bigint FROM entity_changes) AS change_bytes")
            .fetch_one(&mut *connection).await.map_err(crate::postgres::storage::read_error)?;
    let count = |name| {
        counts
            .try_get::<i64, _>(name)
            .map_err(|_| PostgresError::Incompatible)
            .and_then(|n| usize::try_from(n).map_err(|_| PostgresError::Limit))
    };
    if count("rows")? > limits.max_rows
        || count("changes")? > limits.max_changes
        || count("bytes")?.saturating_add(count("change_bytes")?) > limits.max_bytes
    {
        return Err(PostgresError::Limit);
    }
    // Materialization is bounded by the counts/bytes read under the same
    // serialization transaction. No other admitted writer owns this dataset.
    let rows = sqlx::query("SELECT id,payload FROM entities ORDER BY id")
        .fetch_all(&mut *connection)
        .await
        .map_err(crate::postgres::storage::read_error)?;
    let rows = rows
        .into_iter()
        .map(|row| {
            let id: String = row.try_get("id").map_err(|_| PostgresError::Incompatible)?;
            let payload: Vec<u8> = row
                .try_get("payload")
                .map_err(|_| PostgresError::Incompatible)?;
            let value = value::decode_dict(&payload)?;
            if value.id().is_none_or(|r| r.val != id) {
                return Err(PostgresError::Incompatible);
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let changes =
        sqlx::query("SELECT revision::text,payload FROM entity_changes ORDER BY revision")
            .fetch_all(&mut *connection)
            .await
            .map_err(crate::postgres::storage::read_error)?;
    let changes = changes
        .into_iter()
        .map(|row| {
            let revision = value::number(
                &row.try_get::<String, _>("revision")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            let change = value::decode_change(
                &row.try_get::<Vec<u8>, _>("payload")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            if change.version != revision {
                return Err(PostgresError::Incompatible);
            }
            Ok(change)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let image = GraphImage {
        state,
        rows,
        changes,
        floor: value::number(&get("floor")?)?,
    };
    let graph =
        EntityGraph::restore(image, catalog, limits).map_err(|_| PostgresError::Incompatible)?;
    let generation = meta
        .try_get::<i64, _>("owner_generation")
        .map_err(|_| PostgresError::Incompatible)?;
    let recovered = Recovered {
        graph,
        dataset: value::fixed(
            meta.try_get("dataset")
                .map_err(|_| PostgresError::Incompatible)?,
        )?,
        history_authority: value::fixed(
            meta.try_get("history_authority")
                .map_err(|_| PostgresError::Incompatible)?,
        )?,
        history_sequence: value::number(&get("history_sequence")?)?,
        cursor_key: value::fixed(
            meta.try_get("cursor_key")
                .map_err(|_| PostgresError::Incompatible)?,
        )?,
        generation,
    };
    Ok(recovered)
}
