//! Bounded compatibility and wire adapters. All profiles use the same registry.
use super::*;
use crate::H4Codec;
use haystack_core::{
    codecs::codec_for,
    data::{HCol, HGrid},
    kinds::HRef,
};

#[derive(Debug, Clone)]
pub enum LegacySubscriptionRequest {
    Subscribe {
        watch: Option<String>,
        ids: Vec<String>,
    },
    Poll {
        watch: String,
    },
    Unsubscribe {
        watch: String,
        ids: Vec<String>,
    },
}
/// Owned HTTP adaptation parameters; session authority is supplied separately.
pub struct SubscriptionWireRequest {
    pub operation: &'static str,
    pub body: Vec<u8>,
    pub input: H4Codec,
    pub output: H4Codec,
    pub legacy_allowed: bool,
}

impl StateSubscriptionService {
    /// Disconnect owns only attachments created by this connection. HTTP watches
    /// and attachments of other connections remain with the application.
    pub fn detach_legacy_connection(&self, connection: [u8; 16]) {
        let mut table = self.inner.records.lock();
        let ids: Vec<_> = table
            .entries
            .iter()
            .filter(|(_, entry)| entry.legacy && entry.connection == Some(connection))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(entry) = table.entries.remove(&id) {
                entry.active.store(false, Ordering::Release);
                table
                    .bindings
                    .remove(&(entry.session.id(), entry.identity.key.clone()));
                table.reserved_bytes = table.reserved_bytes.saturating_sub(entry.reservation);
            }
        }
    }
    /// HTTP retains one admission through decode, execution and bounded encoding.
    pub async fn wire_admitted(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        request: SubscriptionWireRequest,
    ) -> Result<Vec<u8>, ReadError> {
        let SubscriptionWireRequest {
            operation,
            body,
            input,
            output,
            legacy_allowed,
        } = request;
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        if body.len() > wire::MAX_WIRE_BYTES {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let owner = self.inner.clone();
        let dispatched = Arc::new(AtomicBool::new(false));
        let worker = dispatched.clone();
        let result = admission
            .run_task(move |principal, budget| {
                budget.charge(BudgetKind::Work, body.len())?;
                budget.charge(
                    BudgetKind::Retained,
                    body.len().saturating_mul(32).saturating_add(8192),
                )?;
                let codec = codec_for(input.mime()).ok_or(ReadError::Unavailable)?;
                let output = codec_for(output.mime()).ok_or(ReadError::Unavailable)?;
                let legacy = if legacy_allowed {
                    let grid = codec
                        .decode_grid(
                            std::str::from_utf8(&body)
                                .map_err(|_| ReadError::InvalidQuery("invalid watch encoding"))?,
                        )
                        .map_err(|_| ReadError::InvalidQuery("invalid watch grid"))?;
                    if grid.cols.iter().any(|col| col.name == "payload")
                        || grid.rows.iter().any(|row| row.has("payload"))
                    {
                        None
                    } else {
                        Some(legacy_request(operation, grid)?)
                    }
                } else {
                    None
                };
                let encoded = if let Some(request) = legacy {
                    let grid = owner.legacy(&principal, &session, request, None, budget)?;
                    output
                        .encode_grid(&grid)
                        .map_err(|_| ReadError::Budget(BudgetKind::Output))?
                        .into_bytes()
                } else {
                    if output.mime_type() == "text/trio" {
                        return Err(ReadError::InvalidQuery("unsupported subscription codec"));
                    }
                    let request: SubscriptionRequest = wire::decode_grid(&body, codec)
                        .map_err(|_| ReadError::InvalidQuery("invalid subscription envelope"))?;
                    if request.operation() != operation {
                        return Err(ReadError::InvalidQuery("subscription operation mismatch"));
                    }
                    worker.store(true, Ordering::Release);
                    let outcome =
                        owner.execute(&principal, &session, request, false, None, budget)?;
                    encode_outcome(&outcome, output, budget)?
                };
                check_output(budget, encoded.len())?;
                budget.check()?;
                if !session.is_active() {
                    return Err(ReadError::Forbidden);
                }
                Ok(encoded)
            })
            .await;
        match result {
            Ok(body) => Ok(body),
            Err(_) if dispatched.load(Ordering::Acquire) => wire::encode_grid(
                &SubscriptionOutcome::Unknown,
                codec_for(output.mime()).ok_or(ReadError::Unavailable)?,
            )
            .map_err(|_| ReadError::Budget(BudgetKind::Output)),
            Err(error) => Err(error),
        }
    }
    /// WebSocket payload execution happens in the writer, after bounded queueing.
    pub async fn payload_admitted(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        if payload.len() > wire::MAX_PAYLOAD_BYTES {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let owner = self.inner.clone();
        let dispatched = Arc::new(AtomicBool::new(false));
        let worker = dispatched.clone();
        let result = admission
            .run_task(move |principal, budget| {
                budget.charge(BudgetKind::Work, payload.len())?;
                budget.charge(
                    BudgetKind::Retained,
                    payload.len().saturating_mul(32).saturating_add(8192),
                )?;
                let request: SubscriptionRequest = wire::decode(&payload)
                    .map_err(|_| ReadError::InvalidQuery("invalid subscription payload"))?;
                worker.store(true, Ordering::Release);
                let outcome = owner.execute(&principal, &session, request, false, None, budget)?;
                let bytes = outcome
                    .source_bytes()
                    .map_err(|_| ReadError::Budget(BudgetKind::Output))?;
                budget.charge(BudgetKind::Work, bytes.saturating_mul(8))?;
                budget.charge(BudgetKind::Retained, bytes.saturating_mul(16))?;
                let encoded =
                    wire::encode(&outcome).map_err(|_| ReadError::Budget(BudgetKind::Output))?;
                check_output(budget, encoded.len())?;
                budget.check()?;
                if !session.is_active() {
                    return Err(ReadError::Forbidden);
                }
                Ok(encoded)
            })
            .await;
        match result {
            Ok(body) => Ok(body),
            Err(_) if dispatched.load(Ordering::Acquire) => {
                wire::encode(&SubscriptionOutcome::Unknown)
                    .map_err(|_| ReadError::Budget(BudgetKind::Output))
            }
            Err(error) => Err(error),
        }
    }
    pub async fn legacy_admitted(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        request: LegacySubscriptionRequest,
        connection: Option<[u8; 16]>,
    ) -> Result<HGrid, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let owner = self.inner.clone();
        admission
            .run_task(move |principal, budget| {
                owner.legacy(&principal, &session, request, connection, budget)
            })
            .await
    }
    /// Compatibility pushes are fresh, sanitized snapshots for this connection.
    /// They never advance the explicit polling fence and retain no encode cache.
    pub async fn legacy_push_admitted(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        connection: [u8; 16],
    ) -> Result<Vec<(String, HGrid)>, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let owner = self.inner.clone();
        admission
            .run_task(move |principal, budget| {
                if !session.is_active() || !session.matches(&principal) {
                    return Err(ReadError::Forbidden);
                }
                let entries: Vec<_> = owner
                    .registry(budget)?
                    .entries
                    .values()
                    .filter(|entry| {
                        entry.legacy
                            && entry.connection == Some(connection)
                            && entry.active.load(Ordering::Acquire)
                            && entry.session.matches(&principal)
                    })
                    .cloned()
                    .collect();
                budget.charge(BudgetKind::Retained, entries.len().saturating_mul(256))?;
                let mut pushes = Vec::new();
                let mut output_bytes = 0usize;
                for entry in entries {
                    if !entry.session.is_active() {
                        continue;
                    }
                    let record = SubscriptionInner::record(&entry, budget)?;
                    let Status::Live(live) = record.status else {
                        continue;
                    };
                    let capture = owner
                        .capture(&principal, &live.ids, Some(&live), budget)
                        .map_err(|_| ReadError::Forbidden)?;
                    let rows = capture
                        .projection
                        .values()
                        .map(|row| (*row.row).clone())
                        .collect();
                    let grid = grid(rows, Some(legacy_id(&entry.identity)));
                    let encoded = codec_for("text/zinc")
                        .expect("Zinc codec")
                        .encode_grid(&grid)
                        .map_err(|_| ReadError::Budget(BudgetKind::Output))?;
                    output_bytes = output_bytes.saturating_add(encoded.len());
                    check_output(budget, output_bytes)?;
                    pushes.push((legacy_id(&entry.identity), grid));
                }
                budget.check()?;
                Ok(pushes)
            })
            .await
    }
}
fn check_output(budget: &mut Budget, bytes: usize) -> Result<(), ReadError> {
    if bytes > budget.limits.max_output_bytes {
        return Err(ReadError::Budget(BudgetKind::Output));
    }
    budget.charge(BudgetKind::Retained, bytes.saturating_mul(2))
}
fn encode_outcome(
    outcome: &SubscriptionOutcome,
    output: &dyn haystack_core::codecs::Codec,
    budget: &mut Budget,
) -> Result<Vec<u8>, ReadError> {
    let bytes = outcome
        .source_bytes()
        .map_err(|_| ReadError::Budget(BudgetKind::Output))?;
    budget.charge(BudgetKind::Work, bytes.saturating_mul(16))?;
    budget.charge(BudgetKind::Retained, bytes.saturating_mul(32))?;
    wire::encode_grid(outcome, output).map_err(|_| ReadError::Budget(BudgetKind::Output))
}
fn legacy_request(operation: &str, request: HGrid) -> Result<LegacySubscriptionRequest, ReadError> {
    let watch = request.meta.get("watchId").and_then(|value| {
        if let Kind::Str(value) = value {
            Some(value.clone())
        } else {
            None
        }
    });
    if request.rows.len() > wire::MAX_IDS {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    let ids = request
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Kind::Ref(id)) => Ok(id.val.clone()),
            _ => Err(ReadError::InvalidQuery("watch requires Ref id")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    match operation {
        "watchSub" => Ok(LegacySubscriptionRequest::Subscribe { watch, ids }),
        "watchPoll" if ids.is_empty() => Ok(LegacySubscriptionRequest::Poll {
            watch: watch.ok_or(ReadError::InvalidQuery("missing watchId"))?,
        }),
        "watchUnsub" => Ok(LegacySubscriptionRequest::Unsubscribe {
            watch: watch.ok_or(ReadError::InvalidQuery("missing watchId"))?,
            ids,
        }),
        _ => Err(ReadError::InvalidQuery("unsupported watch operation")),
    }
}
fn legacy_id(watch: &SubscriptionId) -> String {
    watch.watch.iter().map(|v| format!("{v:02x}")).collect()
}
fn grid(rows: Vec<HDict>, watch: Option<String>) -> HGrid {
    let names: std::collections::BTreeSet<_> = rows
        .iter()
        .flat_map(|row| row.tag_names().map(str::to_owned))
        .collect();
    let mut meta = HDict::new();
    if let Some(watch) = watch {
        meta.set("watchId", Kind::Str(watch));
    }
    HGrid::from_parts(meta, names.into_iter().map(HCol::new).collect(), rows)
}
impl SubscriptionInner {
    fn legacy_entry(
        &self,
        watch: &str,
        principal: &Principal,
        budget: &Budget,
    ) -> Result<Arc<Entry>, ReadError> {
        if watch.len() != 32 {
            return Err(ReadError::InvalidQuery("unknown watch"));
        }
        if !watch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ReadError::InvalidQuery("unknown watch"));
        }
        let mut id = [0u8; 16];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&watch[index * 2..index * 2 + 2], 16)
                .map_err(|_| ReadError::InvalidQuery("unknown watch"))?;
        }
        let table = self.registry(budget)?;
        table
            .entries
            .get(&id)
            .filter(|entry| {
                entry.legacy
                    && entry.active.load(Ordering::Acquire)
                    && entry.session.matches(principal)
            })
            .cloned()
            .ok_or(ReadError::InvalidQuery("unknown watch"))
    }
    fn legacy(
        self: &Arc<Self>,
        principal: &Principal,
        session: &SubscriptionSession,
        request: LegacySubscriptionRequest,
        connection: Option<[u8; 16]>,
        budget: &mut Budget,
    ) -> Result<HGrid, ReadError> {
        if !session.matches(principal) || !session.is_active() {
            return Err(ReadError::Forbidden);
        }
        let (watch, ids) = match &request {
            LegacySubscriptionRequest::Subscribe { watch, ids } => {
                (watch.as_deref(), ids.as_slice())
            }
            LegacySubscriptionRequest::Unsubscribe { watch, ids } => {
                (Some(watch.as_str()), ids.as_slice())
            }
            LegacySubscriptionRequest::Poll { watch } => (Some(watch.as_str()), &[][..]),
        };
        if watch.is_some_and(|watch| watch.len() > 32) || ids.len() > wire::MAX_IDS {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let mut bytes = 512usize;
        for id in ids {
            if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
                return Err(ReadError::InvalidQuery("invalid watch id"));
            }
            bytes = bytes.saturating_add(id.len() + 64);
        }
        if bytes > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.charge(BudgetKind::Work, bytes.saturating_mul(4))?;
        budget.charge(BudgetKind::Retained, bytes.saturating_mul(4))?;
        let (request, return_rows) = match request {
            LegacySubscriptionRequest::Subscribe { watch: None, ids } => (
                SubscriptionRequest::Create(SubscriptionCreate {
                    authority: self.authority,
                    key: legacy_id(&SubscriptionId {
                        authority: self.authority,
                        watch: rand::random(),
                        key: String::new(),
                    }),
                    ids,
                    lease_ms: self.limits.max_lease.as_millis() as u64,
                }),
                true,
            ),
            LegacySubscriptionRequest::Poll { watch } => {
                let entry = self.legacy_entry(&watch, principal, budget)?;
                (
                    SubscriptionRequest::Poll {
                        watch: entry.identity.clone(),
                    },
                    true,
                )
            }
            LegacySubscriptionRequest::Subscribe {
                watch: Some(watch),
                ids,
            } => {
                let entry = self.legacy_entry(&watch, principal, budget)?;
                let record = Self::record(&entry, budget)?;
                let Status::Live(live) = record.status else {
                    return Err(ReadError::InvalidQuery("unknown watch"));
                };
                let mut all = live.ids.to_vec();
                all.extend(ids);
                (
                    SubscriptionRequest::Replace {
                        watch: entry.identity.clone(),
                        expected_scope_generation: live.scope,
                        ids: all,
                    },
                    true,
                )
            }
            LegacySubscriptionRequest::Unsubscribe { watch, ids } => {
                let entry = self.legacy_entry(&watch, principal, budget)?;
                if ids.is_empty() {
                    (
                        SubscriptionRequest::Unsubscribe {
                            watch: entry.identity.clone(),
                        },
                        false,
                    )
                } else {
                    let record = Self::record(&entry, budget)?;
                    let Status::Live(live) = record.status else {
                        return Err(ReadError::InvalidQuery("unknown watch"));
                    };
                    let remaining: Vec<_> = live
                        .ids
                        .iter()
                        .filter(|id| !ids.contains(id))
                        .cloned()
                        .collect();
                    (
                        SubscriptionRequest::Replace {
                            watch: entry.identity.clone(),
                            expected_scope_generation: live.scope,
                            ids: remaining,
                        },
                        false,
                    )
                }
            }
        };
        // Legacy ownership is the authenticated principal, preserving its weaker
        // historical profile. Scoped operations always require exact session identity.
        let owner_session = if let Some(watch) = request.watch() {
            self.legacy_entry(&legacy_id(watch), principal, budget)?
                .session
                .clone()
        } else {
            session.clone()
        };
        let outcome = self.execute(principal, &owner_session, request, true, connection, budget)?;
        match outcome {
            SubscriptionOutcome::Delivery(delivery) => {
                let ack = SubscriptionRequest::Acknowledge {
                    watch: delivery.watch.clone(),
                    scope_generation: delivery.scope_generation,
                    token: delivery.token,
                    through: delivery.through,
                };
                if !matches!(
                    self.execute(principal, &owner_session, ack, true, None, budget)?,
                    SubscriptionOutcome::Acknowledged { .. }
                ) {
                    return Err(ReadError::Unavailable);
                }
                let rows = if return_rows {
                    let mut rows = delivery.rows.clone();
                    for id in &delivery.removed {
                        let mut row = HDict::new();
                        row.set("id", Kind::Ref(HRef::from_val(id)));
                        row.set("removed", Kind::Marker);
                        rows.push(row);
                    }
                    rows
                } else {
                    Vec::new()
                };
                Ok(grid(rows, Some(legacy_id(&delivery.watch))))
            }
            SubscriptionOutcome::Idle { watch, .. } => {
                Ok(grid(Vec::new(), Some(legacy_id(&watch))))
            }
            SubscriptionOutcome::Closed { .. } => Ok(HGrid::new()),
            SubscriptionOutcome::Rejected(SubscriptionRejection::Forbidden) => {
                Err(ReadError::Forbidden)
            }
            SubscriptionOutcome::Rejected(
                SubscriptionRejection::Limit | SubscriptionRejection::Capacity,
            ) => Err(ReadError::Budget(BudgetKind::Output)),
            SubscriptionOutcome::Resync(_) => {
                Err(ReadError::InvalidQuery("watch requires resubscription"))
            }
            _ => Err(ReadError::InvalidQuery("watch rejected")),
        }
    }
}
