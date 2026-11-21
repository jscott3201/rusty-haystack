use super::*;
use backup::{Body, Entity, Point, Sample};

const CONTROL: &str = "CREATE TABLE selector(singleton smallint PRIMARY KEY CHECK(singleton=1),format integer NOT NULL CHECK(format=1),schema_name text NOT NULL,dataset bytea NOT NULL,incarnation bytea NOT NULL,epoch bigint NOT NULL); CREATE TABLE operations(id text PRIMARY KEY,phase text NOT NULL CHECK(phase IN ('quiesced','captured','staged','committed','cancelled')),source text NOT NULL,target text NOT NULL,digest text,source_incarnation bytea NOT NULL,source_epoch bigint NOT NULL,target_incarnation bytea,target_epoch bigint,kind text NOT NULL CHECK(kind IN ('export','restore')),source_revision text NOT NULL,source_history_sequence text NOT NULL,backup_manifest bytea);";

struct Operation {
    id: String,
    phase: String,
    source: String,
    target: String,
    digest: Option<String>,
    source_incarnation: Vec<u8>,
    source_epoch: i64,
    target_incarnation: Option<Vec<u8>>,
    target_epoch: Option<i64>,
    kind: String,
    source_revision: u64,
    source_history_sequence: u64,
    manifest: Option<Manifest>,
}
async fn operation(
    c: &mut PgConnection,
    control: &str,
    id: &str,
) -> Result<Option<Operation>, PostgresError> {
    operation_id(id)?;
    path(c, control).await?;
    let row=sqlx::query("SELECT id,phase,source,target,digest,source_incarnation,source_epoch,target_incarnation,target_epoch,kind,source_revision,source_history_sequence,CASE WHEN octet_length(backup_manifest)<=1048576 THEN backup_manifest END AS backup_manifest FROM operations WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(c).await.map_err(storage::read_error)?;
    row.map(|r| {
        Ok(Operation {
            id: r.try_get("id").map_err(storage::read_error)?,
            phase: r.try_get("phase").map_err(storage::read_error)?,
            source: r.try_get("source").map_err(storage::read_error)?,
            target: r.try_get("target").map_err(storage::read_error)?,
            digest: r.try_get("digest").map_err(storage::read_error)?,
            source_incarnation: r
                .try_get("source_incarnation")
                .map_err(storage::read_error)?,
            source_epoch: r.try_get("source_epoch").map_err(storage::read_error)?,
            target_incarnation: r
                .try_get("target_incarnation")
                .map_err(storage::read_error)?,
            target_epoch: r.try_get("target_epoch").map_err(storage::read_error)?,
            kind: r.try_get("kind").map_err(storage::read_error)?,
            source_revision: value::number(
                &r.try_get::<String, _>("source_revision")
                    .map_err(storage::read_error)?,
            )?,
            source_history_sequence: value::number(
                &r.try_get::<String, _>("source_history_sequence")
                    .map_err(storage::read_error)?,
            )?,
            manifest: r
                .try_get::<Option<Vec<u8>>, _>("backup_manifest")
                .map_err(storage::read_error)?
                .map(|b| serde_json::from_slice(&b).map_err(|_| PostgresError::Incompatible))
                .transpose()?,
        })
    })
    .transpose()
}
async fn selection(c: &mut PgConnection, control: &str) -> Result<Authority, PostgresError> {
    path(c, control).await?;
    let row = sqlx::query(
        "SELECT schema_name,dataset,incarnation,epoch FROM selector WHERE singleton=1 AND format=1 FOR UPDATE",
    )
    .fetch_one(c)
    .await
    .map_err(storage::read_error)?;
    Ok(Authority {
        schema: row.try_get("schema_name").map_err(storage::read_error)?,
        dataset: value::fixed(row.try_get("dataset").map_err(storage::read_error)?)?,
        incarnation: value::fixed(row.try_get("incarnation").map_err(storage::read_error)?)?,
        epoch: row.try_get("epoch").map_err(storage::read_error)?,
    })
}
async fn status(
    c: &mut PgConnection,
    control: &str,
    id: Option<&str>,
) -> Result<Status, PostgresError> {
    let selected = selection(c, control).await?;
    let op = match id {
        Some(id) => Some(
            operation(c, control, id)
                .await?
                .ok_or(PostgresError::Conflict)?,
        ),
        None => None,
    };
    Ok(Status {
        format: 1,
        selected,
        operation: op.as_ref().map(|o| o.id.clone()),
        phase: op.as_ref().map(|o| o.phase.clone()),
        source: op.as_ref().map(|o| o.source.clone()),
        target: op.as_ref().map(|o| o.target.clone()),
        backup_sha256: op.as_ref().and_then(|o| o.digest.clone()),
        source_revision: op.as_ref().map(|o| o.source_revision),
        source_history_sequence: op.as_ref().map(|o| o.source_history_sequence),
        archive: op.and_then(|o| o.manifest),
    })
}
async fn check_control(c: &mut PgConnection, control: &str) -> Result<(), PostgresError> {
    path(c, control).await?;
    let rows=sqlx::query("SELECT relname,relpersistence::text AS persistence FROM pg_class JOIN pg_namespace ON pg_namespace.oid=relnamespace WHERE nspname=$1 AND relkind IN ('r','p')").bind(control).fetch_all(&mut *c).await.map_err(storage::read_error)?;
    if rows.len() != 2
        || rows.iter().any(|r| {
            !matches!(
                r.try_get::<String, _>("relname").ok().as_deref(),
                Some("selector" | "operations")
            ) || r.try_get::<String, _>("persistence").ok().as_deref() != Some("p")
        })
    {
        return Err(PostgresError::Incompatible);
    };
    let _ = selection(c, control).await?;
    Ok(())
}
async fn metadata(
    c: &mut PgConnection,
    schema: &str,
) -> Result<sqlx::postgres::PgRow, PostgresError> {
    path(c, schema).await?;
    sqlx::query("SELECT dataset,incarnation,owner_generation,authority_state,operation,revision::text,catalog_generation::text,catalog,history_authority,history_sequence::text FROM metadata WHERE singleton=1 AND format=3 FOR UPDATE").fetch_one(c).await.map_err(storage::read_error)
}
fn field<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<T, PostgresError> {
    row.try_get(name).map_err(storage::read_error)
}
fn pin(row: &sqlx::postgres::PgRow, schema: &str) -> Result<Authority, PostgresError> {
    Ok(Authority {
        schema: schema.into(),
        dataset: value::fixed(field(row, "dataset")?)?,
        incarnation: value::fixed(field(row, "incarnation")?)?,
        epoch: field(row, "owner_generation")?,
    })
}
async fn source_fence(
    c: &mut PgConnection,
    o: &Operation,
) -> Result<sqlx::postgres::PgRow, PostgresError> {
    let r = metadata(c, &o.source).await?;
    if field::<String>(&r, "authority_state")? != "quiesced"
        || field::<Option<String>>(&r, "operation")?.as_deref() != Some(&o.id)
        || field::<Vec<u8>>(&r, "incarnation")? != o.source_incarnation
        || field::<i64>(&r, "owner_generation")? != o.source_epoch
        || value::number(&field::<String>(&r, "revision")?)? != o.source_revision
        || value::number(&field::<String>(&r, "history_sequence")?)? != o.source_history_sequence
    {
        return Err(PostgresError::Conflict);
    };
    Ok(r)
}
async fn target_fence(
    c: &mut PgConnection,
    o: &Operation,
) -> Result<sqlx::postgres::PgRow, PostgresError> {
    let r = metadata(c, &o.target).await?;
    if field::<String>(&r, "authority_state")? != "staged"
        || field::<Option<String>>(&r, "operation")?.as_deref() != Some(&o.id)
        || Some(field::<Vec<u8>>(&r, "incarnation")?) != o.target_incarnation
        || Some(field::<i64>(&r, "owner_generation")?) != o.target_epoch
    {
        return Err(PostgresError::Conflict);
    };
    Ok(r)
}
pub(super) async fn execute(
    c: &mut PgConnection,
    config: &PostgresConfig,
    catalog: &Arc<ActivatedCatalog>,
    command: Command,
    backup: Option<Backup>,
) -> Result<Outcome, PostgresError> {
    storage::check_durability(c).await?;
    let control = &config.schema;
    if let Command::Status { operation } = command {
        gates(c, &[control]).await?;
        check_control(c, control).await?;
        return Ok(Outcome::Status(
            status(c, control, operation.as_deref()).await?,
        ));
    }
    let (id, source, target) = match &command {
        Command::Export {
            operation,
            source,
            target,
        } => (operation.clone(), source.clone(), target.clone()),
        Command::Restore {
            operation: id,
            target,
            ..
        } => {
            let existing = operation(c, control, id).await?;
            let source = match existing {
                Some(o) => o.source,
                None => selection(c, control).await?.schema,
            };
            (id.clone(), source, target.clone())
        }
        Command::Import { operation: id, .. }
        | Command::Activate { operation: id }
        | Command::Cancel { operation: id } => {
            let o = operation(c, control, id)
                .await?
                .ok_or(PostgresError::Conflict)?;
            (id.clone(), o.source, o.target)
        }
        Command::Status { .. } => unreachable!(),
    };
    operation_id(&id)?;
    if source == target || source == *control || target == *control {
        return Err(PostgresError::Configuration);
    };
    gates(c, &[control, &source, &target]).await?;
    if matches!(command, Command::Export { .. } | Command::Restore { .. }) {
        prepare(c, control, &id, &source, &target, backup.as_ref()).await?;
    } else {
        check_control(c, control).await?;
    }
    let o = operation(c, control, &id)
        .await?
        .ok_or(PostgresError::Conflict)?;
    if o.source != source || o.target != target {
        return Err(PostgresError::Conflict);
    };
    match command {
        Command::Export { .. } => {
            if !matches!(o.phase.as_str(), "quiesced" | "captured") {
                return Err(PostgresError::Conflict);
            };
            let backup = capture(c, &o, catalog, &config.limits).await?;
            if o.digest.as_ref().is_some_and(|d| d != backup.sha256()) {
                return Err(PostgresError::Conflict);
            };
            begin(c).await?;
            path(c, control).await?;
            sqlx::query(
                "UPDATE operations SET phase='captured',digest=$1,backup_manifest=$3 WHERE id=$2",
            )
            .bind(backup.sha256())
            .bind(&id)
            .bind(serde_json::to_vec(backup.manifest()).map_err(|_| PostgresError::Incompatible)?)
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
            commit(c).await?;
            Ok(Outcome::Backup(backup))
        }
        Command::Import { .. } | Command::Restore { .. } => {
            let backup = backup.ok_or(PostgresError::Configuration)?;
            stage(c, control, &o, &backup, catalog, &config.limits).await?;
            Ok(Outcome::Status(status(c, control, Some(&id)).await?))
        }
        Command::Activate { .. } => {
            activate(c, control, &o).await?;
            Ok(Outcome::Status(status(c, control, Some(&id)).await?))
        }
        Command::Cancel { .. } => {
            cancel(c, control, &o).await?;
            Ok(Outcome::Status(status(c, control, Some(&id)).await?))
        }
        Command::Status { .. } => unreachable!(),
    }
}
async fn prepare(
    c: &mut PgConnection,
    control: &str,
    id: &str,
    source: &str,
    target: &str,
    archive: Option<&Backup>,
) -> Result<(), PostgresError> {
    let has_control = exists(c, control).await?;
    if has_control {
        check_control(c, control).await?;
        if let Some(o) = operation(c, control, id).await? {
            if o.kind
                != if archive.is_some() {
                    "restore"
                } else {
                    "export"
                }
                || o.source != source
                || o.target != target
                || archive.is_some_and(|b| o.digest.as_deref() != Some(b.sha256()))
            {
                return Err(PostgresError::Conflict);
            };
            return Ok(());
        }
    }
    if !exists(c, source).await? || exists(c, target).await? {
        return Err(PostgresError::Conflict);
    };
    begin(c).await?;
    path(c, source).await?;
    storage::initialize_schema(c, source).await?;
    let r = metadata(c, source).await?;
    let before = pin(&r, source)?;
    if field::<String>(&r, "authority_state")? != "active" {
        return Err(PostgresError::Conflict);
    };
    if let Some(b) = archive
        && (b.manifest().dataset != before.dataset
            || b.manifest().catalog != field::<String>(&r, "catalog")?)
    {
        return Err(PostgresError::Incompatible);
    }
    if has_control {
        if selection(c, control).await? != before {
            return Err(PostgresError::Conflict);
        };
    } else {
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA \"{control}\"")))
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
        path(c, control).await?;
        sqlx::raw_sql(CONTROL)
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
        sqlx::query("INSERT INTO selector VALUES(1,1,$1,$2,$3,$4)")
            .bind(source)
            .bind(before.dataset.as_slice())
            .bind(before.incarnation.as_slice())
            .bind(before.epoch)
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
    }
    let epoch = before.epoch.checked_add(1).ok_or(PostgresError::Limit)?;
    path(c, source).await?;
    sqlx::query("UPDATE metadata SET authority_state='quiesced',operation=$1,owner_generation=$2 WHERE singleton=1").bind(id).bind(epoch).execute(&mut *c).await.map_err(storage::read_error)?;
    path(c, control).await?;
    sqlx::query("UPDATE selector SET epoch=$1 WHERE singleton=1")
        .bind(epoch)
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    sqlx::query("INSERT INTO operations(id,phase,source,target,digest,source_incarnation,source_epoch,target_incarnation,target_epoch,kind,source_revision,source_history_sequence,backup_manifest) VALUES($1,$2,$3,$4,$5,$6,$7,NULL,NULL,$8,$9,$10,$11)")
        .bind(id).bind(if archive.is_some(){"captured"}else{"quiesced"}).bind(source).bind(target).bind(archive.map(Backup::sha256)).bind(before.incarnation.as_slice()).bind(epoch).bind(if archive.is_some(){"restore"}else{"export"}).bind(field::<String>(&r,"revision")?).bind(field::<String>(&r,"history_sequence")?).bind(archive.map(|b|serde_json::to_vec(b.manifest()).map_err(|_|PostgresError::Incompatible)).transpose()?).execute(&mut *c).await.map_err(storage::read_error)?;
    commit(c).await?;
    #[cfg(test)]
    interrupt(c, id, Interruption::AfterQuiesce).await?;
    Ok(())
}
async fn capture(
    c: &mut PgConnection,
    o: &Operation,
    catalog: &Arc<ActivatedCatalog>,
    limits: &PostgresLimits,
) -> Result<Backup, PostgresError> {
    begin(c).await?;
    let r = source_fence(c, o).await?;
    // Validate the ordinary graph contract in the exact same quiesced cut.
    let _ = super::super::recovery::read_graph(c, catalog.clone(), limits.recovery).await?;
    let counts=sqlx::query("SELECT (SELECT count(*) FROM entities) AS entities,(SELECT count(*) FROM history_points) AS points,(SELECT count(*) FROM history_samples) AS samples,(SELECT coalesce(sum(octet_length(id)+octet_length(payload)),0) FROM entities)+(SELECT coalesce(sum(octet_length(id)+coalesce(octet_length(evicted),0)),0) FROM history_points)+(SELECT coalesce(sum(octet_length(point)+octet_length(payload)),0) FROM history_samples) AS bytes").fetch_one(&mut *c).await.map_err(storage::read_error)?;
    let count = |name| {
        field::<i64>(&counts, name)
            .and_then(|n| usize::try_from(n).map_err(|_| PostgresError::Limit))
    };
    // Numeric aggregate is projected as text to avoid driver-specific decimal conversions.
    let byte_count:String=sqlx::query_scalar("SELECT ((SELECT coalesce(sum(octet_length(id)+octet_length(payload)),0) FROM entities)+(SELECT coalesce(sum(octet_length(id)+coalesce(octet_length(evicted),0)),0) FROM history_points)+(SELECT coalesce(sum(octet_length(point)+octet_length(payload)),0) FROM history_samples))::text").fetch_one(&mut *c).await.map_err(storage::read_error)?;
    if count("entities")? > limits.recovery.max_rows
        || count("points")? > crate::HistoryStoreLimits::default().max_points
        || count("samples")? > limits.max_history_samples
        || value::number(&byte_count)? > limits.recovery.max_bytes as u64
    {
        return Err(PostgresError::Limit);
    };
    let entities = sqlx::query("SELECT id,payload FROM entities ORDER BY id COLLATE \"C\"")
        .fetch_all(&mut *c)
        .await
        .map_err(storage::read_error)?
        .iter()
        .map(|r| {
            Ok(Entity {
                id: field(r, "id")?,
                payload: backup::encode(&field::<Vec<u8>>(r, "payload")?),
            })
        })
        .collect::<Result<Vec<_>, PostgresError>>()?;
    let points=sqlx::query("SELECT id,incarnation,generation::text,evicted FROM history_points ORDER BY id COLLATE \"C\"").fetch_all(&mut *c).await.map_err(storage::read_error)?.iter().map(|r|Ok(Point{id:field(r,"id")?,incarnation:value::fixed(field(r,"incarnation")?)?,generation:value::number(&field::<String>(r,"generation")?)?,evicted_through:field::<Option<Vec<u8>>>(r,"evicted")?.map(String::from_utf8).transpose().map_err(|_|PostgresError::Incompatible)?})).collect::<Result<Vec<_>,PostgresError>>()?;
    let samples=sqlx::query("SELECT point,seconds,nanos,payload FROM history_samples ORDER BY point COLLATE \"C\",seconds,nanos").fetch_all(&mut *c).await.map_err(storage::read_error)?.iter().map(|r|Ok(Sample{point:field(r,"point")?,seconds:field(r,"seconds")?,nanos:u32::try_from(field::<i32>(r,"nanos")?).map_err(|_|PostgresError::Incompatible)?,payload:backup::encode(&field::<Vec<u8>>(r,"payload")?)})).collect::<Result<Vec<_>,PostgresError>>()?;
    let manifest = Manifest {
        format: 1,
        schema_format: 3,
        typed_payload_format: 1,
        operation: o.id.clone(),
        source_schema: o.source.clone(),
        dataset: value::fixed(field(&r, "dataset")?)?,
        incarnation: value::fixed(field(&r, "incarnation")?)?,
        revision: value::number(&field::<String>(&r, "revision")?)?,
        catalog_generation: value::number(&field::<String>(&r, "catalog_generation")?)?,
        catalog: field(&r, "catalog")?,
        history_authority: value::fixed(field(&r, "history_authority")?)?,
        history_sequence: value::number(&field::<String>(&r, "history_sequence")?)?,
        source_epoch: field(&r, "owner_generation")?,
        entities: entities.len(),
        history_points: points.len(),
        history_samples: samples.len(),
        history_completeness: "all-retained-bounded-live".into(),
        continuation_policy: "rotate-identities-reset-feeds-and-receipts".into(),
    };
    let backup = Backup::create(
        Body {
            manifest,
            entities,
            points,
            samples,
        },
        catalog,
        limits,
    )?;
    commit(c).await?;
    Ok(backup)
}
async fn stage(
    c: &mut PgConnection,
    control: &str,
    o: &Operation,
    b: &Backup,
    catalog: &Arc<ActivatedCatalog>,
    limits: &PostgresLimits,
) -> Result<(), PostgresError> {
    if o.digest.as_deref() != Some(b.sha256()) {
        return Err(PostgresError::Conflict);
    };
    if o.phase == "committed" {
        // The journal retains this outcome even after later maintenance has
        // changed the source, target or selection. Digest matching still applies.
        return Ok(());
    }
    if o.phase == "staged" {
        begin(c).await?;
        source_fence(c, o).await?;
        target_fence(c, o).await?;
        return commit(c).await;
    }
    if o.phase != "captured" || exists(c, &o.target).await? {
        return Err(PostgresError::Conflict);
    };
    begin(c).await?;
    let source = source_fence(c, o).await?;
    let m = b.manifest();
    if m.dataset != value::fixed::<16>(field(&source, "dataset")?)?
        || m.catalog != field::<String>(&source, "catalog")?
    {
        return Err(PostgresError::Incompatible);
    };
    path(c, &o.target).await?;
    storage::initialize_schema(c, &o.target).await?;
    let incarnation: [u8; 16] = rand::random();
    let history: [u8; 16] = rand::random();
    let cursor: [u8; 32] = rand::random();
    let epoch = o.source_epoch.checked_add(1).ok_or(PostgresError::Limit)?;
    sqlx::query("INSERT INTO metadata VALUES(1,3,$1,$2,$3::text::numeric,$4::text::numeric,$3::text::numeric,$5,0,$9,$6,$7,'staged',$8)").bind(m.dataset.as_slice()).bind(incarnation.as_slice()).bind(m.revision.to_string()).bind(m.catalog_generation.to_string()).bind(history.as_slice()).bind(&m.catalog).bind(cursor.as_slice()).bind(&o.id).bind(epoch).execute(&mut *c).await.map_err(storage::read_error)?;
    for row in &b.body.entities {
        sqlx::query("INSERT INTO entities VALUES($1,$2)")
            .bind(&row.id)
            .bind(backup::decode(&row.payload)?)
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
    }
    for point in &b.body.points {
        let incarnation: [u8; 16] = rand::random();
        sqlx::query("INSERT INTO history_points VALUES($1,$2,$3::text::numeric,$4)")
            .bind(&point.id)
            .bind(incarnation.as_slice())
            .bind(point.generation.to_string())
            .bind(point.evicted_through.as_ref().map(|s| s.as_bytes()))
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
    }
    for sample in &b.body.samples {
        sqlx::query("INSERT INTO history_samples VALUES($1,$2,$3,$4)")
            .bind(&sample.point)
            .bind(sample.seconds)
            .bind(sample.nanos as i32)
            .bind(backup::decode(&sample.payload)?)
            .execute(&mut *c)
            .await
            .map_err(storage::read_error)?;
    }
    #[cfg(test)]
    interrupt(c, &o.id, Interruption::DuringImport).await?;
    let _ = super::super::recovery::read_graph(c, catalog.clone(), limits.recovery).await?;
    path(c, control).await?;
    sqlx::query(
        "UPDATE operations SET phase='staged',target_incarnation=$1,target_epoch=$3 WHERE id=$2",
    )
    .bind(incarnation.as_slice())
    .bind(&o.id)
    .bind(epoch)
    .execute(&mut *c)
    .await
    .map_err(storage::read_error)?;
    commit(c).await
}
async fn activate(c: &mut PgConnection, control: &str, o: &Operation) -> Result<(), PostgresError> {
    if o.phase == "committed" {
        // First activation records the outcome atomically with selection. A
        // terminal replay reports that history without reselecting old authority.
        return Ok(());
    }
    if o.phase != "staged" {
        return Err(PostgresError::Conflict);
    };
    begin(c).await?;
    let source = source_fence(c, o).await?;
    if selection(c, control).await? != pin(&source, &o.source)? {
        return Err(PostgresError::Conflict);
    };
    let target = target_fence(c, o).await?;
    #[cfg(test)]
    interrupt(c, &o.id, Interruption::BeforeSelection).await?;
    let epoch = o.source_epoch.checked_add(1).ok_or(PostgresError::Limit)?;
    path(c, &o.source).await?;
    sqlx::query(
        "UPDATE metadata SET authority_state='retired',owner_generation=$1 WHERE singleton=1",
    )
    .bind(epoch)
    .execute(&mut *c)
    .await
    .map_err(storage::read_error)?;
    path(c, &o.target).await?;
    sqlx::query("UPDATE metadata SET authority_state='active' WHERE singleton=1")
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    path(c, control).await?;
    sqlx::query(
        "UPDATE selector SET schema_name=$1,dataset=$2,incarnation=$3,epoch=$4 WHERE singleton=1",
    )
    .bind(&o.target)
    .bind(field::<Vec<u8>>(&target, "dataset")?)
    .bind(field::<Vec<u8>>(&target, "incarnation")?)
    .bind(field::<i64>(&target, "owner_generation")?)
    .execute(&mut *c)
    .await
    .map_err(storage::read_error)?;
    sqlx::query("UPDATE operations SET phase='committed' WHERE id=$1")
        .bind(&o.id)
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    commit(c).await?;
    #[cfg(test)]
    interrupt(c, &o.id, Interruption::AfterSelectionCommit).await?;
    Ok(())
}
async fn cancel(c: &mut PgConnection, control: &str, o: &Operation) -> Result<(), PostgresError> {
    if o.phase == "cancelled" {
        return Ok(());
    };
    if o.phase == "committed" {
        return Err(PostgresError::Conflict);
    };
    begin(c).await?;
    let source = source_fence(c, o).await?;
    if selection(c, control).await? != pin(&source, &o.source)? {
        return Err(PostgresError::Conflict);
    };
    if o.phase == "staged" {
        let target = target_fence(c, o).await?;
        let epoch = field::<i64>(&target, "owner_generation")?
            .checked_add(1)
            .ok_or(PostgresError::Limit)?;
        sqlx::query(
            "UPDATE metadata SET authority_state='retired',owner_generation=$1 WHERE singleton=1",
        )
        .bind(epoch)
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    } else if exists(c, &o.target).await? {
        return Err(PostgresError::Conflict);
    };
    let epoch = o.source_epoch.checked_add(1).ok_or(PostgresError::Limit)?;
    path(c, &o.source).await?;
    sqlx::query("UPDATE metadata SET authority_state='active',operation=NULL,owner_generation=$1 WHERE singleton=1").bind(epoch).execute(&mut *c).await.map_err(storage::read_error)?;
    path(c, control).await?;
    sqlx::query("UPDATE selector SET epoch=$1 WHERE singleton=1")
        .bind(epoch)
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    sqlx::query("UPDATE operations SET phase='cancelled' WHERE id=$1")
        .bind(&o.id)
        .execute(&mut *c)
        .await
        .map_err(storage::read_error)?;
    commit(c).await
}
