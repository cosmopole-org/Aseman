//! The repositories on the storage module (ADR 0036): a storage transaction is a
//! [`CapsuleStore`], so every repository runs on whichever provider plugin the node
//! loaded, inside the action's one transaction.

use crate::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue, ComparisonOperator,
    QueryPredicate, SortDirection,
};
use aseman_storage::schema::Model;
use aseman_storage::{Cond, FindMany, Id, Order, StorageError, Trx, Value, Where, codec};

fn store_error(error: StorageError) -> CapsuleStoreError {
    match error {
        StorageError::Conflict(_) => CapsuleStoreError::Conflict,
        other => CapsuleStoreError::Failed(other.to_string()),
    }
}

fn value(model: &Model, field: &str, value: &CapsuleValue) -> Value {
    if model.relations.contains_key(field) || field == "id" {
        if let CapsuleValue::Bytes(bytes) = value
            && let Ok(id) = <[u8; 16]>::try_from(bytes.as_slice())
        {
            return Value::Id(Id(id));
        }
    }
    match model.fields.get(field) {
        Some(field_type) => codec::record_value(*field_type, value),
        None => codec::record_value(aseman_storage::schema::FieldType::Text, value),
    }
}

fn filter(model: &Model, predicate: &QueryPredicate) -> Where {
    match predicate {
        QueryPredicate::Compare {
            field,
            operator,
            value: compared,
        } => {
            let compared = value(model, field, compared);
            Where::Field(
                field.clone(),
                match operator {
                    ComparisonOperator::Equal => Cond::Equals(compared),
                    ComparisonOperator::NotEqual => Cond::Not(compared),
                    ComparisonOperator::LessThan => Cond::Lt(compared),
                    ComparisonOperator::LessOrEqual => Cond::Lte(compared),
                    ComparisonOperator::GreaterThan => Cond::Gt(compared),
                    ComparisonOperator::GreaterOrEqual => Cond::Gte(compared),
                },
            )
        }
        QueryPredicate::And { predicates } => {
            Where::And(predicates.iter().map(|child| filter(model, child)).collect())
        }
        QueryPredicate::Or { predicates } => {
            Where::Or(predicates.iter().map(|child| filter(model, child)).collect())
        }
        QueryPredicate::Not { predicate } => Where::Not(Box::new(filter(model, predicate))),
        QueryPredicate::RelationshipExists { relationship } => {
            Where::Field(relationship.clone(), Cond::IsNull(false))
        }
    }
}

impl CapsuleStore for Trx {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        self.capsule(&kind.0, Id(id.0)).map_err(store_error)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        self.put_capsule(capsule, expected_revision)
            .map_err(store_error)
    }

    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        // Every write joins this transaction; a failure fails (and rolls back) it.
        for (capsule, expected) in writes {
            self.put(capsule, *expected)?;
        }
        Ok(())
    }

    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        if !query.aggregates.is_empty() || !query.traversals.is_empty() || query.cursor.is_some() {
            return Err(CapsuleStoreError::Failed(
                "unsupported storage capability: aggregates, traversal, and cursors".to_owned(),
            ));
        }
        let model = self
            .schema()
            .model(&query.kind.0)
            .map_err(store_error)?
            .clone();
        let find = FindMany {
            filter: query.predicate.as_ref().map(|predicate| filter(&model, predicate)),
            order_by: query
                .sort
                .iter()
                .map(|sort| match sort.direction {
                    SortDirection::Ascending => Order::asc(&sort.field),
                    SortDirection::Descending => Order::desc(&sort.field),
                })
                .collect(),
            skip: 0,
            take: Some(u64::from(query.limit)),
        };
        self.capsules(&query.kind.0, &find).map_err(store_error)
    }
}
