//! Whole-unit feed traversal with current read authorization and scoped cursors.
use crate::{
    BudgetKind, MutationService, Principal, ReadAdmission, ReadContext, ReadError, ReadOperation,
    budget::Budget, sanitize,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_core::{
    codecs::entity::{self, ChangesPage, ChangesRequest, EntityDiff, EntityWire},
    data::HDict,
    graph::{GraphDiff, GraphState},
};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub(crate) fn raw_diff(diff: &GraphDiff) -> EntityDiff {
    EntityDiff {
        revision: diff.version,
        span: diff.span,
        id: diff.ref_val.clone(),
        operation: diff.op.clone(),
        changed: diff
            .new
            .as_ref()
            .or(diff.changed_tags.as_ref())
            .cloned()
            .unwrap_or_default(),
        previous: diff.old.as_ref().or(diff.previous_tags.as_ref()).cloned(),
    }
}
impl MutationService {
    /// A missing cursor bootstraps at the current head. Subsequent pages accept
    /// future entity revisions, but reset/catalog/principal/policy changes expire
    /// the cursor. Notifications are hints and need not be received for replay.
    pub async fn changes(
        &self,
        context: ReadContext,
        request: ChangesRequest,
    ) -> Result<ChangesPage, ReadError> {
        let admission = self.inner.reads.begin(context).await?;
        self.changes_admitted(admission, request).await
    }
    pub async fn changes_admitted(
        &self,
        admission: ReadAdmission,
        request: ChangesRequest,
    ) -> Result<ChangesPage, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let service = self.clone();
        admission
            .run_task(move |principal, budget| service.feed_page(&principal, request, budget))
            .await
    }
    pub(crate) fn feed_page(
        &self,
        principal: &Principal,
        request: ChangesRequest,
        budget: &mut Budget,
    ) -> Result<ChangesPage, ReadError> {
        request
            .to_kind()
            .map_err(|_| ReadError::InvalidQuery("invalid changes request"))?;
        if principal.bytes() > budget.limits.max_input_bytes
            || matches!(principal,Principal::Authenticated{permissions,..} if permissions.len()>budget.limits.max_ids)
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.check()?;
        let policy = self.inner.reads.policy_snapshot(principal)?;
        budget.check()?;
        if !policy.operation(ReadOperation::Changes) {
            return Err(ReadError::Forbidden);
        }
        if policy.scope_key().len() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let graph = self.store().graph();
        loop {
            if let Some(result) = graph.read_for(budget.wait_quantum()?, |g| {
                let state = g.state();
                let position = match &request.cursor {
                    Some(cursor) => {
                        self.decode_cursor(cursor, principal, policy.scope_key(), state)?
                    }
                    None => state.revision,
                };
                let units = g
                    .change_units_since(position)
                    .map_err(|_| ReadError::StaleCursor)?;
                let mut page = ChangesPage {
                    dataset: self.store().dataset(),
                    incarnation: state.incarnation,
                    head: state.revision,
                    floor: units.floor,
                    position,
                    cursor: String::new(),
                    complete: position == state.revision,
                    changes: vec![],
                };
                let mut used = 0;
                let mut encoded_bound = 0usize;
                for unit in units {
                    budget.check()?;
                    // Source diff count includes denied rows; whole spans are
                    // never split or skipped to manufacture an advancing cursor.
                    if unit.len() > request.max_diffs.saturating_sub(used) {
                        if used == 0 {
                            return Err(ReadError::UnitTooLarge);
                        }
                        break;
                    }
                    budget.charge(BudgetKind::Work, unit.len())?;
                    budget.charge(
                        BudgetKind::Retained,
                        unit.retained_bytes().saturating_mul(4),
                    )?;
                    let mut changes = Vec::new();
                    for diff in unit.diffs() {
                        budget.charge(BudgetKind::Work, diff.ref_val.len().saturating_add(1))?;
                        if !policy.entity(&diff.ref_val) || !policy.tag(&diff.ref_val, "id") {
                            continue;
                        }
                        let empty = HDict::new();
                        let changed = diff
                            .new
                            .as_ref()
                            .or(diff.changed_tags.as_ref())
                            .unwrap_or(&empty);
                        let changed =
                            sanitize::record(&diff.ref_val, changed, policy.as_ref(), budget)?
                                .unwrap_or_default();
                        let previous = diff
                            .old
                            .as_ref()
                            .or(diff.previous_tags.as_ref())
                            .map(|row| {
                                sanitize::record(&diff.ref_val, row, policy.as_ref(), budget)
                            })
                            .transpose()?
                            .flatten();
                        changes.push(EntityDiff {
                            revision: diff.version,
                            span: diff.span,
                            id: budget.copy_string(&diff.ref_val)?,
                            operation: diff.op.clone(),
                            changed,
                            previous,
                        });
                    }
                    budget.check()?;
                    let candidate = ChangesPage {
                        dataset: page.dataset,
                        incarnation: page.incarnation,
                        head: page.head,
                        floor: page.floor,
                        position: unit.span.last,
                        cursor: self.encode_cursor(
                            principal,
                            policy.scope_key(),
                            state,
                            unit.span.last,
                        ),
                        complete: unit.span.last == state.revision,
                        changes,
                    };
                    let unit_bytes = entity::encode(&candidate)
                        .map_err(|_| ReadError::UnitTooLarge)?
                        .len();
                    if unit_bytes > self.limits().page_bytes {
                        return Err(ReadError::UnitTooLarge);
                    }
                    if unit_bytes > self.limits().page_bytes.saturating_sub(encoded_bound) {
                        break;
                    }
                    encoded_bound += unit_bytes;
                    used += unit.len();
                    page.changes.extend(candidate.changes);
                    page.position = unit.span.last;
                }
                budget.check()?;
                page.complete = page.position == page.head;
                page.cursor =
                    self.encode_cursor(principal, policy.scope_key(), state, page.position);
                if entity::encode(&page)
                    .map_err(|_| ReadError::UnitTooLarge)?
                    .len()
                    > self.limits().page_bytes
                {
                    return Err(ReadError::UnitTooLarge);
                }
                Ok(page)
            }) {
                return result;
            }
        }
    }
    fn mac(&self, principal: &Principal, scope: &str, bytes: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.inner.cursor_key).expect("HMAC key");
        mac.update(b"entity-feed-v1\0typed-v1\0");
        mac.update(bytes);
        // Bounded private principal/scope bytes never appear in the cursor.
        let principal = format!("{principal:?}");
        mac.update(&(principal.len() as u64).to_be_bytes());
        mac.update(principal.as_bytes());
        mac.update(scope.as_bytes());
        mac
    }
    fn encode_cursor(
        &self,
        principal: &Principal,
        scope: &str,
        state: GraphState,
        position: u64,
    ) -> String {
        let expires = u64::try_from(
            self.inner
                .started
                .elapsed()
                .as_nanos()
                .saturating_add(self.inner.reads.limits().cursor_ttl.as_nanos()),
        )
        .unwrap_or(u64::MAX);
        let mut bytes = Vec::with_capacity(89);
        bytes.push(1);
        bytes.extend_from_slice(&self.store().dataset());
        bytes.extend_from_slice(&state.incarnation);
        bytes.extend_from_slice(&state.catalog_generation.to_be_bytes());
        bytes.extend_from_slice(&position.to_be_bytes());
        bytes.extend_from_slice(&expires.to_be_bytes());
        let signature = self.mac(principal, scope, &bytes).finalize().into_bytes();
        bytes.extend_from_slice(&signature);
        URL_SAFE_NO_PAD.encode(bytes)
    }
    fn decode_cursor(
        &self,
        cursor: &str,
        principal: &Principal,
        scope: &str,
        state: GraphState,
    ) -> Result<u64, ReadError> {
        if cursor.len() > 128 {
            return Err(ReadError::StaleCursor);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| ReadError::StaleCursor)?;
        if bytes.len() != 89 || bytes[0] != 1 {
            return Err(ReadError::StaleCursor);
        }
        self.mac(principal, scope, &bytes[..57])
            .verify_slice(&bytes[57..])
            .map_err(|_| ReadError::StaleCursor)?;
        let number = |offset| {
            u64::from_be_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .expect("validated cursor length"),
            )
        };
        if bytes[1..17] != self.store().dataset()
            || bytes[17..33] != state.incarnation
            || number(33) != state.catalog_generation
            || u128::from(number(49)) <= self.inner.started.elapsed().as_nanos()
        {
            return Err(ReadError::StaleCursor);
        }
        Ok(number(41))
    }
}
