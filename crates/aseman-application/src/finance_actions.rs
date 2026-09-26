// The finance action bodies, split from the module scaffold so the shared helpers
// stay readable. Each function is a faithful translation of one legacy finance
// action closure, with identical validation, ordering, and client-visible errors.

// ── publish_finance_catalog ───────────────────────────────────────────────────

pub fn publish_finance_catalog(
    ports: &FinancePorts,
    caller: &str,
    input: PublishFinanceCatalogInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT {
        return Err(denied("global platform owner required"));
    }
    let catalog = federated_finance_object(input.catalog, "finance catalog")?;
    let version = catalog.get("version").and_then(Value::as_str).unwrap_or("").to_string();
    if !valid_finance_id(&version)
        || !federated_finance_safe_numbers(&Value::Object(catalog.clone()))
    {
        return Err(denied("invalid finance catalog"));
    }
    for key in [
        "tokenScale",
        "defaultInputPerMillionMinor",
        "defaultOutputPerMillionMinor",
        "sandboxPerMinuteMinor",
        "minChargeMinor",
        "platformCommissionBps",
        "authorizationSafetyBps",
        "quoteTtlMs",
        "holdTtlMs",
    ] {
        if catalog.get(key).and_then(Value::as_i64).is_none() {
            return Err(denied(&format!("invalid finance catalog integer: {key}")));
        }
    }
    if catalog.get("tokenScale").and_then(Value::as_i64).unwrap_or(0) <= 0
        || catalog.get("sandboxPerMinuteMinor").and_then(Value::as_i64).unwrap_or(0) <= 0
    {
        return Err(denied("tokenScale and sandbox rate must be positive"));
    }
    let commission = catalog.get("platformCommissionBps").and_then(Value::as_i64).unwrap_or(0);
    let safety = catalog.get("authorizationSafetyBps").and_then(Value::as_i64).unwrap_or(0);
    let quote_ttl = catalog.get("quoteTtlMs").and_then(Value::as_i64).unwrap_or(0);
    let hold_ttl = catalog.get("holdTtlMs").and_then(Value::as_i64).unwrap_or(0);
    if commission > 10_000
        || !(10_000..=100_000).contains(&safety)
        || quote_ttl > hold_ttl
        || hold_ttl > FINANCE_HOLD_MAX_TTL_MS
    {
        return Err(denied("invalid finance catalog policy"));
    }
    for key in [
        "settlementAuthority",
        "platformAccountId",
        "providerClearingAccountId",
        "nodeOwnerAccountId",
    ] {
        let account = catalog.get(key).and_then(Value::as_str).unwrap_or("");
        if !valid_finance_id(account) || ports.account(account)?.is_none() {
            return Err(denied(&format!("invalid finance catalog account: {key}")));
        }
    }
    let catalog_value = Value::Object(catalog.clone());
    let catalog_hash = finance_hash(&catalog_value)?;
    let mut already_published = false;
    if let Ok(existing) = billing_catalog(ports, &version)
        && !existing.is_empty()
    {
        if Value::Object(existing) != catalog_value {
            return Err(denied("pricing version is immutable"));
        }
        already_published = true;
    }
    put_billing_catalog(ports, &version, &catalog_value)?;
    ports
        .ledger
        .put_doc(
            FinanceDoc::BillingCurrent,
            "",
            "current",
            &json!({"version": version, "catalogHash": catalog_hash}),
            false,
        )
        .map_err(ApplicationError::from)?;
    Ok(json!({
        "ok": true,
        "catalog": catalog,
        "catalogHash": catalog_hash,
        "alreadyPublished": already_published,
    }))
}

// ── register_finance_node ─────────────────────────────────────────────────────

pub fn register_finance_node(
    ports: &FinancePorts,
    caller: &str,
    input: RegisterFinanceNodeInput,
) -> Result<Value, ApplicationError> {
    let mut node = federated_finance_object(input.node, "finance node")?;
    let owner = node.get("nodeOwnerAccountId").and_then(Value::as_str).unwrap_or("");
    let authority = node.get("settlementAuthority").and_then(Value::as_str).unwrap_or("");
    let origin = node.get("originId").and_then(Value::as_str).unwrap_or("");
    let meter = node.get("meterProgramId").and_then(Value::as_str).unwrap_or("");
    let talent_meter = node.get("talentMeterProgramId").and_then(Value::as_str).unwrap_or("");
    let meter_creature = node.get("meterCreatureId").and_then(Value::as_str).unwrap_or("");
    let meter_entity = node.get("meterEntityId").and_then(Value::as_str).unwrap_or("");
    let talent_meter_creature = node.get("talentMeterCreatureId").and_then(Value::as_str).unwrap_or("");
    let talent_meter_entity = node.get("talentMeterEntityId").and_then(Value::as_str).unwrap_or("");
    let revision = node.get("revision").and_then(Value::as_str).unwrap_or("");
    let rate = node.get("sandboxPerMinuteMinor").and_then(Value::as_i64).unwrap_or(0);
    if caller != owner
        || authority != caller
        || !valid_finance_id(caller)
        || !valid_finance_origin(origin)
        || !valid_finance_id(meter)
        || !valid_finance_id(talent_meter)
        || !valid_finance_id(meter_creature)
        || !valid_finance_id(meter_entity)
        || !valid_finance_id(talent_meter_creature)
        || !valid_finance_id(talent_meter_entity)
        || !valid_finance_hash(revision)
        || rate <= 0
        || rate > 9_007_199_254_740_991
        || ports.account(caller)?.is_none()
    {
        return Err(denied("invalid host-attested finance node registration"));
    }
    let now = ports.clock.unix_millis();
    node.insert("status".into(), json!("active"));
    node.insert("updatedAt".into(), json!(now));
    put_billing_nodes(ports, &json!({caller.to_string(): Value::Object(node.clone())}))?;
    Ok(json!({"ok": true, "node": node}))
}

// ── retire_finance_node ───────────────────────────────────────────────────────

pub fn retire_finance_node(
    ports: &FinancePorts,
    caller: &str,
    input: RetireFinanceNodeInput,
) -> Result<Value, ApplicationError> {
    if input.node_owner_account_id != caller || !valid_finance_id(caller) {
        return Err(denied("node owner mismatch"));
    }
    let nodes = billing_nodes(ports);
    let mut node = nodes
        .get(caller)
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| denied("finance node not found"))?;
    let now = ports.clock.unix_millis();
    node.insert("status".into(), json!("retired"));
    node.insert("updatedAt".into(), json!(now));
    node.insert(
        "revision".into(),
        json!(finance_hash(&json!({
            "prior": node.get("revision"), "status": "retired", "updatedAt": now
        }))?),
    );
    put_billing_nodes(ports, &json!({caller.to_string(): Value::Object(node.clone())}))?;
    Ok(json!({"ok": true, "node": node}))
}

// ── register_finance_resource ─────────────────────────────────────────────────

pub fn register_finance_resource(
    ports: &FinancePorts,
    caller: &str,
    input: RegisterFinanceResourceInput,
) -> Result<Value, ApplicationError> {
    let mut resource = federated_finance_object(input.resource, "finance resource")?;
    let resource_id = resource.get("programId").and_then(Value::as_str).unwrap_or("").to_string();
    let kind = resource.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    let owner = resource.get("owner").and_then(Value::as_str).unwrap_or("").to_string();
    let host_owner = resource.get("hostNodeOwnerAccountId").and_then(Value::as_str).unwrap_or("").to_string();
    let bucket = federated_finance_market_bucket(&kind)
        .ok_or_else(|| denied("invalid finance resource kind"))?;
    let pricing = resource.get("pricing").cloned().unwrap_or(Value::Null);
    if caller != host_owner
        || !valid_finance_id(&resource_id)
        || !valid_finance_id(&owner)
        || ports.account(&owner)?.is_none()
        || !federated_finance_safe_numbers(&pricing)
    {
        return Err(denied("invalid host-attested finance resource"));
    }
    let nodes = billing_nodes(ports);
    let node = nodes
        .get(&host_owner)
        .and_then(Value::as_object)
        .ok_or_else(|| denied("finance execution node not registered"))?;
    if node.get("status").and_then(Value::as_str) != Some("active")
        || resource.get("hostOriginId").and_then(Value::as_str) != node.get("originId").and_then(Value::as_str)
        || resource.get("billingMeterProgramId").and_then(Value::as_str) != node.get("meterProgramId").and_then(Value::as_str)
        || resource.get("billingMeterCreatureId").and_then(Value::as_str) != node.get("meterCreatureId").and_then(Value::as_str)
        || resource.get("billingMeterEntityId").and_then(Value::as_str) != node.get("meterEntityId").and_then(Value::as_str)
        || resource.get("nodeRegistrationRevision").and_then(Value::as_str) != node.get("revision").and_then(Value::as_str)
        || resource.get("nodeSandboxPerMinuteMinor").and_then(Value::as_i64) != node.get("sandboxPerMinuteMinor").and_then(Value::as_i64)
    {
        return Err(denied("resource does not match its active finance node"));
    }
    let entries = market_doc(ports, bucket);
    let existing = entries.get(&resource_id).and_then(Value::as_object);
    if let Some(existing) = existing
        && existing.get("hostNodeOwnerAccountId").and_then(Value::as_str) != Some(host_owner.as_str())
    {
        return Err(denied("resource migration requires a new program id"));
    }
    let requested_status = resource
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| caller == LEGACY_ROOT && matches!(*status, "approved" | "denied"))
        .unwrap_or("pending");
    let preserved_status = existing
        .and_then(|row| row.get("status"))
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "approved" | "denied"))
        .unwrap_or(requested_status);
    resource.insert("status".into(), json!(preserved_status));
    resource.insert("federated".into(), json!(true));
    resource.insert("registeredAt".into(), json!(ports.clock.unix_millis()));
    put_market_doc(ports, bucket, &json!({resource_id.clone(): Value::Object(resource.clone())}))?;
    Ok(json!({"ok": true, "resource": resource}))
}

// ── review_finance_resource ───────────────────────────────────────────────────

pub fn review_finance_resource(
    ports: &FinancePorts,
    caller: &str,
    input: ReviewFinanceResourceInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT || !matches!(input.status.as_str(), "approved" | "denied") {
        return Err(denied("global finance reviewer required"));
    }
    let bucket = federated_finance_market_bucket(&input.kind)
        .ok_or_else(|| denied("invalid finance resource kind"))?;
    let entries = market_doc(ports, bucket);
    let mut resource = entries
        .get(&input.resource_id)
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| denied("finance resource not found"))?;
    resource.insert("status".into(), json!(input.status));
    resource.insert("reviewedBy".into(), json!(caller));
    resource.insert("reviewedAt".into(), json!(ports.clock.unix_millis()));
    if !input.reason.is_empty() {
        resource.insert("reason".into(), json!(input.reason));
    }
    put_market_doc(ports, bucket, &json!({input.resource_id.clone(): Value::Object(resource.clone())}))?;
    Ok(json!({"ok": true, "resource": resource}))
}

// ── retire_finance_resource ───────────────────────────────────────────────────

pub fn retire_finance_resource(
    ports: &FinancePorts,
    caller: &str,
    input: RetireFinanceResourceInput,
) -> Result<Value, ApplicationError> {
    let bucket = federated_finance_market_bucket(&input.kind)
        .ok_or_else(|| denied("invalid finance resource kind"))?;
    let entries = market_doc(ports, bucket);
    let resource = entries
        .get(&input.resource_id)
        .and_then(Value::as_object)
        .ok_or_else(|| denied("finance resource not found"))?;
    let host_owner = resource.get("hostNodeOwnerAccountId").and_then(Value::as_str).unwrap_or("");
    if caller != host_owner && caller != LEGACY_ROOT {
        return Err(denied("resource host or global reviewer required"));
    }
    put_market_doc(ports, bucket, &json!({input.resource_id.clone(): Value::Null}))?;
    Ok(json!({"ok": true, "resourceId": input.resource_id}))
}

// ── publish_finance_quote ─────────────────────────────────────────────────────

fn validate_federated_quote_resource(
    ports: &FinancePorts,
    execution: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    let resource_id = execution.get("resourceId").and_then(Value::as_str).unwrap_or("");
    let kind = execution.get("kind").and_then(Value::as_str).unwrap_or("");
    let bucket = federated_finance_market_bucket(kind)
        .ok_or_else(|| denied("invalid quote execution resource kind"))?;
    if !valid_finance_id(resource_id) {
        return Err(denied("invalid quote execution resource id"));
    }
    let entries = market_doc(ports, bucket);
    let resource = entries
        .get(resource_id)
        .and_then(Value::as_object)
        .ok_or_else(|| denied("quoted resource is not globally registered"))?;
    if resource.get("status").and_then(Value::as_str) != Some("approved") {
        return Err(denied("quoted resource is not globally approved"));
    }
    let node_owner = resource.get("hostNodeOwnerAccountId").and_then(Value::as_str).unwrap_or("");
    let nodes = billing_nodes(ports);
    let node = nodes
        .get(node_owner)
        .and_then(Value::as_object)
        .ok_or_else(|| denied("quoted resource node is not registered"))?;
    if node.get("status").and_then(Value::as_str) != Some("active")
        || execution.get("nodeOwnerAccountId") != resource.get("hostNodeOwnerAccountId")
        || execution.get("hostOriginId") != resource.get("hostOriginId")
        || execution.get("meterProgramId") != resource.get("billingMeterProgramId")
        || execution.get("meterCreatureId") != resource.get("billingMeterCreatureId")
        || execution.get("meterEntityId") != resource.get("billingMeterEntityId")
        || execution.get("nodeRegistrationRevision") != resource.get("nodeRegistrationRevision")
        || execution.get("sandboxPerMinuteMinor") != resource.get("nodeSandboxPerMinuteMinor")
        || resource.get("hostOriginId") != node.get("originId")
        || resource.get("billingMeterProgramId") != node.get("meterProgramId")
        || resource.get("billingMeterCreatureId") != node.get("meterCreatureId")
        || resource.get("billingMeterEntityId") != node.get("meterEntityId")
        || resource.get("nodeRegistrationRevision") != node.get("revision")
        || resource.get("nodeSandboxPerMinuteMinor") != node.get("sandboxPerMinuteMinor")
        || execution.get("settlementAuthority") != node.get("settlementAuthority")
    {
        return Err(denied("quote execution does not match the active global resource binding"));
    }
    Ok(())
}

pub fn publish_finance_quote(
    ports: &FinancePorts,
    caller: &str,
    input: PublishFinanceQuoteInput,
) -> Result<Value, ApplicationError> {
    let mut quote = federated_finance_object(input.quote, "finance quote")?;
    let quote_id = quote.get("quoteId").and_then(Value::as_str).unwrap_or("").to_string();
    let payer = quote.get("payerUserId").and_then(Value::as_str).unwrap_or("").to_string();
    let max_amount = quote.get("maxAmount").and_then(Value::as_i64).unwrap_or(0);
    let hold = quote
        .get("holdRequest")
        .and_then(Value::as_object)
        .ok_or_else(|| denied("quote holdRequest missing"))?;
    let execution = quote
        .get("executionPlan")
        .and_then(Value::as_object)
        .ok_or_else(|| denied("quote executionPlan missing"))?;
    let quote_kind = quote.get("kind").and_then(Value::as_str).unwrap_or("");
    let authority = execution.get("settlementAuthority").and_then(Value::as_str).unwrap_or("");
    let meter = execution.get("meterProgramId").and_then(Value::as_str).unwrap_or("");
    let meter_creature = execution.get("meterCreatureId").and_then(Value::as_str).unwrap_or("");
    let meter_entity = execution.get("meterEntityId").and_then(Value::as_str).unwrap_or("");
    let pricing_version = quote.get("pricingVersion").and_then(Value::as_str).unwrap_or("");
    let active_catalog = billing_current(ports);
    let catalog_exists = !pricing_version.is_empty()
        && ports
            .ledger
            .get_doc(FinanceDoc::BillingCatalog, pricing_version, "catalog")
            .map(|catalog| !catalog.is_empty())
            .unwrap_or(false);
    let nodes = billing_nodes(ports);
    let issuer_node = nodes.get(caller).and_then(Value::as_object);
    let coordinator_node = nodes.get(authority).and_then(Value::as_object);
    let expected_meter = coordinator_node.and_then(|node| {
        if quote_kind == "talent" {
            node.get("talentMeterProgramId")
        } else {
            node.get("meterProgramId")
        }
    });
    if !valid_finance_id(&quote_id)
        || !valid_finance_id(&payer)
        || !matches!(quote_kind, "agent" | "tool" | "talent")
        || active_catalog.get("version").and_then(Value::as_str) != Some(pricing_version)
        || !catalog_exists
        || max_amount <= 0
        || (quote_kind == "talent" && authority != caller)
        || issuer_node.and_then(|node| node.get("status")).and_then(Value::as_str) != Some("active")
        || coordinator_node.and_then(|node| node.get("status")).and_then(Value::as_str) != Some("active")
        || expected_meter.and_then(Value::as_str) != Some(meter)
        || (quote_kind != "talent"
            && (coordinator_node.and_then(|node| node.get("meterCreatureId")).and_then(Value::as_str) != Some(meter_creature)
                || coordinator_node.and_then(|node| node.get("meterEntityId")).and_then(Value::as_str) != Some(meter_entity)))
        || hold.get("quoteId").and_then(Value::as_str) != Some(quote_id.as_str())
        || hold.get("maxAmount").and_then(Value::as_i64) != Some(max_amount)
        || hold.get("settlementAuthority").and_then(Value::as_str) != Some(authority)
        || hold.get("meterProgramId").and_then(Value::as_str) != Some(meter)
        || ports.account(&payer)?.is_none()
        || ports.account(caller)?.is_none()
        || !federated_finance_safe_numbers(&Value::Object(quote.clone()))
    {
        return Err(denied("invalid immutable finance quote"));
    }
    let resources = execution
        .get("resources")
        .and_then(Value::as_array)
        .ok_or_else(|| denied("quote execution resources missing"))?;
    if quote_kind == "talent" {
        if !resources.is_empty() {
            return Err(denied("talent quote cannot contain execution resources"));
        }
    } else {
        if resources.is_empty() || resources.len() > 9 {
            return Err(denied("invalid quote execution resource count"));
        }
        let mut seen = std::collections::HashMap::<String, bool>::new();
        for (index, raw) in resources.iter().enumerate() {
            let row = raw.as_object().ok_or_else(|| denied("invalid quote execution resource"))?;
            validate_federated_quote_resource(ports, row)?;
            let resource_id = row.get("resourceId").and_then(Value::as_str).unwrap_or("").to_string();
            if seen.insert(resource_id.clone(), true).is_some() {
                return Err(denied("duplicate quote execution resource"));
            }
            if index == 0
                && (row.get("kind").and_then(Value::as_str) != Some(quote_kind)
                    || resource_id != quote.get("resourceId").and_then(Value::as_str).unwrap_or(""))
            {
                return Err(denied("quote coordinator resource mismatch"));
            }
        }
        let coordinator = resources[0].as_object().unwrap();
        if coordinator.get("settlementAuthority").and_then(Value::as_str) != Some(authority)
            || coordinator.get("meterProgramId").and_then(Value::as_str) != Some(meter)
            || coordinator.get("meterCreatureId").and_then(Value::as_str) != Some(meter_creature)
            || coordinator.get("meterEntityId").and_then(Value::as_str) != Some(meter_entity)
        {
            return Err(denied("quote coordinator execution mismatch"));
        }
    }
    let beneficiaries = hold
        .get("beneficiaries")
        .and_then(Value::as_array)
        .ok_or_else(|| denied("quote beneficiaries missing"))?;
    if beneficiaries.is_empty() || beneficiaries.len() > FINANCE_MAX_BENEFICIARIES {
        return Err(denied("invalid quote beneficiary count"));
    }
    let mut cap_total = 0_i64;
    for raw in beneficiaries {
        let row = raw.as_object().ok_or_else(|| denied("invalid quote beneficiary"))?;
        let user_id = row.get("userId").and_then(Value::as_str).unwrap_or("");
        let amount = row.get("maxAmount").and_then(Value::as_i64).unwrap_or(0);
        if !valid_finance_id(user_id) || amount <= 0 || ports.account(user_id)?.is_none() {
            return Err(denied("invalid quote beneficiary"));
        }
        cap_total = cap_total.checked_add(amount).ok_or_else(|| denied("quote beneficiary overflow"))?;
    }
    if cap_total != max_amount {
        return Err(denied("quote caps do not equal maxAmount"));
    }
    if let Ok(existing) = billing_quote(ports, &quote_id)
        && !existing.is_empty()
    {
        let mut comparable = existing.clone();
        comparable.remove("quoteIssuerNodeOwnerId");
        comparable.remove("publishedAt");
        if comparable != quote {
            return Err(denied("quote id is immutable"));
        }
        return Ok(json!({"ok": true, "alreadyPublished": true, "quote": existing}));
    }
    quote.insert("quoteIssuerNodeOwnerId".into(), json!(caller));
    quote.insert("publishedAt".into(), json!(ports.clock.unix_millis()));
    put_billing_quote(ports, &quote_id, &Value::Object(quote.clone()))?;
    Ok(json!({"ok": true, "quote": quote}))
}

// ── create_hold ───────────────────────────────────────────────────────────────

fn finance_beneficiary_plan_hash(beneficiaries: &[HoldBeneficiaryInput]) -> String {
    let mut hasher = Sha256::new();
    for beneficiary in beneficiaries {
        hasher.update(beneficiary.user_id.as_bytes());
        hasher.update([0]);
        hasher.update(beneficiary.role.as_bytes());
        hasher.update([0]);
        hasher.update(beneficiary.max_amount.to_string().as_bytes());
        hasher.update(*b"\n");
    }
    hex::encode(hasher.finalize())
}

pub fn create_hold(
    ports: &FinancePorts,
    payer_id: &str,
    input: CreateHoldInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.pricing_version)
        || !valid_finance_id(&input.idempotency_key)
        || !valid_finance_id(&input.settlement_authority)
        || !valid_finance_id(&input.meter_program_id)
    {
        return Err(denied(
            "invalid quote, pricing, meter, authority, or idempotency identifier",
        ));
    }
    if !valid_finance_hash(&input.context_hash) || !valid_finance_hash(&input.beneficiary_plan_hash)
    {
        return Err(denied("contextHash and beneficiaryPlanHash must be sha256 hex"));
    }
    if input.max_amount <= 0 {
        return Err(denied("maxAmount must be greater than zero"));
    }
    if input.expires_at <= now
        || input.expires_at
            > now
                .checked_add(FINANCE_HOLD_MAX_TTL_MS)
                .ok_or_else(|| denied("hold expiry overflow"))?
    {
        return Err(denied("expiresAt must be in the future and within 24 hours"));
    }
    if input.beneficiaries.is_empty() || input.beneficiaries.len() > FINANCE_MAX_BENEFICIARIES {
        return Err(denied("beneficiaries must contain between 1 and 64 entries"));
    }

    let quote = billing_quote(ports, &input.quote_id)?;
    if quote.get("payerUserId").and_then(Value::as_str) != Some(payer_id) {
        return Err(denied("billing quote payer mismatch"));
    }
    let quote_expires_at = as_i64(quote.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    if quote_expires_at <= 0 || now > quote_expires_at {
        return Err(denied("billing quote expired"));
    }
    let signed_request = serde_json::to_value(&input).map_err(|error| failed(error.to_string()))?;
    if quote.get("holdRequest") != Some(&signed_request) {
        return Err(denied("hold request does not match server quote"));
    }
    let project_id = quote.get("projectId").and_then(Value::as_str).unwrap_or("").to_string();
    if !project_id.is_empty() && !ports.is_project_member(payer_id, &project_id)? {
        return Err(denied("payer is not a project member"));
    }
    if ports.account(&input.settlement_authority)?.is_none() {
        return Err(denied("settlement authority not found"));
    }
    if ports.programs.program(&input.meter_program_id)?.is_none() {
        return Err(denied("meter program not found"));
    }

    let computed_plan_hash = finance_beneficiary_plan_hash(&input.beneficiaries);
    if computed_plan_hash != input.beneficiary_plan_hash.to_ascii_lowercase() {
        return Err(denied("beneficiary plan hash mismatch"));
    }
    let request_hash = finance_hash(&serde_json::to_value(&input).map_err(|error| failed(error.to_string()))?)?;
    let request_marker = FinanceMarker::HoldRequest {
        payer: payer_id.to_owned(),
        key: input.idempotency_key.clone(),
    };
    let previous = ports.ledger.marker(&request_marker)?;
    if !previous.is_empty() {
        let Some((hold_id, previous_hash)) = previous.split_once('|') else {
            return Err(denied("invalid hold idempotency record"));
        };
        if previous_hash != request_hash {
            return Err(denied("idempotency key already used with different request"));
        }
        let hold = get_finance_hold(ports.ledger, hold_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "hold": hold}));
    }

    let mut cap_total = 0_i64;
    let mut caps = std::collections::HashMap::<String, i64>::new();
    let mut participants = vec![payer_id.to_owned(), input.settlement_authority.clone()];
    for beneficiary in &input.beneficiaries {
        if !valid_finance_id(&beneficiary.user_id)
            || !valid_finance_id(&beneficiary.role)
            || beneficiary.max_amount <= 0
        {
            return Err(denied("invalid beneficiary"));
        }
        if beneficiary.user_id == payer_id {
            return Err(denied("payer cannot be a hold beneficiary"));
        }
        let cap_key = format!("{}|{}", beneficiary.user_id, beneficiary.role);
        if caps.insert(cap_key, beneficiary.max_amount).is_some() {
            return Err(denied("duplicate beneficiary role"));
        }
        if ports.account(&beneficiary.user_id)?.is_none() {
            return Err(denied("beneficiary not found"));
        }
        cap_total = cap_total.checked_add(beneficiary.max_amount).ok_or_else(|| denied("beneficiary cap overflow"))?;
        participants.push(beneficiary.user_id.clone());
    }
    if cap_total != input.max_amount {
        return Err(denied("beneficiary caps must equal maxAmount"));
    }

    let Some(mut payer) = ports.account(payer_id)? else {
        return Err(denied("payer creature not found"));
    };
    if finance_counter(ports.ledger, WalletCounter::Debt, payer_id)? > 0 {
        return Err(denied("wallet has outstanding payment debt"));
    }
    let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, payer_id)?;
    if withdrawable > payer.balance {
        return Err(denied("withdrawable balance exceeds available balance"));
    }
    let nonwithdrawable = payer.balance - withdrawable;
    let withdrawable_amount = input.max_amount.saturating_sub(nonwithdrawable);
    payer.balance = payer
        .balance
        .checked_sub(input.max_amount)
        .ok_or_else(|| denied(
            "insufficient available balance to authorize this run (funds may be held by another active run)",
        ))?;
    set_finance_counter(
        ports.ledger,
        WalletCounter::Withdrawable,
        payer_id,
        withdrawable.checked_sub(withdrawable_amount).ok_or_else(|| {
            denied("could not authorize this run against the current balance (concurrent authorization in progress) — please retry")
        })?,
    )?;
    let held = finance_counter(ports.ledger, WalletCounter::Held, payer_id)?
        .checked_add(input.max_amount)
        .ok_or_else(|| denied("held balance overflow"))?;
    reserve_project_budget(ports, &project_id, input.max_amount, now)?;

    let hold_id = ports.ledger.gen_id();
    let hold = json!({
        "version": 2,
        "holdId": hold_id,
        "payerUserId": payer_id,
        "quoteId": input.quote_id,
        "pricingVersion": input.pricing_version,
        "maxAmount": input.max_amount,
        "remainingAmount": input.max_amount,
        "withdrawableAmount": withdrawable_amount,
        "meterProgramId": input.meter_program_id,
        "settlementAuthority": input.settlement_authority,
        "expiresAt": input.expires_at,
        "projectId": project_id,
        "contextHash": input.context_hash,
        "beneficiaryPlanHash": input.beneficiary_plan_hash.to_ascii_lowercase(),
        "beneficiaries": input.beneficiaries,
        "requestHash": request_hash,
        "status": "open",
        "createdAt": now,
    });
    let hold_map = hold.as_object().cloned().ok_or_else(|| denied("invalid hold record"))?;

    ports.store_account(&payer)?;
    set_finance_counter(ports.ledger, WalletCounter::Held, payer_id, held)?;
    put_finance_hold(ports.ledger, &hold_id, &hold_map)?;
    ports
        .ledger
        .put_marker(&request_marker, &format!("{hold_id}|{request_hash}"))?;
    ports
        .ledger
        .put_doc(FinanceDoc::Hold, &hold_id, "hold", &Value::Object(hold_map.clone()), false)
        .map_err(ApplicationError::from)?;
    let journal_id = write_finance_journal(
        ports.ledger,
        "hold.created",
        &hold_id,
        payer_id,
        json!({
            "entries": [
                {"account": format!("wallet:{payer_id}:available"), "amount": -input.max_amount},
                {"account": format!("wallet:{payer_id}:held"), "amount": input.max_amount}
            ],
            "quoteId": input.quote_id,
            "pricingVersion": input.pricing_version,
            "projectId": project_id,
        }),
        &participants,
        now,
    )?;

    Ok(json!({"applied": true, "hold": hold_map, "journalId": journal_id}))
}

// ── start_hold ────────────────────────────────────────────────────────────────

pub fn start_hold(
    ports: &FinancePorts,
    authority_id: &str,
    input: StartHoldInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.hold_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.run_id)
    {
        return Err(denied("invalid run authorization"));
    }
    let run_marker = FinanceMarker::Run {
        authority: authority_id.to_owned(),
        run_id: input.run_id.clone(),
    };
    let previous_hold_id = ports.ledger.marker(&run_marker)?;
    if !previous_hold_id.is_empty() {
        if previous_hold_id != input.hold_id {
            return Err(denied("run id already used for another hold"));
        }
        let hold = get_finance_hold(ports.ledger, &input.hold_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "hold": hold}));
    }
    let mut hold = get_finance_hold(ports.ledger, &input.hold_id)?;
    if hold.get("status").and_then(Value::as_str) != Some("open") {
        return Err(denied("hold is not open"));
    }
    if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("payer does not match hold"));
    }
    if hold.get("quoteId").and_then(Value::as_str) != Some(input.quote_id.as_str()) {
        return Err(denied("quote does not match hold"));
    }
    if hold.get("settlementAuthority").and_then(Value::as_str) != Some(authority_id) {
        return Err(denied("caller is not the settlement authority"));
    }
    let expires_at = as_i64(hold.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    if expires_at <= 0 || now > expires_at {
        return Err(denied("hold expired"));
    }
    hold.insert("status".to_string(), json!("running"));
    hold.insert("runId".to_string(), json!(input.run_id));
    hold.insert("startedAt".to_string(), json!(now));
    put_finance_hold(ports.ledger, &input.hold_id, &hold)?;
    ports.ledger.put_marker(&run_marker, &input.hold_id)?;
    let participants = vec![input.payer_user_id.clone(), authority_id.to_owned()];
    let journal_id = write_finance_journal(
        ports.ledger,
        "hold.started",
        &input.hold_id,
        &input.payer_user_id,
        json!({"entries": [], "quoteId": input.quote_id, "runId": input.run_id}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "hold": hold, "journalId": journal_id}))
}

// ── settle_hold ───────────────────────────────────────────────────────────────

pub fn settle_hold(
    ports: &FinancePorts,
    authority_id: &str,
    input: SettleHoldInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.hold_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.settlement_id)
        || !valid_finance_hash(&input.usage_hash)
    {
        return Err(denied("invalid settlement identifiers or usageHash"));
    }
    let settlement_marker = FinanceMarker::Settlement {
        authority: authority_id.to_owned(),
        settlement_id: input.settlement_id.clone(),
    };
    let previous_hold_id = ports.ledger.marker(&settlement_marker)?;
    if !previous_hold_id.is_empty() {
        if previous_hold_id != input.hold_id {
            return Err(denied("settlement id already used for another hold"));
        }
        let hold = get_finance_hold(ports.ledger, &input.hold_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "hold": hold}));
    }
    let mut hold = get_finance_hold(ports.ledger, &input.hold_id)?;
    if hold.get("status").and_then(Value::as_str) != Some("running") {
        return Err(denied("hold is not running"));
    }
    if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("payer does not match hold"));
    }
    if hold.get("quoteId").and_then(Value::as_str) != Some(input.quote_id.as_str()) {
        return Err(denied("quote does not match hold"));
    }
    if hold.get("runId").and_then(Value::as_str) != Some(input.settlement_id.as_str()) {
        return Err(denied("settlement does not match authorized run"));
    }
    if hold.get("settlementAuthority").and_then(Value::as_str) != Some(authority_id) {
        return Err(denied("caller is not the settlement authority"));
    }
    let expires_at = as_i64(hold.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    if expires_at <= 0 || now > expires_at {
        return Err(denied("hold expired"));
    }
    let max_amount = as_i64(hold.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(0);
    if max_amount <= 0 {
        return Err(denied("invalid hold amount"));
    }
    let beneficiaries = hold
        .get("beneficiaries")
        .and_then(Value::as_array)
        .ok_or_else(|| denied("hold beneficiaries missing"))?;
    let mut caps = std::collections::HashMap::<String, i64>::new();
    for item in beneficiaries {
        let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        let cap = as_i64(item.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(0);
        if user_id.is_empty() || role.is_empty() || cap <= 0 {
            return Err(denied("invalid hold beneficiary"));
        }
        caps.insert(format!("{user_id}|{role}"), cap);
    }
    let mut actual_amount = 0_i64;
    let mut allocated = std::collections::HashMap::<String, i64>::new();
    let mut credits = std::collections::HashMap::<String, i64>::new();
    for line in &input.lines {
        if line.amount <= 0
            || !valid_finance_id(&line.user_id)
            || !valid_finance_id(&line.role)
            || line.source_ref.len() > 256
        {
            return Err(denied("invalid settlement line"));
        }
        let cap_key = format!("{}|{}", line.user_id, line.role);
        let Some(cap) = caps.get(&cap_key) else {
            return Err(denied("settlement beneficiary role not authorized by hold"));
        };
        actual_amount = actual_amount.checked_add(line.amount).ok_or_else(|| denied("settlement amount overflow"))?;
        let role_total = allocated.entry(cap_key).or_insert(0);
        *role_total = role_total.checked_add(line.amount).ok_or_else(|| denied("beneficiary role amount overflow"))?;
        if *role_total > *cap {
            return Err(denied("settlement exceeds beneficiary role cap"));
        }
        let credited = credits.entry(line.user_id.clone()).or_insert(0);
        *credited = credited.checked_add(line.amount).ok_or_else(|| denied("beneficiary amount overflow"))?;
    }
    if actual_amount > max_amount {
        return Err(denied("settlement exceeds hold"));
    }
    let refund_amount = max_amount.checked_sub(actual_amount).ok_or_else(|| denied("refund underflow"))?;
    let project_id = hold.get("projectId").and_then(Value::as_str).unwrap_or("").to_string();
    finalize_project_budget(ports, &project_id, max_amount, actual_amount, now)?;

    add_finance_counter(ports.ledger, WalletCounter::Spent, &input.payer_user_id, actual_amount)?;
    let mut participants = vec![input.payer_user_id.clone(), authority_id.to_owned()];
    let mut wallet_credits = std::collections::HashMap::<String, i64>::new();
    let mut debt_repays = std::collections::HashMap::<String, i64>::new();
    for (user_id, amount) in &credits {
        add_finance_counter(ports.ledger, WalletCounter::Earned, user_id, *amount)?;
        let Some(mut receiver) = ports.account(user_id)? else {
            return Err(denied("settlement beneficiary not found"));
        };
        let debt = finance_counter(ports.ledger, WalletCounter::Debt, user_id)?;
        let debt_repaid = debt.min(*amount);
        let wallet_credit = amount.checked_sub(debt_repaid).ok_or_else(|| denied("beneficiary credit underflow"))?;
        receiver.balance = receiver.balance.checked_add(wallet_credit).ok_or_else(|| denied("beneficiary balance overflow"))?;
        let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id)?
            .checked_add(wallet_credit)
            .ok_or_else(|| denied("withdrawable earnings overflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Debt, user_id, debt - debt_repaid)?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id, withdrawable)?;
        wallet_credits.insert(user_id.clone(), wallet_credit);
        debt_repays.insert(user_id.clone(), debt_repaid);
        ports.store_account(&receiver)?;
        participants.push(user_id.clone());
    }
    let Some(mut payer) = ports.account(&input.payer_user_id)? else {
        return Err(denied("payer creature not found"));
    };
    payer.balance = payer.balance.checked_add(refund_amount).ok_or_else(|| denied("payer balance overflow"))?;
    let held_withdrawable = as_i64(hold.get("withdrawableAmount").unwrap_or(&Value::Null)).unwrap_or(0);
    let withdrawable_refund = refund_amount.min(held_withdrawable);
    let withdrawable_spent = held_withdrawable.checked_sub(withdrawable_refund).ok_or_else(|| denied("withdrawable settlement underflow"))?;
    let payer_withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.payer_user_id)?
        .checked_add(withdrawable_refund)
        .ok_or_else(|| denied("withdrawable refund overflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.payer_user_id, payer_withdrawable)?;
    ports.store_account(&payer)?;
    let held = finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id)?
        .checked_sub(max_amount)
        .ok_or_else(|| denied("held balance underflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id, held)?;

    hold.insert("status".to_string(), json!("settled"));
    hold.insert("remainingAmount".to_string(), json!(0));
    hold.insert("actualAmount".to_string(), json!(actual_amount));
    hold.insert("refundedAmount".to_string(), json!(refund_amount));
    hold.insert("withdrawableRefundedAmount".to_string(), json!(withdrawable_refund));
    hold.insert("withdrawableSpentAmount".to_string(), json!(withdrawable_spent));
    hold.insert("settlementId".to_string(), json!(input.settlement_id));
    hold.insert("usageHash".to_string(), json!(input.usage_hash));
    hold.insert("settlementLines".to_string(), serde_json::to_value(&input.lines).map_err(|e| failed(e.to_string()))?);
    hold.insert("finalizedAt".to_string(), json!(now));
    put_finance_hold(ports.ledger, &input.hold_id, &hold)?;
    ports.ledger.put_marker(&settlement_marker, &input.hold_id)?;

    let mut entries = vec![
        json!({"account": format!("wallet:{}:held", input.payer_user_id), "amount": -max_amount}),
        json!({"account": format!("wallet:{}:available", input.payer_user_id), "amount": refund_amount}),
    ];
    for (user_id, gross_amount) in &credits {
        let wallet_credit = wallet_credits.get(user_id).copied().unwrap_or(0);
        let debt_repaid = debt_repays.get(user_id).copied().unwrap_or(0);
        entries.push(json!({
            "account": format!("wallet:{user_id}:available"),
            "amount": wallet_credit,
            "grossAmount": gross_amount,
        }));
        if debt_repaid > 0 {
            entries.push(json!({
                "account": format!("wallet:{user_id}:debt"),
                "amount": -debt_repaid,
            }));
        }
    }
    let journal_id = write_finance_journal(
        ports.ledger,
        "hold.settled",
        &input.hold_id,
        &input.payer_user_id,
        json!({
            "entries": entries,
            "quoteId": input.quote_id,
            "settlementId": input.settlement_id,
            "usageHash": input.usage_hash,
            "actualAmount": actual_amount,
            "refundedAmount": refund_amount,
            "settlementLines": input.lines,
        }),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "hold": hold, "journalId": journal_id}))
}

// ── release_hold ──────────────────────────────────────────────────────────────

pub fn release_hold(
    ports: &FinancePorts,
    caller_id: &str,
    input: ReleaseHoldInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.hold_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.release_id)
        || input.reason.len() > 256
    {
        return Err(denied("invalid release request"));
    }
    let release_marker = FinanceMarker::Release {
        caller: caller_id.to_owned(),
        release_id: input.release_id.clone(),
    };
    let previous_hold_id = ports.ledger.marker(&release_marker)?;
    if !previous_hold_id.is_empty() {
        if previous_hold_id != input.hold_id {
            return Err(denied("release id already used for another hold"));
        }
        let hold = get_finance_hold(ports.ledger, &input.hold_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "hold": hold}));
    }
    let mut hold = get_finance_hold(ports.ledger, &input.hold_id)?;
    let active_status = hold.get("status").and_then(Value::as_str).unwrap_or("");
    if active_status != "open" && active_status != "running" {
        return Err(denied("hold is not active"));
    }
    if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("payer does not match hold"));
    }
    let authority = hold.get("settlementAuthority").and_then(Value::as_str).unwrap_or("").to_string();
    let expires_at = as_i64(hold.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    let payer_open_release = caller_id == input.payer_user_id && active_status == "open";
    let payer_expired_release = caller_id == input.payer_user_id && now >= expires_at;
    if caller_id != authority && !payer_open_release && !payer_expired_release {
        return Err(denied("only the authority may release an active hold"));
    }
    let max_amount = as_i64(hold.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(0);
    if max_amount <= 0 {
        return Err(denied("invalid hold amount"));
    }
    let project_id = hold.get("projectId").and_then(Value::as_str).unwrap_or("").to_string();
    finalize_project_budget(ports, &project_id, max_amount, 0, now)?;
    let Some(mut payer) = ports.account(&input.payer_user_id)? else {
        return Err(denied("payer creature not found"));
    };
    payer.balance = payer.balance.checked_add(max_amount).ok_or_else(|| denied("payer balance overflow"))?;
    let withdrawable_refund = as_i64(hold.get("withdrawableAmount").unwrap_or(&Value::Null)).unwrap_or(0);
    let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.payer_user_id)?
        .checked_add(withdrawable_refund)
        .ok_or_else(|| denied("withdrawable refund overflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.payer_user_id, withdrawable)?;
    ports.store_account(&payer)?;
    let held = finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id)?
        .checked_sub(max_amount)
        .ok_or_else(|| denied("held balance underflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id, held)?;

    let status = if now >= expires_at { "expired" } else { "released" };
    hold.insert("status".to_string(), json!(status));
    hold.insert("remainingAmount".to_string(), json!(0));
    hold.insert("refundedAmount".to_string(), json!(max_amount));
    hold.insert("withdrawableRefundedAmount".to_string(), json!(withdrawable_refund));
    hold.insert("releaseId".to_string(), json!(input.release_id));
    hold.insert("releaseReason".to_string(), json!(input.reason));
    hold.insert("finalizedAt".to_string(), json!(now));
    put_finance_hold(ports.ledger, &input.hold_id, &hold)?;
    ports.ledger.put_marker(&release_marker, &input.hold_id)?;

    let participants = vec![input.payer_user_id.clone(), authority];
    let journal_id = write_finance_journal(
        ports.ledger,
        "hold.released",
        &input.hold_id,
        &input.payer_user_id,
        json!({
            "entries": [
                {"account": format!("wallet:{}:held", input.payer_user_id), "amount": -max_amount},
                {"account": format!("wallet:{}:available", input.payer_user_id), "amount": max_amount}
            ],
            "status": status,
            "reason": input.reason,
        }),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "hold": hold, "journalId": journal_id}))
}

// ── get_hold ──────────────────────────────────────────────────────────────────

pub fn get_hold(
    ports: &FinancePorts,
    caller_id: &str,
    input: GetHoldInput,
) -> Result<Value, ApplicationError> {
    if !valid_finance_id(&input.hold_id) {
        return Err(denied("invalid hold id"));
    }
    let hold = get_finance_hold(ports.ledger, &input.hold_id)?;
    let payer_id = hold.get("payerUserId").and_then(Value::as_str).unwrap_or("");
    if !input.payer_user_id.is_empty() && input.payer_user_id != payer_id {
        return Err(denied("payer does not match hold"));
    }
    let authority = hold.get("settlementAuthority").and_then(Value::as_str).unwrap_or("");
    let is_beneficiary = hold
        .get("beneficiaries")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|item| item.get("userId").and_then(Value::as_str) == Some(caller_id))
        })
        .unwrap_or(false);
    if caller_id != payer_id && caller_id != authority && !is_beneficiary {
        return Err(denied("access denied"));
    }
    Ok(json!({"hold": hold}))
}

// ── get_financial_account ─────────────────────────────────────────────────────

pub fn get_financial_account(
    ports: &FinancePorts,
    caller_id: &str,
    input: GetFinancialAccountInput,
) -> Result<Value, ApplicationError> {
    let user_id = if input.user_id.is_empty() {
        caller_id.to_owned()
    } else {
        input.user_id.clone()
    };
    if !valid_finance_id(&user_id) {
        return Err(denied("invalid financial account id"));
    }
    if user_id != caller_id && caller_id != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    let limit = if input.limit <= 0 { 50 } else { input.limit.min(100) as usize };
    financial_account_snapshot(ports, &user_id, limit)
}

// ── request_payout ────────────────────────────────────────────────────────────

pub fn request_payout(
    ports: &FinancePorts,
    user_id: &str,
    input: RequestPayoutInput,
) -> Result<Value, ApplicationError> {
    let destination = input.destination_ref.trim();
    if !valid_finance_id(&input.request_id)
        || input.amount <= 0
        || destination.is_empty()
        || destination.len() > 256
        || destination.chars().any(char::is_control)
    {
        return Err(denied("invalid payout request"));
    }
    let request_hash = finance_hash(&serde_json::to_value(&input).map_err(|e| failed(e.to_string()))?)?;
    let marker = FinanceMarker::PayoutRequest {
        user: user_id.to_owned(),
        request_id: input.request_id.clone(),
    };
    let previous = ports.ledger.marker(&marker)?;
    if !previous.is_empty() {
        let Some((payout_id, previous_hash)) = previous.split_once(char::from(124)) else {
            return Err(denied("invalid payout idempotency record"));
        };
        if previous_hash != request_hash {
            return Err(denied("requestId already used with different payout data"));
        }
        return Ok(json!({"applied": false, "alreadyApplied": true, "payout": get_finance_payout(ports.ledger, payout_id)?}));
    }
    if finance_counter(ports.ledger, WalletCounter::Debt, user_id)? > 0 {
        return Err(denied("wallet has outstanding payment debt"));
    }
    let Some(mut creature) = ports.account(user_id)? else {
        return Err(denied("financial account not found"));
    };
    let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id)?;
    if input.amount > withdrawable || input.amount > creature.balance {
        return Err(denied("withdrawable earnings are not enough"));
    }
    creature.balance = creature.balance.checked_sub(input.amount).ok_or_else(|| denied("wallet payout underflow"))?;
    let next_withdrawable = withdrawable.checked_sub(input.amount).ok_or_else(|| denied("withdrawable payout underflow"))?;
    let payout_held = finance_counter(ports.ledger, WalletCounter::PayoutHeld, user_id)?
        .checked_add(input.amount)
        .ok_or_else(|| denied("payout held overflow"))?;
    let now = ports.clock.unix_millis();
    let payout_id = ports.ledger.gen_id();
    let payout = json!({
        "payoutId": payout_id,
        "requestId": input.request_id,
        "userId": user_id,
        "amount": input.amount,
        "destinationRef": destination,
        "status": "pending",
        "createdAt": now,
        "requestHash": request_hash,
    });
    let payout_map = payout.as_object().cloned().ok_or_else(|| denied("invalid payout record"))?;
    ports.store_account(&creature)?;
    set_finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id, next_withdrawable)?;
    set_finance_counter(ports.ledger, WalletCounter::PayoutHeld, user_id, payout_held)?;
    put_finance_payout(ports.ledger, &payout_id, &payout_map)?;
    ports.ledger.put_marker(&marker, &format!("{payout_id}|{request_hash}"))?;
    let participants = vec![user_id.to_owned()];
    let journal_id = write_finance_journal(
        ports.ledger,
        "payout.requested",
        "",
        user_id,
        json!({
            "entries": [
                {"account": format!("wallet:{user_id}:available"), "amount": -input.amount},
                {"account": format!("wallet:{user_id}:payout_held"), "amount": input.amount}
            ],
            "payoutId": payout_id,
            "destinationRef": destination,
        }),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "payout": payout_map, "journalId": journal_id}))
}

// ── resolve_payout ────────────────────────────────────────────────────────────

pub fn resolve_payout(
    ports: &FinancePorts,
    caller: &str,
    input: ResolvePayoutInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    if !valid_finance_id(&input.payout_id)
        || !valid_finance_id(&input.resolution_id)
        || (input.status != "paid" && input.status != "rejected")
        || input.provider_reference.len() > 256
        || input.provider_reference.chars().any(char::is_control)
        || input.reason.len() > 256
        || input.reason.chars().any(char::is_control)
        || (input.status == "paid" && input.provider_reference.trim().is_empty())
    {
        return Err(denied("invalid payout resolution"));
    }
    let request_hash = finance_hash(&serde_json::to_value(&input).map_err(|e| failed(e.to_string()))?)?;
    let marker = FinanceMarker::PayoutResolution {
        resolution_id: input.resolution_id.clone(),
    };
    let previous = ports.ledger.marker(&marker)?;
    if !previous.is_empty() {
        let Some((payout_id, previous_hash)) = previous.split_once(char::from(124)) else {
            return Err(denied("invalid payout resolution idempotency record"));
        };
        if payout_id != input.payout_id || previous_hash != request_hash {
            return Err(denied("resolutionId already used with different payout data"));
        }
        return Ok(json!({"applied": false, "alreadyApplied": true, "payout": get_finance_payout(ports.ledger, payout_id)?}));
    }
    let mut payout = get_finance_payout(ports.ledger, &input.payout_id)?;
    if payout.get("status").and_then(Value::as_str) != Some("pending") {
        return Err(denied("payout is not pending"));
    }
    let user_id = payout.get("userId").and_then(Value::as_str).unwrap_or("").to_string();
    let amount = as_i64(payout.get("amount").unwrap_or(&Value::Null)).unwrap_or(0);
    if user_id.is_empty() || amount <= 0 {
        return Err(denied("invalid payout record"));
    }
    let payout_held = finance_counter(ports.ledger, WalletCounter::PayoutHeld, &user_id)?
        .checked_sub(amount)
        .ok_or_else(|| denied("payout held underflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::PayoutHeld, &user_id, payout_held)?;
    let mut entries = vec![json!({"account": format!("wallet:{user_id}:payout_held"), "amount": -amount})];
    if input.status == "rejected" {
        let Some(mut creature) = ports.account(&user_id)? else {
            return Err(denied("payout owner not found"));
        };
        creature.balance = creature.balance.checked_add(amount).ok_or_else(|| denied("payout refund overflow"))?;
        let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &user_id)?
            .checked_add(amount)
            .ok_or_else(|| denied("withdrawable payout refund overflow"))?;
        ports.store_account(&creature)?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &user_id, withdrawable)?;
        entries.push(json!({"account": format!("wallet:{user_id}:available"), "amount": amount}));
    } else {
        entries.push(json!({"account": "external:payouts", "amount": amount}));
    }
    let now = ports.clock.unix_millis();
    payout.insert("status".to_string(), json!(input.status));
    payout.insert("providerReference".to_string(), json!(input.provider_reference));
    payout.insert("reason".to_string(), json!(input.reason));
    payout.insert("resolutionId".to_string(), json!(input.resolution_id));
    payout.insert("resolvedAt".to_string(), json!(now));
    put_finance_payout(ports.ledger, &input.payout_id, &payout)?;
    ports.ledger.put_marker(&marker, &format!("{}|{request_hash}", input.payout_id))?;
    let participants = vec![user_id.clone(), caller.to_owned()];
    let journal_id = write_finance_journal(
        ports.ledger,
        &format!("payout.{}", input.status),
        "",
        &user_id,
        json!({
            "entries": entries,
            "payoutId": input.payout_id,
            "providerReference": input.provider_reference,
            "reason": input.reason,
        }),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "payout": payout, "journalId": journal_id}))
}

// ── list_payouts ──────────────────────────────────────────────────────────────

pub fn list_payouts(
    ports: &FinancePorts,
    caller: &str,
    input: ListPayoutsInput,
) -> Result<Value, ApplicationError> {
    let limit = if input.limit <= 0 { 50_usize } else { input.limit.min(200) as usize };
    if input.user_id.is_empty() {
        if caller != LEGACY_ROOT {
            return Ok(json!({"payouts": finance_payout_records(ports, caller, limit)?}));
        }
        let mut payouts: Vec<Value> = Vec::new();
        for payout_id in ports.ledger.doc_ids(FinanceDoc::Payout)? {
            if let Ok(payout) = get_finance_payout(ports.ledger, &payout_id) {
                payouts.push(Value::Object(payout));
            }
        }
        payouts.sort_by(|a, b| {
            as_i64(b.get("createdAt").unwrap_or(&Value::Null)).unwrap_or(0)
                .cmp(&as_i64(a.get("createdAt").unwrap_or(&Value::Null)).unwrap_or(0))
        });
        payouts.truncate(limit);
        return Ok(json!({"payouts": payouts}));
    }
    if input.user_id != caller && caller != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    Ok(json!({"payouts": finance_payout_records(ports, &input.user_id, limit)?}))
}

// ── open_pool ─────────────────────────────────────────────────────────────────

pub fn open_pool(
    ports: &FinancePorts,
    payer_id: &str,
    input: OpenPoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.settlement_authority)
        || !valid_finance_id(&input.meter_program_id)
        || !valid_finance_id(&input.idempotency_key)
    {
        return Err(denied("invalid authority, meter, or idempotency identifier"));
    }
    if input.max_amount <= 0 {
        return Err(denied("maxAmount must be greater than zero"));
    }
    if input.expires_at <= now {
        return Err(denied("pool expiry must be in the future"));
    }
    let marker = FinanceMarker::PoolOpen {
        payer: payer_id.to_owned(),
        key: input.idempotency_key.clone(),
    };
    let existing = ports.ledger.marker(&marker)?;
    if !existing.is_empty() {
        let pool = get_finance_pool(ports.ledger, &existing)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
    }
    if finance_counter(ports.ledger, WalletCounter::Debt, payer_id)? > 0 {
        return Err(denied("wallet has outstanding payment debt"));
    }
    let Some(mut payer) = ports.account(payer_id)? else {
        return Err(denied("payer creature not found"));
    };
    let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, payer_id)?;
    if withdrawable > payer.balance {
        return Err(denied("withdrawable balance exceeds available balance"));
    }
    let withdrawable_amount = withdrawable_debit_portion(payer.balance, withdrawable, input.max_amount);
    payer.balance = payer
        .balance
        .checked_sub(input.max_amount)
        .ok_or_else(|| denied("insufficient available balance to open this pool"))?;
    set_finance_counter(
        ports.ledger,
        WalletCounter::Withdrawable,
        payer_id,
        withdrawable
            .checked_sub(withdrawable_amount)
            .ok_or_else(|| denied("withdrawable composition underflow"))?,
    )?;
    ports.store_account(&payer)?;
    let held = finance_counter(ports.ledger, WalletCounter::Held, payer_id)?
        .checked_add(input.max_amount)
        .ok_or_else(|| denied("held balance overflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Held, payer_id, held)?;

    let pool_id = ports.ledger.gen_id();
    let pool = json!({
        "version": 1,
        "poolId": pool_id,
        "payerUserId": payer_id,
        "maxAmount": input.max_amount,
        "remaining": input.max_amount,
        "reserved": 0,
        "spent": 0,
        "refunded": 0,
        "withdrawableAmount": withdrawable_amount,
        "settlementAuthority": input.settlement_authority,
        "meterProgramId": input.meter_program_id,
        "status": "open",
        "expiresAt": input.expires_at,
        "idempotencyKey": input.idempotency_key,
        "createdAt": now,
        "updatedAt": now,
    });
    let pool_map = pool.as_object().cloned().ok_or_else(|| denied("pool encode failed"))?;
    put_finance_pool(ports.ledger, &pool_id, &pool_map)?;
    ports.ledger.put_marker(&marker, &pool_id)?;
    ports.ledger.put_pool_of_user(payer_id, &pool_id)?;
    let participants = vec![payer_id.to_owned(), input.settlement_authority.clone()];
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.opened",
        &pool_id,
        payer_id,
        json!({"maxAmount": input.max_amount, "withdrawableAmount": withdrawable_amount}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "pool": pool, "journalId": journal_id}))
}

// ── refresh_pool ──────────────────────────────────────────────────────────────

pub fn refresh_pool(
    ports: &FinancePorts,
    payer_id: &str,
    input: RefreshPoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id) || !valid_finance_id(&input.refresh_id) {
        return Err(denied("invalid pool or refresh identifier"));
    }
    if input.amount <= 0 {
        return Err(denied("refresh amount must be greater than zero"));
    }
    let marker = FinanceMarker::PoolRefresh {
        payer: payer_id.to_owned(),
        refresh_id: input.refresh_id.clone(),
    };
    if !ports.ledger.marker(&marker)?.is_empty() {
        let pool = get_finance_pool(ports.ledger, &input.pool_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
    }
    if finance_counter(ports.ledger, WalletCounter::Debt, payer_id)? > 0 {
        return Err(denied("wallet has outstanding payment debt"));
    }
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    if pool.get("payerUserId").and_then(Value::as_str) != Some(payer_id) {
        return Err(denied("pool does not belong to caller"));
    }
    if pool.get("status").and_then(Value::as_str) != Some("open") {
        return Err(denied("pool is not open"));
    }
    let Some(mut payer) = ports.account(payer_id)? else {
        return Err(denied("payer creature not found"));
    };
    let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, payer_id)?;
    if withdrawable > payer.balance {
        return Err(denied("withdrawable balance exceeds available balance"));
    }
    let withdrawable_add = withdrawable_debit_portion(payer.balance, withdrawable, input.amount);
    payer.balance = payer
        .balance
        .checked_sub(input.amount)
        .ok_or_else(|| denied("insufficient available balance to refresh this pool"))?;
    set_finance_counter(
        ports.ledger,
        WalletCounter::Withdrawable,
        payer_id,
        withdrawable
            .checked_sub(withdrawable_add)
            .ok_or_else(|| denied("withdrawable composition underflow"))?,
    )?;
    ports.store_account(&payer)?;
    let held = finance_counter(ports.ledger, WalletCounter::Held, payer_id)?
        .checked_add(input.amount)
        .ok_or_else(|| denied("held balance overflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Held, payer_id, held)?;

    let max_amount = as_i64(pool.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(input.amount)
        .ok_or_else(|| denied("pool maxAmount overflow"))?;
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(input.amount)
        .ok_or_else(|| denied("pool remaining overflow"))?;
    let pool_withdrawable = as_i64(pool.get("withdrawableAmount").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(withdrawable_add)
        .ok_or_else(|| denied("pool withdrawable overflow"))?;
    if input.expires_at > now {
        pool.insert("expiresAt".to_string(), json!(input.expires_at));
    }
    pool.insert("maxAmount".to_string(), json!(max_amount));
    pool.insert("remaining".to_string(), json!(remaining));
    pool.insert("withdrawableAmount".to_string(), json!(pool_withdrawable));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    ports.ledger.put_marker(&marker, &input.pool_id)?;
    let authority = pool.get("settlementAuthority").and_then(Value::as_str).unwrap_or("").to_string();
    let participants = vec![payer_id.to_owned(), authority];
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.refreshed",
        &input.pool_id,
        payer_id,
        json!({"amount": input.amount, "maxAmount": max_amount}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "pool": Value::Object(pool), "journalId": journal_id}))
}

// ── close_pool ────────────────────────────────────────────────────────────────

pub fn close_pool(
    ports: &FinancePorts,
    caller_id: &str,
    input: ClosePoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id) || !valid_finance_id(&input.close_id) {
        return Err(denied("invalid pool or close identifier"));
    }
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    let payer_id = pool.get("payerUserId").and_then(Value::as_str).unwrap_or("").to_string();
    let authority = pool.get("settlementAuthority").and_then(Value::as_str).unwrap_or("").to_string();
    if caller_id != payer_id && caller_id != authority {
        return Err(denied("caller may not close this pool"));
    }
    let status = pool.get("status").and_then(Value::as_str).unwrap_or("");
    let close_marker = FinanceMarker::PoolClose {
        pool_id: input.pool_id.clone(),
    };
    if status == "closed" {
        if ports.ledger.marker(&close_marker)? == input.close_id {
            return Ok(json!({"applied": false, "alreadyApplied": true, "pool": Value::Object(pool)}));
        }
        return Err(denied("pool is already closed"));
    }
    if status != "open" {
        return Err(denied("pool is not open"));
    }
    let reserved = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(0);
    if reserved != 0 {
        return Err(denied("pool has in-flight run reservations; cannot close yet"));
    }
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0);
    let pool_withdrawable = as_i64(pool.get("withdrawableAmount").unwrap_or(&Value::Null)).unwrap_or(0);
    let withdrawable_refund = remaining.min(pool_withdrawable);
    if remaining > 0 {
        let Some(mut payer) = ports.account(&payer_id)? else {
            return Err(denied("payer creature not found"));
        };
        payer.balance = payer.balance.checked_add(remaining).ok_or_else(|| denied("payer balance overflow"))?;
        let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &payer_id)?
            .checked_add(withdrawable_refund)
            .ok_or_else(|| denied("withdrawable refund overflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &payer_id, withdrawable)?;
        ports.store_account(&payer)?;
        let held = finance_counter(ports.ledger, WalletCounter::Held, &payer_id)?
            .checked_sub(remaining)
            .ok_or_else(|| denied("held balance underflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Held, &payer_id, held)?;
    }
    let refunded = as_i64(pool.get("refunded").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(remaining)
        .ok_or_else(|| denied("pool refunded overflow"))?;
    pool.insert("status".to_string(), json!("closed"));
    pool.insert("refunded".to_string(), json!(refunded));
    pool.insert("remaining".to_string(), json!(0));
    pool.insert("closeId".to_string(), json!(input.close_id));
    pool.insert("closeReason".to_string(), json!(input.reason));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    ports.ledger.put_marker(&close_marker, &input.close_id)?;
    let participants = vec![payer_id.clone(), authority];
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.closed",
        &input.pool_id,
        &payer_id,
        json!({"refunded": remaining, "withdrawableRefunded": withdrawable_refund}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "pool": Value::Object(pool), "journalId": journal_id}))
}

// ── reserve_pool ──────────────────────────────────────────────────────────────

pub fn reserve_pool(
    ports: &FinancePorts,
    authority_id: &str,
    input: ReservePoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.run_id)
    {
        return Err(denied("invalid pool, payer, quote, or run identifier"));
    }
    if input.max_amount <= 0 {
        return Err(denied("reservation amount must be greater than zero"));
    }
    if let Ok(existing) = ports
        .ledger
        .get_doc(FinanceDoc::PoolReservation, &input.run_id, "reservation")
    {
        if existing.get("poolId").and_then(Value::as_str) == Some(input.pool_id.as_str()) {
            return Ok(json!({"applied": false, "alreadyApplied": true, "reservation": existing}));
        }
        return Err(denied("run already reserved against another pool"));
    }
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    if pool.get("settlementAuthority").and_then(Value::as_str) != Some(authority_id) {
        return Err(denied("caller is not this pool's settlement authority"));
    }
    if pool.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("payer does not match pool"));
    }
    if pool.get("status").and_then(Value::as_str) != Some("open") {
        return Err(denied("pool is not open"));
    }
    let expires_at = as_i64(pool.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    if expires_at <= 0 || now > expires_at {
        return Err(denied("pool expired"));
    }
    let quote = billing_quote(ports, &input.quote_id)?;
    if quote.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("quote payer does not match reservation"));
    }
    if quote.get("requestId").and_then(Value::as_str) != Some(input.run_id.as_str()) {
        return Err(denied("quote is not bound to this run"));
    }
    if as_i64(quote.get("maxAmount").unwrap_or(&Value::Null)) != Some(input.max_amount) {
        return Err(denied("reservation amount does not match the quote"));
    }
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0);
    if remaining < input.max_amount {
        return Err(denied("pool has insufficient remaining balance for this run"));
    }
    let reserved = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(input.max_amount)
        .ok_or_else(|| denied("pool reserved overflow"))?;
    pool.insert("remaining".to_string(), json!(remaining - input.max_amount));
    pool.insert("reserved".to_string(), json!(reserved));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    let reservation = json!({
        "runId": input.run_id,
        "poolId": input.pool_id,
        "payerUserId": input.payer_user_id,
        "quoteId": input.quote_id,
        "amount": input.max_amount,
        "status": "reserved",
        "createdAt": now,
    });
    put_finance_pool_reservation(ports.ledger, &input.run_id, reservation.as_object().unwrap())?;
    let participants = vec![input.payer_user_id.clone(), authority_id.to_owned()];
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.reserved",
        &input.pool_id,
        &input.payer_user_id,
        json!({"runId": input.run_id, "amount": input.max_amount}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "reservation": reservation, "journalId": journal_id}))
}

// ── settle_pool ───────────────────────────────────────────────────────────────

pub fn settle_pool(
    ports: &FinancePorts,
    authority_id: &str,
    input: SettlePoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.run_id)
        || !valid_finance_id(&input.settlement_id)
        || !valid_finance_hash(&input.usage_hash)
    {
        return Err(denied("invalid settlement identifiers or usageHash"));
    }
    let settlement_marker = FinanceMarker::PoolSettlement {
        authority: authority_id.to_owned(),
        settlement_id: input.settlement_id.clone(),
    };
    if !ports.ledger.marker(&settlement_marker)?.is_empty() {
        let pool = get_finance_pool(ports.ledger, &input.pool_id)?;
        return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
    }
    let mut reservation = get_finance_pool_reservation(ports.ledger, &input.run_id)?;
    if reservation.get("status").and_then(Value::as_str) != Some("reserved") {
        return Err(denied("run reservation is not open for settlement"));
    }
    if reservation.get("poolId").and_then(Value::as_str) != Some(input.pool_id.as_str())
        || reservation.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
        || reservation.get("quoteId").and_then(Value::as_str) != Some(input.quote_id.as_str())
    {
        return Err(denied("settlement does not match the run reservation"));
    }
    let slice = as_i64(reservation.get("amount").unwrap_or(&Value::Null)).unwrap_or(0);
    if slice <= 0 {
        return Err(denied("invalid reservation amount"));
    }
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    if pool.get("settlementAuthority").and_then(Value::as_str) != Some(authority_id) {
        return Err(denied("caller is not this pool's settlement authority"));
    }
    if pool.get("status").and_then(Value::as_str) != Some("open") {
        return Err(denied("pool is not open"));
    }
    let quote = billing_quote(ports, &input.quote_id)?;
    if quote.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("quote payer does not match settlement"));
    }
    let beneficiaries = quote
        .get("beneficiaries")
        .and_then(Value::as_array)
        .ok_or_else(|| denied("quote beneficiaries missing"))?;
    let mut caps = std::collections::HashMap::<String, i64>::new();
    for item in beneficiaries {
        let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        let cap = as_i64(item.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(0);
        if user_id.is_empty() || role.is_empty() || cap <= 0 {
            return Err(denied("invalid quote beneficiary"));
        }
        caps.insert(format!("{user_id}|{role}"), cap);
    }
    let mut actual_amount = 0_i64;
    let mut allocated = std::collections::HashMap::<String, i64>::new();
    let mut credits = std::collections::HashMap::<String, i64>::new();
    for line in &input.lines {
        if line.amount <= 0
            || !valid_finance_id(&line.user_id)
            || !valid_finance_id(&line.role)
            || line.source_ref.len() > 256
        {
            return Err(denied("invalid settlement line"));
        }
        if line.user_id == input.payer_user_id {
            return Err(denied("payer cannot be a settlement beneficiary"));
        }
        let cap_key = format!("{}|{}", line.user_id, line.role);
        let Some(cap) = caps.get(&cap_key) else {
            return Err(denied("settlement beneficiary role not authorized by quote"));
        };
        actual_amount = actual_amount.checked_add(line.amount).ok_or_else(|| denied("settlement amount overflow"))?;
        let role_total = allocated.entry(cap_key).or_insert(0);
        *role_total = role_total.checked_add(line.amount).ok_or_else(|| denied("beneficiary role amount overflow"))?;
        if *role_total > *cap {
            return Err(denied("settlement exceeds beneficiary role cap"));
        }
        let credited = credits.entry(line.user_id.clone()).or_insert(0);
        *credited = credited.checked_add(line.amount).ok_or_else(|| denied("beneficiary amount overflow"))?;
    }
    if actual_amount > slice {
        return Err(denied("settlement exceeds the run reservation"));
    }
    let mut participants = vec![input.payer_user_id.clone(), authority_id.to_owned()];
    for (user_id, amount) in &credits {
        add_finance_counter(ports.ledger, WalletCounter::Earned, user_id, *amount)?;
        let Some(mut receiver) = ports.account(user_id)? else {
            return Err(denied("settlement beneficiary not found"));
        };
        let debt = finance_counter(ports.ledger, WalletCounter::Debt, user_id)?;
        let debt_repaid = debt.min(*amount);
        let wallet_credit = amount.checked_sub(debt_repaid).ok_or_else(|| denied("beneficiary credit underflow"))?;
        receiver.balance = receiver.balance.checked_add(wallet_credit).ok_or_else(|| denied("beneficiary balance overflow"))?;
        let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id)?
            .checked_add(wallet_credit)
            .ok_or_else(|| denied("withdrawable earnings overflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Debt, user_id, debt - debt_repaid)?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id, withdrawable)?;
        ports.store_account(&receiver)?;
        participants.push(user_id.clone());
    }
    let refund_to_pool = slice.checked_sub(actual_amount).ok_or_else(|| denied("reservation refund underflow"))?;
    if actual_amount > 0 {
        let held = finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id)?
            .checked_sub(actual_amount)
            .ok_or_else(|| denied("held balance underflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id, held)?;
        add_finance_counter(ports.ledger, WalletCounter::Spent, &input.payer_user_id, actual_amount)?;
    }
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(refund_to_pool)
        .ok_or_else(|| denied("pool remaining overflow"))?;
    let reserved = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_sub(slice)
        .ok_or_else(|| denied("pool reserved underflow"))?;
    let spent = as_i64(pool.get("spent").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(actual_amount)
        .ok_or_else(|| denied("pool spent overflow"))?;
    pool.insert("remaining".to_string(), json!(remaining));
    pool.insert("reserved".to_string(), json!(reserved));
    pool.insert("spent".to_string(), json!(spent));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    reservation.insert("status".to_string(), json!("settled"));
    reservation.insert("settlementId".to_string(), json!(input.settlement_id));
    reservation.insert("actualAmount".to_string(), json!(actual_amount));
    reservation.insert("usageHash".to_string(), json!(input.usage_hash));
    reservation.insert("settlementLines".to_string(), serde_json::to_value(&input.lines).map_err(|e| failed(e.to_string()))?);
    reservation.insert("settledAt".to_string(), json!(now));
    put_finance_pool_reservation(ports.ledger, &input.run_id, &reservation)?;
    ports.ledger.put_marker(&settlement_marker, &input.run_id)?;
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.settled",
        &input.pool_id,
        &input.payer_user_id,
        json!({"runId": input.run_id, "actual": actual_amount, "refundedToPool": refund_to_pool}),
        &participants,
        now,
    )?;
    Ok(json!({
        "applied": true,
        "actualAmount": actual_amount,
        "refundedToPool": refund_to_pool,
        "journalId": journal_id,
    }))
}

// ── release_pool ──────────────────────────────────────────────────────────────

pub fn release_pool(
    ports: &FinancePorts,
    caller_id: &str,
    input: ReleasePoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.run_id)
        || !valid_finance_id(&input.release_id)
    {
        return Err(denied("invalid pool, payer, run, or release identifier"));
    }
    let mut reservation = get_finance_pool_reservation(ports.ledger, &input.run_id)?;
    let status = reservation.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "released"
        && reservation.get("releaseId").and_then(Value::as_str) == Some(input.release_id.as_str())
    {
        return Ok(json!({"applied": false, "alreadyApplied": true, "reservation": reservation}));
    }
    if status != "reserved" {
        return Err(denied("run reservation is not open for release"));
    }
    if reservation.get("poolId").and_then(Value::as_str) != Some(input.pool_id.as_str())
        || reservation.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
    {
        return Err(denied("release does not match the run reservation"));
    }
    let slice = as_i64(reservation.get("amount").unwrap_or(&Value::Null)).unwrap_or(0);
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    let authority = pool.get("settlementAuthority").and_then(Value::as_str).unwrap_or("").to_string();
    let expires_at = as_i64(pool.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    let payer_recovery = caller_id == input.payer_user_id && expires_at > 0 && now > expires_at;
    if caller_id != authority && !payer_recovery {
        return Err(denied("caller may not release this reservation"));
    }
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(slice)
        .ok_or_else(|| denied("pool remaining overflow"))?;
    let reserved = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_sub(slice)
        .ok_or_else(|| denied("pool reserved underflow"))?;
    pool.insert("remaining".to_string(), json!(remaining));
    pool.insert("reserved".to_string(), json!(reserved));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    reservation.insert("status".to_string(), json!("released"));
    reservation.insert("releaseId".to_string(), json!(input.release_id));
    reservation.insert("releaseReason".to_string(), json!(input.reason));
    reservation.insert("releasedAt".to_string(), json!(now));
    put_finance_pool_reservation(ports.ledger, &input.run_id, &reservation)?;
    let participants = vec![input.payer_user_id.clone(), authority];
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.released",
        &input.pool_id,
        &input.payer_user_id,
        json!({"runId": input.run_id, "amount": slice}),
        &participants,
        now,
    )?;
    Ok(json!({"applied": true, "amount": slice, "journalId": journal_id}))
}

// ── debit_pool ────────────────────────────────────────────────────────────────

pub fn debit_pool(
    ports: &FinancePorts,
    authority_id: &str,
    input: DebitPoolInput,
) -> Result<Value, ApplicationError> {
    let now = ports.clock.unix_millis();
    if !valid_finance_id(&input.pool_id)
        || !valid_finance_id(&input.payer_user_id)
        || !valid_finance_id(&input.quote_id)
        || !valid_finance_id(&input.run_id)
        || !valid_finance_id(&input.debit_id)
        || !valid_finance_hash(&input.usage_hash)
    {
        return Err(denied("invalid debit identifiers or usageHash"));
    }
    let debit_marker = FinanceMarker::PoolDebit {
        authority: authority_id.to_owned(),
        debit_id: input.debit_id.clone(),
    };
    if !ports.ledger.marker(&debit_marker)?.is_empty() {
        let pool = get_finance_pool(ports.ledger, &input.pool_id)?;
        let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0);
        return Ok(json!({"applied": false, "alreadyApplied": true, "remaining": remaining}));
    }
    let mut pool = get_finance_pool(ports.ledger, &input.pool_id)?;
    if pool.get("settlementAuthority").and_then(Value::as_str) != Some(authority_id) {
        return Err(denied("caller is not this pool's settlement authority"));
    }
    if pool.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("payer does not match pool"));
    }
    if pool.get("status").and_then(Value::as_str) != Some("open") {
        return Err(denied("pool is not open"));
    }
    let expires_at = as_i64(pool.get("expiresAt").unwrap_or(&Value::Null)).unwrap_or(0);
    if expires_at <= 0 || now > expires_at {
        return Err(denied("pool expired"));
    }
    let quote = billing_quote(ports, &input.quote_id)?;
    if quote.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str()) {
        return Err(denied("quote payer does not match debit"));
    }
    let beneficiaries = quote
        .get("beneficiaries")
        .and_then(Value::as_array)
        .ok_or_else(|| denied("quote beneficiaries missing"))?;
    let mut authorized = std::collections::HashMap::<String, bool>::new();
    for item in beneficiaries {
        let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        if user_id.is_empty() || role.is_empty() {
            return Err(denied("invalid quote beneficiary"));
        }
        authorized.insert(format!("{user_id}|{role}"), true);
    }
    let mut delta = 0_i64;
    let mut credits = std::collections::HashMap::<String, i64>::new();
    let mut user_credits = std::collections::HashMap::<String, i64>::new();
    for line in &input.lines {
        if line.amount <= 0
            || !valid_finance_id(&line.user_id)
            || !valid_finance_id(&line.role)
            || line.source_ref.len() > 256
        {
            return Err(denied("invalid debit line"));
        }
        if line.user_id == input.payer_user_id {
            return Err(denied("payer cannot be a debit beneficiary"));
        }
        let cap_key = format!("{}|{}", line.user_id, line.role);
        if !authorized.contains_key(&cap_key) {
            return Err(denied("debit beneficiary role not authorized by quote"));
        }
        delta = delta.checked_add(line.amount).ok_or_else(|| denied("debit amount overflow"))?;
        let credited = credits.entry(cap_key).or_insert(0);
        *credited = credited.checked_add(line.amount).ok_or_else(|| denied("beneficiary amount overflow"))?;
        let user_credited = user_credits.entry(line.user_id.clone()).or_insert(0);
        *user_credited = user_credited.checked_add(line.amount).ok_or_else(|| denied("beneficiary amount overflow"))?;
    }
    if delta <= 0 {
        return Err(denied("debit must charge a positive amount"));
    }
    let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(0);
    if remaining < delta {
        return Ok(json!({
            "applied": false,
            "exhausted": true,
            "remaining": remaining,
            "charged": 0,
        }));
    }
    let mut participants = vec![input.payer_user_id.clone(), authority_id.to_owned()];
    for (user_id, amount) in &user_credits {
        if user_id.is_empty() {
            return Err(denied("invalid credit beneficiary"));
        }
        add_finance_counter(ports.ledger, WalletCounter::Earned, user_id, *amount)?;
        let Some(mut receiver) = ports.account(user_id)? else {
            return Err(denied("debit beneficiary not found"));
        };
        let debt = finance_counter(ports.ledger, WalletCounter::Debt, user_id)?;
        let debt_repaid = debt.min(*amount);
        let wallet_credit = amount.checked_sub(debt_repaid).ok_or_else(|| denied("beneficiary credit underflow"))?;
        receiver.balance = receiver.balance.checked_add(wallet_credit).ok_or_else(|| denied("beneficiary balance overflow"))?;
        let withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id)?
            .checked_add(wallet_credit)
            .ok_or_else(|| denied("withdrawable earnings overflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Debt, user_id, debt - debt_repaid)?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id, withdrawable)?;
        ports.store_account(&receiver)?;
        participants.push(user_id.clone());
    }
    let held = finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id)?
        .checked_sub(delta)
        .ok_or_else(|| denied("held balance underflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Held, &input.payer_user_id, held)?;
    add_finance_counter(ports.ledger, WalletCounter::Spent, &input.payer_user_id, delta)?;
    let new_remaining = remaining.checked_sub(delta).ok_or_else(|| denied("pool remaining underflow"))?;
    let spent = as_i64(pool.get("spent").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(delta)
        .ok_or_else(|| denied("pool spent overflow"))?;
    pool.insert("remaining".to_string(), json!(new_remaining));
    pool.insert("spent".to_string(), json!(spent));
    pool.insert("updatedAt".to_string(), json!(now));
    put_finance_pool(ports.ledger, &input.pool_id, &pool)?;
    let mut record = ports
        .ledger
        .get_doc(FinanceDoc::LiveDebit, &input.run_id, "debit")
        .unwrap_or_default();
    if record.is_empty() {
        record.insert("runId".to_string(), json!(input.run_id));
        record.insert("poolId".to_string(), json!(input.pool_id));
        record.insert("payerUserId".to_string(), json!(input.payer_user_id));
        record.insert("quoteId".to_string(), json!(input.quote_id));
        record.insert("createdAt".to_string(), json!(now));
    }
    let charged_total = as_i64(record.get("chargedTotal").unwrap_or(&Value::Null)).unwrap_or(0)
        .checked_add(delta)
        .ok_or_else(|| denied("run charged total overflow"))?;
    record.insert("chargedTotal".to_string(), json!(charged_total));
    let mut record_credits = record
        .get("credits")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (cap_key, amount) in &credits {
        let prior = as_i64(record_credits.get(cap_key).unwrap_or(&Value::Null)).unwrap_or(0)
            .checked_add(*amount)
            .ok_or_else(|| denied("run credit overflow"))?;
        record_credits.insert(cap_key.clone(), json!(prior));
    }
    record.insert("credits".to_string(), Value::Object(record_credits));
    record.insert("lastDebitId".to_string(), json!(input.debit_id));
    record.insert("lastUsageHash".to_string(), json!(input.usage_hash));
    record.insert("updatedAt".to_string(), json!(now));
    put_finance_live_debit(ports.ledger, &input.run_id, &record)?;
    ports.ledger.put_marker(&debit_marker, &input.run_id)?;
    let journal_id = write_finance_journal(
        ports.ledger,
        "pool.debited",
        &input.pool_id,
        &input.payer_user_id,
        json!({"runId": input.run_id, "amount": delta, "remaining": new_remaining}),
        &participants,
        now,
    )?;
    Ok(json!({
        "applied": true,
        "charged": delta,
        "remaining": new_remaining,
        "journalId": journal_id,
    }))
}

// ── reconcile_financial_system ────────────────────────────────────────────────

pub fn reconcile_financial_system(
    ports: &FinancePorts,
    caller: &str,
    input: ReconcileFinancialSystemInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    let max_issues = if input.max_issues <= 0 {
        100_usize
    } else {
        input.max_issues.min(1000) as usize
    };
    let mut issues: Vec<Value> = Vec::new();
    let mut report = |code: &str, reference: &str, detail: String| {
        if issues.len() < max_issues {
            issues.push(json!({"code": code, "reference": reference, "detail": detail}));
        }
    };
    let mut held_expected = std::collections::HashMap::<String, i64>::new();
    let mut project_reserved_expected = std::collections::HashMap::<String, i64>::new();
    let mut project_spent_expected = std::collections::HashMap::<String, i64>::new();
    let mut spent_expected = std::collections::HashMap::<String, i64>::new();
    let mut earned_expected = std::collections::HashMap::<String, i64>::new();
    let mut hold_count = 0_i64;
    let mut active_hold_count = 0_i64;
    for hold_id in ports.ledger.doc_ids(FinanceDoc::Hold)? {
        let Ok(hold) = ports.ledger.get_doc(FinanceDoc::Hold, &hold_id, "hold") else {
            report("hold.unreadable", &hold_id, "hold JSON cannot be read".to_string());
            continue;
        };
        hold_count += 1;
        let payer = hold.get("payerUserId").and_then(Value::as_str).unwrap_or("");
        let project = hold.get("projectId").and_then(Value::as_str).unwrap_or("");
        let status = hold.get("status").and_then(Value::as_str).unwrap_or("");
        let max_amount = as_i64(hold.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
        let remaining = as_i64(hold.get("remainingAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
        if payer.is_empty() || max_amount <= 0 {
            report("hold.invalid", &hold_id, "payer or maxAmount is invalid".to_string());
            continue;
        }
        match status {
            "open" | "running" => {
                active_hold_count += 1;
                if remaining != max_amount {
                    report(
                        "hold.remaining_mismatch",
                        &hold_id,
                        format!("remaining={remaining}, max={max_amount}"),
                    );
                }
                if !finance_map_add(&mut held_expected, payer, max_amount) {
                    report("held.overflow", payer, "expected held balance overflow".to_string());
                }
                if !project.is_empty()
                    && !finance_map_add(&mut project_reserved_expected, project, max_amount)
                {
                    report("project.reserved_overflow", project, "expected reservation overflow".to_string());
                }
            }
            "settled" => {
                let actual = as_i64(hold.get("actualAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
                let refunded = as_i64(hold.get("refundedAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
                if remaining != 0
                    || actual < 0
                    || refunded < 0
                    || actual.checked_add(refunded) != Some(max_amount)
                {
                    report(
                        "hold.settlement_mismatch",
                        &hold_id,
                        format!("actual={actual}, refunded={refunded}, max={max_amount}, remaining={remaining}"),
                    );
                    continue;
                }
                let mut line_total = 0_i64;
                for line in hold
                    .get("settlementLines")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let user_id = line.get("userId").and_then(Value::as_str).unwrap_or("");
                    let amount = as_i64(line.get("amount").unwrap_or(&Value::Null)).unwrap_or(-1);
                    if amount <= 0 || !finance_map_add(&mut earned_expected, user_id, amount) {
                        report("settlement.line_invalid", &hold_id, "invalid beneficiary settlement line".to_string());
                        continue;
                    }
                    line_total = line_total.checked_add(amount).unwrap_or(i64::MAX);
                }
                if line_total != actual {
                    report("settlement.lines_mismatch", &hold_id, format!("lines={line_total}, actual={actual}"));
                }
                if !finance_map_add(&mut spent_expected, payer, actual) {
                    report("spent.overflow", payer, "expected spent counter overflow".to_string());
                }
                if !project.is_empty()
                    && !finance_map_add(&mut project_spent_expected, project, actual)
                {
                    report("project.spent_overflow", project, "expected project spend overflow".to_string());
                }
            }
            "released" | "expired" => {
                let refunded = as_i64(hold.get("refundedAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
                if remaining != 0 || refunded != max_amount {
                    report(
                        "hold.release_mismatch",
                        &hold_id,
                        format!("refunded={refunded}, max={max_amount}, remaining={remaining}"),
                    );
                }
            }
            _ => report("hold.status_invalid", &hold_id, format!("status={status}")),
        }
    }
    for pool_id in ports.ledger.doc_ids(FinanceDoc::Pool)? {
        let Ok(pool) = ports.ledger.get_doc(FinanceDoc::Pool, &pool_id, "pool") else {
            report("pool.unreadable", &pool_id, "pool JSON cannot be read".to_string());
            continue;
        };
        let payer = pool.get("payerUserId").and_then(Value::as_str).unwrap_or("");
        let status = pool.get("status").and_then(Value::as_str).unwrap_or("");
        let max_amount = as_i64(pool.get("maxAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
        let remaining = as_i64(pool.get("remaining").unwrap_or(&Value::Null)).unwrap_or(-1);
        let reserved = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(-1);
        let spent = as_i64(pool.get("spent").unwrap_or(&Value::Null)).unwrap_or(-1);
        let refunded = as_i64(pool.get("refunded").unwrap_or(&Value::Null)).unwrap_or(-1);
        if payer.is_empty()
            || max_amount < 0
            || remaining < 0
            || reserved < 0
            || spent < 0
            || refunded < 0
        {
            report("pool.invalid", &pool_id, "payer or pool amounts are invalid".to_string());
            continue;
        }
        let sum = remaining
            .checked_add(reserved)
            .and_then(|v| v.checked_add(spent))
            .and_then(|v| v.checked_add(refunded));
        if sum != Some(max_amount) {
            report(
                "pool.balance_mismatch",
                &pool_id,
                format!("remaining={remaining}, reserved={reserved}, spent={spent}, refunded={refunded}, max={max_amount}"),
            );
        }
        if status == "open"
            && !finance_map_add(&mut held_expected, payer, remaining.saturating_add(reserved))
        {
            report("held.overflow", payer, "expected held (pool) overflow".to_string());
        }
    }
    let mut pool_reserved_expected = std::collections::HashMap::<String, i64>::new();
    for run_id in ports.ledger.doc_ids(FinanceDoc::PoolReservation)? {
        let Ok(reservation) = ports.ledger.get_doc(FinanceDoc::PoolReservation, &run_id, "reservation") else {
            report("reservation.unreadable", &run_id, "reservation JSON cannot be read".to_string());
            continue;
        };
        let payer = reservation.get("payerUserId").and_then(Value::as_str).unwrap_or("");
        let pool_id = reservation.get("poolId").and_then(Value::as_str).unwrap_or("");
        let status = reservation.get("status").and_then(Value::as_str).unwrap_or("");
        let amount = as_i64(reservation.get("amount").unwrap_or(&Value::Null)).unwrap_or(-1);
        if payer.is_empty() || pool_id.is_empty() || amount < 0 {
            report("reservation.invalid", &run_id, "reservation fields are invalid".to_string());
            continue;
        }
        match status {
            "reserved" => {
                if !finance_map_add(&mut pool_reserved_expected, pool_id, amount) {
                    report("reservation.overflow", pool_id, "expected pool reserved overflow".to_string());
                }
            }
            "settled" => {
                let actual = as_i64(reservation.get("actualAmount").unwrap_or(&Value::Null)).unwrap_or(-1);
                if actual < 0 {
                    report("reservation.settlement_invalid", &run_id, "settled reservation missing actualAmount".to_string());
                    continue;
                }
                let mut line_total = 0_i64;
                for line in reservation
                    .get("settlementLines")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let user_id = line.get("userId").and_then(Value::as_str).unwrap_or("");
                    let line_amount = as_i64(line.get("amount").unwrap_or(&Value::Null)).unwrap_or(-1);
                    if line_amount <= 0 || !finance_map_add(&mut earned_expected, user_id, line_amount) {
                        report("reservation.line_invalid", &run_id, "invalid pool settlement line".to_string());
                        continue;
                    }
                    line_total = line_total.checked_add(line_amount).unwrap_or(i64::MAX);
                }
                if line_total != actual {
                    report("reservation.lines_mismatch", &run_id, format!("lines={line_total}, actual={actual}"));
                }
                if !finance_map_add(&mut spent_expected, payer, actual) {
                    report("spent.overflow", payer, "expected spent (pool) overflow".to_string());
                }
            }
            "released" => {}
            _ => report("reservation.status_invalid", &run_id, format!("status={status}")),
        }
    }
    for run_id in ports.ledger.doc_ids(FinanceDoc::LiveDebit)? {
        let Ok(record) = ports.ledger.get_doc(FinanceDoc::LiveDebit, &run_id, "debit") else {
            report("livedebit.unreadable", &run_id, "live debit JSON cannot be read".to_string());
            continue;
        };
        let payer = record.get("payerUserId").and_then(Value::as_str).unwrap_or("");
        let charged = as_i64(record.get("chargedTotal").unwrap_or(&Value::Null)).unwrap_or(-1);
        if payer.is_empty() || charged < 0 {
            report("livedebit.invalid", &run_id, "live debit fields are invalid".to_string());
            continue;
        }
        let mut credit_total = 0_i64;
        if let Some(credits) = record.get("credits").and_then(Value::as_object) {
            for (cap_key, value) in credits {
                let user_id = cap_key.split('|').next().unwrap_or("");
                let amount = value.as_i64().unwrap_or(-1);
                if user_id.is_empty() || amount <= 0 || !finance_map_add(&mut earned_expected, user_id, amount) {
                    report("livedebit.credit_invalid", &run_id, "invalid live debit credit".to_string());
                    continue;
                }
                credit_total = credit_total.checked_add(amount).unwrap_or(i64::MAX);
            }
        }
        if credit_total != charged {
            report("livedebit.credit_mismatch", &run_id, format!("credits={credit_total}, charged={charged}"));
        }
        if !finance_map_add(&mut spent_expected, payer, charged) {
            report("spent.overflow", payer, "expected spent (live debit) overflow".to_string());
        }
    }
    for pool_id in ports.ledger.doc_ids(FinanceDoc::Pool)? {
        let Ok(pool) = ports.ledger.get_doc(FinanceDoc::Pool, &pool_id, "pool") else {
            continue;
        };
        let stored = as_i64(pool.get("reserved").unwrap_or(&Value::Null)).unwrap_or(0);
        let expected = pool_reserved_expected.get(&pool_id).copied().unwrap_or(0);
        if stored != expected {
            report("pool.reserved_mismatch", &pool_id, format!("stored={stored}, expected={expected}"));
        }
    }
    let mut held_actual = std::collections::HashMap::<String, i64>::new();
    for (payer, raw) in ports.ledger.counter_links(WalletCounter::Held)? {
        match raw.parse::<i64>() {
            Ok(value) if value >= 0 => {
                held_actual.insert(payer.clone(), value);
            }
            _ => report("held.invalid", &payer, format!("stored={raw}")),
        }
    }
    let mut held_users: Vec<String> = held_expected
        .keys()
        .chain(held_actual.keys())
        .cloned()
        .collect();
    held_users.sort();
    held_users.dedup();
    for payer in held_users {
        let expected = held_expected.get(&payer).copied().unwrap_or(0);
        let actual = held_actual.get(&payer).copied().unwrap_or(0);
        if actual != expected {
            report("held.mismatch", &payer, format!("stored={actual}, expected={expected}"));
        }
    }
    let mut payout_held_expected = std::collections::HashMap::<String, i64>::new();
    let mut payout_count = 0_i64;
    let mut pending_payout_count = 0_i64;
    for payout_id in ports.ledger.doc_ids(FinanceDoc::Payout)? {
        let Ok(payout) = get_finance_payout(ports.ledger, &payout_id) else {
            report("payout.unreadable", &payout_id, "payout JSON cannot be read".to_string());
            continue;
        };
        payout_count += 1;
        if payout.get("status").and_then(Value::as_str) == Some("pending") {
            pending_payout_count += 1;
            let user_id = payout.get("userId").and_then(Value::as_str).unwrap_or("");
            let amount = as_i64(payout.get("amount").unwrap_or(&Value::Null)).unwrap_or(-1);
            if amount <= 0 || !finance_map_add(&mut payout_held_expected, user_id, amount) {
                report("payout.invalid", &payout_id, "pending payout owner or amount is invalid".to_string());
            }
        }
    }
    let mut payout_held_actual = std::collections::HashMap::<String, i64>::new();
    for (user_id, raw) in ports.ledger.counter_links(WalletCounter::PayoutHeld)? {
        match raw.parse::<i64>() {
            Ok(value) if value >= 0 => {
                payout_held_actual.insert(user_id.clone(), value);
            }
            _ => report("payout.held_invalid", &user_id, "stored payout held amount is invalid".to_string()),
        }
    }
    let mut payout_users: Vec<String> = payout_held_expected
        .keys()
        .chain(payout_held_actual.keys())
        .cloned()
        .collect();
    payout_users.sort();
    payout_users.dedup();
    for user_id in payout_users {
        let expected = payout_held_expected.get(&user_id).copied().unwrap_or(0);
        let actual = payout_held_actual.get(&user_id).copied().unwrap_or(0);
        if actual != expected {
            report("payout.held_mismatch", &user_id, format!("stored={actual}, expected={expected}"));
        }
    }
    let mut total_withdrawable_actual: i64 = 0;
    for (user_id, raw) in ports.ledger.counter_links(WalletCounter::Withdrawable)? {
        let withdrawable = raw.parse::<i64>().unwrap_or(-1);
        let available = ports.account(&user_id)?.map(|account| account.balance);
        if withdrawable < 0 || available.is_none_or(|available| withdrawable > available) {
            report(
                "withdrawable.invalid",
                &user_id,
                format!("withdrawable={withdrawable}, available={}", available.unwrap_or_default()),
            );
        }
        if withdrawable > 0 {
            total_withdrawable_actual = total_withdrawable_actual.saturating_add(withdrawable);
        }
    }
    let total_earned_expected: i64 = earned_expected
        .values()
        .fold(0_i64, |acc, v| acc.saturating_add(*v));
    if total_withdrawable_actual > total_earned_expected {
        report(
            "withdrawable.unbacked_total",
            "",
            format!("withdrawable_total={total_withdrawable_actual}, earned_total={total_earned_expected}"),
        );
    }
    for (kind, expected, code) in [
        (WalletCounter::Spent, &spent_expected, "spent.mismatch"),
        (WalletCounter::Earned, &earned_expected, "earned.mismatch"),
    ] {
        let mut actual = std::collections::HashMap::<String, i64>::new();
        for (user_id, raw) in ports.ledger.counter_links(kind)? {
            match raw.parse::<i64>() {
                Ok(value) if value >= 0 => {
                    actual.insert(user_id.clone(), value);
                }
                _ => report(code, &user_id, "stored counter is invalid".to_string()),
            }
        }
        let mut users: Vec<String> = expected.keys().chain(actual.keys()).cloned().collect();
        users.sort();
        users.dedup();
        for user_id in users {
            let expected_value = expected.get(&user_id).copied().unwrap_or(0);
            let actual_value = actual.get(&user_id).copied().unwrap_or(0);
            if actual_value != expected_value {
                report(code, &user_id, format!("stored={actual_value}, expected={expected_value}"));
            }
        }
    }
    let mut projects: Vec<String> = project_reserved_expected
        .keys()
        .chain(project_spent_expected.keys())
        .cloned()
        .collect();
    for project in ports.ledger.doc_ids(FinanceDoc::ProjectBudget)? {
        projects.push(project.clone());
    }
    projects.sort();
    projects.dedup();
    for project in projects {
        let state = ports
            .ledger
            .get_doc(FinanceDoc::ProjectBudget, &project, "budget")
            .unwrap_or_default();
        let stored_reserved = as_i64(state.get("reservedMinor").unwrap_or(&Value::Null)).unwrap_or(0);
        let stored_spent = as_i64(state.get("spentMinor").unwrap_or(&Value::Null)).unwrap_or(0);
        let expected_reserved = project_reserved_expected.get(&project).copied().unwrap_or(0);
        let expected_spent = project_spent_expected.get(&project).copied().unwrap_or(0);
        if stored_reserved != expected_reserved {
            report(
                "project.reserved_mismatch",
                &project,
                format!("stored={stored_reserved}, expected={expected_reserved}"),
            );
        }
        if stored_spent < expected_spent {
            report(
                "project.spent_undercount",
                &project,
                format!("stored={stored_spent}, minimum={expected_spent}"),
            );
        }
    }
    let issue_count = issues.len();
    Ok(json!({
        "healthy": issue_count == 0,
        "checkedAt": ports.clock.unix_millis(),
        "holdCount": hold_count,
        "activeHoldCount": active_hold_count,
        "payoutCount": payout_count,
        "pendingPayoutCount": pending_payout_count,
        "payerCount": held_expected.len(),
        "projectCount": project_reserved_expected.keys().chain(project_spent_expected.keys()).collect::<std::collections::HashSet<_>>().len(),
        "issueCount": issue_count,
        "issues": issues,
    }))
}

// ── payment_adjustment ────────────────────────────────────────────────────────

pub fn payment_adjustment(
    ports: &FinancePorts,
    caller: &str,
    input: PaymentAdjustmentInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    if !valid_finance_id(&input.user_id)
        || !valid_finance_id(&input.kind)
        || !valid_finance_id(&input.idempotency_key)
        || input.reference.is_empty()
        || input.reference.len() > 256
        || input.amount == 0
    {
        return Err(denied("invalid payment adjustment"));
    }
    let allowed = matches!(
        input.kind.as_str(),
        "refund" | "chargeback" | "dispute" | "manual_debit" | "manual_credit"
    );
    if !allowed || (input.amount > 0 && input.kind != "manual_credit") {
        return Err(denied("unsupported payment adjustment kind"));
    }
    if serde_json::to_vec(&input.metadata).map_err(|e| failed(e.to_string()))?.len() > 4096 {
        return Err(denied("payment adjustment metadata is too large"));
    }
    let request_hash = finance_hash(&serde_json::to_value(&input).map_err(|e| failed(e.to_string()))?)?;
    let marker = FinanceMarker::PaymentAdjustment {
        key: input.idempotency_key.clone(),
    };
    let previous = ports.ledger.marker(&marker)?;
    if !previous.is_empty() {
        let Some((previous_hash, journal_id)) = previous.split_once('|') else {
            return Err(denied("invalid payment adjustment idempotency record"));
        };
        if previous_hash != request_hash {
            return Err(denied("idempotency key already used with different adjustment"));
        }
        return Ok(json!({
            "applied": false, "alreadyApplied": true, "journalId": journal_id,
            "account": financial_account_snapshot(ports, &input.user_id, 20)?,
        }));
    }
    let Some(mut creature) = ports.account(&input.user_id)? else {
        return Err(denied("payment adjustment target not found"));
    };
    let old_debt = finance_counter(ports.ledger, WalletCounter::Debt, &input.user_id)?;
    let old_withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.user_id)?;
    if old_withdrawable > creature.balance {
        return Err(denied("withdrawable balance exceeds available balance"));
    }
    let (wallet_delta, debt_delta) = if input.amount < 0 {
        let reversal = input.amount.checked_abs().ok_or_else(|| denied("payment adjustment overflow"))?;
        let available_debit = creature.balance.min(reversal);
        let debt_added = reversal.checked_sub(available_debit).ok_or_else(|| denied("payment adjustment underflow"))?;
        creature.balance = creature.balance.checked_sub(available_debit).ok_or_else(|| denied("wallet adjustment underflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &input.user_id, old_withdrawable.min(creature.balance))?;
        set_finance_counter(ports.ledger, WalletCounter::Debt, &input.user_id, old_debt.checked_add(debt_added).ok_or_else(|| denied("wallet debt overflow"))?)?;
        (-available_debit, debt_added)
    } else {
        let debt_repaid = old_debt.min(input.amount);
        let wallet_credit = input.amount.checked_sub(debt_repaid).ok_or_else(|| denied("payment adjustment underflow"))?;
        creature.balance = creature.balance.checked_add(wallet_credit).ok_or_else(|| denied("wallet balance overflow"))?;
        set_finance_counter(ports.ledger, WalletCounter::Debt, &input.user_id, old_debt - debt_repaid)?;
        (wallet_credit, -debt_repaid)
    };
    ports.store_account(&creature)?;
    let participants = vec![input.user_id.clone(), caller.to_owned()];
    let now = ports.clock.unix_millis();
    let journal_id = write_finance_journal(
        ports.ledger,
        &format!("payment.{}", input.kind),
        "",
        &input.user_id,
        json!({
            "entries": [
                {"account": format!("wallet:{}:available", input.user_id), "amount": wallet_delta},
                {"account": format!("wallet:{}:debt", input.user_id), "amount": debt_delta},
                {"account": "external:payments", "amount": -input.amount}
            ],
            "adjustmentAmount": input.amount, "kind": input.kind,
            "reference": input.reference, "metadata": input.metadata,
        }),
        &participants,
        now,
    )?;
    ports.ledger.put_marker(&marker, &format!("{request_hash}|{journal_id}"))?;
    Ok(json!({
        "applied": true, "journalId": journal_id,
        "account": financial_account_snapshot(ports, &input.user_id, 20)?,
    }))
}
// ── transfer (finance.transfer) ───────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TransferInput {
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "toUsername", default)]
    pub to_username: String,
}

pub fn transfer(
    ports: &FinancePorts,
    from_id: &str,
    input: TransferInput,
) -> Result<Value, ApplicationError> {
    if input.amount <= 0 {
        return Err(denied("amount must be greater than zero"));
    }
    let Some(mut from) = ports.account(from_id)? else {
        return Err(denied("sender creature not found"));
    };
    if from.balance < input.amount {
        return Err(denied("your balance is not enough"));
    }
    let Some(to_id) = ports.creatures.creature_id_by_username(&input.to_username)? else {
        return Err(denied("target creature not found"));
    };
    if to_id == from.id {
        return Err(denied("cannot transfer to the same wallet"));
    }
    let Some(mut to) = ports.account(&to_id)? else {
        return Err(denied("target creature not found"));
    };
    let from_withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &from.id)?;
    if from_withdrawable > from.balance {
        return Err(denied("withdrawable balance exceeds available balance"));
    }
    let nonwithdrawable = from.balance - from_withdrawable;
    let sent_withdrawable = input.amount.saturating_sub(nonwithdrawable);
    from.balance = from
        .balance
        .checked_sub(input.amount)
        .ok_or_else(|| denied("sender balance underflow"))?;
    set_finance_counter(
        ports.ledger,
        WalletCounter::Withdrawable,
        &from.id,
        from_withdrawable
            .checked_sub(sent_withdrawable)
            .ok_or_else(|| denied("sender withdrawable underflow"))?,
    )?;

    let debt = finance_counter(ports.ledger, WalletCounter::Debt, &to.id)?;
    let debt_repaid = debt.min(input.amount);
    let wallet_credit = input.amount - debt_repaid;
    let sent_nonwithdrawable = input.amount - sent_withdrawable;
    let withdrawable_used_for_debt = debt_repaid.saturating_sub(sent_nonwithdrawable);
    let received_withdrawable = sent_withdrawable
        .checked_sub(withdrawable_used_for_debt)
        .ok_or_else(|| denied("target withdrawable underflow"))?;
    to.balance = to
        .balance
        .checked_add(wallet_credit)
        .ok_or_else(|| denied("target balance overflow"))?;
    let to_withdrawable = finance_counter(ports.ledger, WalletCounter::Withdrawable, &to.id)?
        .checked_add(received_withdrawable)
        .ok_or_else(|| denied("target withdrawable overflow"))?;
    set_finance_counter(ports.ledger, WalletCounter::Debt, &to.id, debt - debt_repaid)?;
    set_finance_counter(ports.ledger, WalletCounter::Withdrawable, &to.id, to_withdrawable)?;
    ports.store_account(&from)?;
    ports.store_account(&to)?;
    let now = ports.clock.unix_millis();
    let journal_id = write_finance_journal(
        ports.ledger,
        "wallet.transfer",
        "",
        &from.id,
        json!({
            "entries": [
                {"account": format!("wallet:{}:available", from.id), "amount": -input.amount},
                {"account": format!("wallet:{}:available", to.id), "amount": wallet_credit},
                {"account": format!("wallet:{}:debt", to.id), "amount": -debt_repaid}
            ],
            "amount": input.amount,
            "withdrawableAmount": sent_withdrawable,
            "debtRepaid": debt_repaid,
        }),
        &[from.id.clone(), to.id.clone()],
        now,
    )?;
    Ok(json!({
        "amount": input.amount,
        "toUserId": to.id,
        "debtRepaid": debt_repaid,
        "journalId": journal_id,
    }))
}

// ── mint (finance.mint) ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MintInput {
    #[serde(rename = "toUserEmail", default)]
    pub to_user_email: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
}

pub fn mint(
    ports: &FinancePorts,
    caller: &str,
    input: MintInput,
) -> Result<Value, ApplicationError> {
    if caller != LEGACY_ROOT {
        return Err(denied("access denied"));
    }
    if input.amount <= 0 {
        return Err(denied("amount must be greater than zero"));
    }
    let marker = match input.idempotency_key.trim() {
        "" => None,
        key => Some(format!("MintApplied::{key}")),
    };
    if let Some(marker) = &marker {
        let applied = ports.ledger.marker(&FinanceMarker::MintApplied {
            key: marker.clone(),
        })?;
        if !applied.is_empty() {
            return Ok(json!({
                "applied": false,
                "alreadyApplied": true,
                "previous": applied,
            }));
        }
    }
    let to_user_id = ports.ledger.email_to_id(&input.to_user_email)?;
    if to_user_id.is_empty() {
        return Err(denied("target user not found"));
    }
    let Some(mut creature) = ports.account(&to_user_id)? else {
        return Err(denied("target user not found"));
    };
    let debt = finance_counter(ports.ledger, WalletCounter::Debt, &creature.id)?;
    let debt_repaid = debt.min(input.amount);
    let wallet_credit = input
        .amount
        .checked_sub(debt_repaid)
        .ok_or_else(|| denied("mint credit underflow"))?;
    creature.balance = creature
        .balance
        .checked_add(wallet_credit)
        .ok_or_else(|| denied("balance overflow"))?;
    ports.store_account(&creature)?;
    set_finance_counter(ports.ledger, WalletCounter::Debt, &creature.id, debt - debt_repaid)?;
    let target_id = creature.id.clone();
    let participants = vec![target_id.clone(), caller.to_owned()];
    let journal_id = write_finance_journal(
        ports.ledger,
        "payment.credited",
        "",
        &target_id,
        json!({
            "entries": [
                {"account": "external:payments", "amount": -input.amount},
                {"account": format!("wallet:{target_id}:available"), "amount": wallet_credit},
                {"account": format!("wallet:{target_id}:debt"), "amount": -debt_repaid}
            ],
            "grossAmount": input.amount, "walletCredit": wallet_credit,
            "debtRepaid": debt_repaid, "paymentReference": input.idempotency_key,
        }),
        &participants,
        ports.clock.unix_millis(),
    )?;
    if let Some(marker) = &marker {
        ports.ledger.put_marker(
            &FinanceMarker::MintApplied { key: marker.clone() },
            &format!("{}:{}:{}", target_id, input.amount, journal_id),
        )?;
    }
    Ok(json!({
        "applied": true, "balance": creature.balance,
        "walletCredit": wallet_credit, "debtRepaid": debt_repaid,
        "debt": debt - debt_repaid, "journalId": journal_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::program::ProgramRecord;
    use aseman_domain::store::StoreRecord;
    use aseman_domain::store_permissions::StorePermissions;
    use aseman_ports::PortResult;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        docs: Mutex<BTreeMap<(FinanceDoc, String, String), Value>>,
        counters: Mutex<BTreeMap<(WalletCounter, String), i64>>,
        markers: Mutex<BTreeMap<String, String>>,
        index: Mutex<BTreeMap<String, Vec<(String, String)>>>,
        creatures: Mutex<BTreeMap<String, i64>>,
        usernames: Mutex<BTreeMap<String, String>>,
        stores: Mutex<BTreeMap<String, StoreRecord>>,
        metadata: Mutex<BTreeMap<String, String>>,
        members: Mutex<BTreeMap<(String, String), StorePermissions>>,
        programs: Mutex<BTreeMap<String, ProgramRecord>>,
        emails: Mutex<BTreeMap<String, String>>,
        id_emails: Mutex<BTreeMap<String, String>>,
        counter: Mutex<u64>,
        clock: Mutex<i64>,
    }

    impl Memory {
        fn clock(&self, at_millis: i64) {
            *self.clock.lock().unwrap() = at_millis;
        }
    }

    impl FinanceLedger for Memory {
        fn get_doc(&self, family: FinanceDoc, id: &str, path: &str) -> PortResult<Map<String, Value>> {
            self.docs
                .lock()
                .unwrap()
                .get(&(family, id.to_owned(), path.to_owned()))
                .and_then(Value::as_object)
                .cloned()
                .ok_or(PortError::NotFound)
        }
        fn put_doc(&self, family: FinanceDoc, id: &str, path: &str, value: &Value, merge: bool) -> PortResult<()> {
            let mut target = if merge {
                self.get_doc(family, id, path).unwrap_or_default()
            } else {
                Map::new()
            };
            if let Some(obj) = value.as_object() {
                for (k, v) in obj {
                    target.insert(k.clone(), v.clone());
                }
            }
            self.docs
                .lock()
                .unwrap()
                .insert((family, id.to_owned(), path.to_owned()), Value::Object(target));
            Ok(())
        }
        fn doc_ids(&self, family: FinanceDoc) -> PortResult<Vec<String>> {
            let docs = self.docs.lock().unwrap();
            let mut ids = docs
                .keys()
                .filter(|(f, _, _)| *f == family)
                .map(|(_, id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            Ok(ids)
        }
        fn counter(&self, kind: WalletCounter, user: &str) -> PortResult<i64> {
            Ok(self.counters.lock().unwrap().get(&(kind, user.to_owned())).copied().unwrap_or(0))
        }
        fn set_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<()> {
            self.counters.lock().unwrap().insert((kind, user.to_owned()), amount);
            Ok(())
        }
        fn add_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<i64> {
            if amount < 0 {
                return Err(PortError::Denied("finance counter amount must be nonnegative"));
            }
            let next = self.counter(kind, user)?.checked_add(amount).ok_or(PortError::Failed("overflow".into()))?;
            self.set_counter(kind, user, next)?;
            Ok(next)
        }
        fn counter_links(&self, kind: WalletCounter) -> PortResult<Vec<(String, String)>> {
            Ok(self
                .counters
                .lock()
                .unwrap()
                .iter()
                .filter(|((k, _), _)| *k == kind)
                .map(|((_, user), v)| (user.clone(), v.to_string()))
                .collect())
        }
        fn marker(&self, marker: &FinanceMarker) -> PortResult<String> {
            let key = format!("{marker:?}");
            Ok(self.markers.lock().unwrap().get(&key).cloned().unwrap_or_default())
        }
        fn put_marker(&self, marker: &FinanceMarker, value: &str) -> PortResult<()> {
            self.markers.lock().unwrap().insert(format!("{marker:?}"), value.to_owned());
            Ok(())
        }
        fn hold_ids_by_payer(&self, user: &str, _limit: usize) -> PortResult<Vec<String>> {
            Ok(self
                .index
                .lock()
                .unwrap()
                .get(&format!("holds:{user}"))
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, id)| id)
                .collect())
        }
        fn journal_ids_by_user(&self, user: &str, _limit: usize) -> PortResult<Vec<String>> {
            Ok(self
                .index
                .lock()
                .unwrap()
                .get(&format!("journals:{user}"))
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, id)| id)
                .collect())
        }
        fn payout_ids_by_user(&self, user: &str, _limit: usize) -> PortResult<Vec<String>> {
            Ok(self
                .index
                .lock()
                .unwrap()
                .get(&format!("payouts:{user}"))
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, id)| id)
                .collect())
        }
        fn pool_of_user(&self, user: &str) -> PortResult<String> {
            Ok(self
                .index
                .lock()
                .unwrap()
                .get(&format!("pool:{user}"))
                .and_then(|items| items.first())
                .map(|(_, id)| id.clone())
                .unwrap_or_default())
        }
        fn put_pool_of_user(&self, user: &str, pool_id: &str) -> PortResult<()> {
            self.index
                .lock()
                .unwrap()
                .insert(format!("pool:{user}"), vec![(String::new(), pool_id.to_owned())]);
            Ok(())
        }
        fn email_to_id(&self, email: &str) -> PortResult<String> {
            Ok(self.emails.lock().unwrap().get(email).cloned().unwrap_or_default())
        }
        fn put_email_to_id(&self, email: &str, user_id: &str) -> PortResult<()> {
            self.emails.lock().unwrap().insert(email.to_owned(), user_id.to_owned());
            Ok(())
        }
        fn id_to_email(&self, user_id: &str) -> PortResult<String> {
            Ok(self.id_emails.lock().unwrap().get(user_id).cloned().unwrap_or_default())
        }
        fn put_id_to_email(&self, user_id: &str, email: &str) -> PortResult<()> {
            self.id_emails.lock().unwrap().insert(user_id.to_owned(), email.to_owned());
            Ok(())
        }
        fn write_journal(
            &self,
            kind: &str,
            hold_id: &str,
            payer_id: &str,
            payload: Value,
            participants: &[String],
            now: i64,
        ) -> PortResult<String> {
            let mut counter = self.counter.lock().unwrap();
            let n = *counter;
            *counter += 1;
            let journal_id = format!("journal-{}", n);
            let entry = json!({
                "journalId": journal_id, "kind": kind, "holdId": hold_id,
                "payerUserId": payer_id, "createdAt": now, "payload": payload,
            });
            self.docs.lock().unwrap().insert(
                (FinanceDoc::Journal, journal_id.clone(), "entry".to_owned()),
                entry,
            );
            let mut index = self.index.lock().unwrap();
            for participant in participants {
                if participant.is_empty() {
                    continue;
                }
                index
                    .entry(format!("journals:{participant}"))
                    .or_default()
                    .push((format!("{now:020}"), journal_id.clone()));
            }
            Ok(journal_id)
        }
        fn gen_id(&self) -> String {
            let mut counter = self.counter.lock().unwrap();
            let n = *counter;
            *counter += 1;
            format!("id-{n}")
        }
    }

    impl CreatureBalances for Memory {
        fn open(&self, _: &str, _: i64) -> PortResult<()> {
            Ok(())
        }
        fn close(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
        fn balance(&self, id: &str) -> PortResult<i64> {
            self.creatures.lock().unwrap().get(id).copied().ok_or(PortError::NotFound)
        }
        fn set_balance(&self, id: &str, balance: i64) -> PortResult<()> {
            let mut creatures = self.creatures.lock().unwrap();
            if !creatures.contains_key(id) {
                return Err(PortError::NotFound);
            }
            creatures.insert(id.to_owned(), balance);
            Ok(())
        }
    }

    impl CreatureDirectory for Memory {
        fn creature(&self, _: &str) -> PortResult<Option<aseman_domain::creature::CreatureRecord>> {
            Ok(None)
        }
        fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>> {
            Ok(self.usernames.lock().unwrap().get(username).cloned())
        }
        fn find_by_username_fragment(
            &self,
            _: &str,
        ) -> PortResult<Option<aseman_domain::creature::CreatureRecord>> {
            Ok(None)
        }
        fn creatures(
            &self,
            _: Option<&str>,
            _: i64,
            _: Option<i64>,
        ) -> PortResult<Vec<aseman_domain::creature::CreatureRecord>> {
            Ok(Vec::new())
        }
        fn create(&self, _: &aseman_domain::creature::CreatureRecord) -> PortResult<()> {
            Ok(())
        }
        fn update(&self, _: &aseman_domain::creature::CreatureRecord) -> PortResult<()> {
            Ok(())
        }
        fn delete(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
    }

    impl StoreDirectory for Memory {
        fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
            Ok(self.stores.lock().unwrap().get(store_id).cloned())
        }
        fn record_signal(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
        fn stores(&self, _: i64, _: Option<i64>) -> PortResult<Vec<StoreRecord>> {
            Ok(Vec::new())
        }
        fn create_store(&self, _: &StoreRecord, _: &str) -> PortResult<()> {
            Ok(())
        }
        fn update_store(&self, _: &StoreRecord) -> PortResult<()> {
            Ok(())
        }
        fn delete_store(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
        fn release_creator(&self, _: &str, _: &str) -> PortResult<()> {
            Ok(())
        }
    }

    impl StoreMetadata for Memory {
        fn store_metadata(&self, store_id: &str, _: &str) -> PortResult<Option<String>> {
            Ok(self.metadata.lock().unwrap().get(store_id).cloned())
        }
        fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()> {
            self.metadata.lock().unwrap().insert(store_id.to_owned(), document.to_owned());
            Ok(())
        }
        fn delete_store_metadata(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
    }

    impl StoreAccess for Memory {
        fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
            Ok(self
                .members
                .lock()
                .unwrap()
                .get(&(store_id.to_owned(), member_id.to_owned()))
                .copied()
                .unwrap_or_default())
        }
        fn set_permissions(
            &self,
            store_id: &str,
            member_id: &str,
            permissions: StorePermissions,
        ) -> PortResult<()> {
            self.members.lock().unwrap().insert((store_id.to_owned(), member_id.to_owned()), permissions);
            Ok(())
        }
        fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
            Ok(self
                .members
                .lock()
                .unwrap()
                .contains_key(&(store_id.to_owned(), member_id.to_owned())))
        }
        fn members(&self, _: &str) -> PortResult<Vec<(String, StorePermissions)>> {
            Ok(Vec::new())
        }
        fn stores_of(&self, _: &str) -> PortResult<Vec<String>> {
            Ok(Vec::new())
        }
        fn join(&self, store_id: &str, member_id: &str, permissions: StorePermissions) -> PortResult<()> {
            self.set_permissions(store_id, member_id, permissions)
        }
        fn leave(&self, _: &str, _: &str) -> PortResult<()> {
            Ok(())
        }
    }

    impl ProgramDirectory for Memory {
        fn program(&self, id: &str) -> PortResult<Option<ProgramRecord>> {
            Ok(self.programs.lock().unwrap().get(id).cloned())
        }
        fn programs(&self, _: i64, _: Option<i64>) -> PortResult<Vec<ProgramRecord>> {
            Ok(Vec::new())
        }
        fn programs_of_machine(&self, _: &str) -> PortResult<Vec<ProgramRecord>> {
            Ok(Vec::new())
        }
        fn create_program(&self, _: &ProgramRecord) -> PortResult<()> {
            Ok(())
        }
        fn update_program(&self, _: &ProgramRecord) -> PortResult<()> {
            Ok(())
        }
        fn delete_program(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
    }

    impl ClockPort for Memory {
        fn unix_millis(&self) -> i64 {
            *self.clock.lock().unwrap()
        }
    }

    fn creature(memory: &Memory, id: &str, balance: i64) {
        memory.creatures.lock().unwrap().insert(id.to_owned(), balance);
    }

    fn ports(memory: &Memory) -> FinancePorts<'_> {
        FinancePorts {
            ledger: memory,
            creatures: memory,
            balances: memory,
            stores: memory,
            store_metadata: memory,
            programs: memory,
            access: memory,
            clock: memory,
        }
    }

    #[test]
    fn create_hold_authorizes_and_releases_funds() {
        let memory = Memory::default();
        memory.clock(1_000_000);
        creature(&memory, "payer@global", 5000);
        creature(&memory, "authority@global", 1000);
        creature(&memory, "worker@global", 0);
        creature(&memory, "creator@global", 0);
        let port = ports(&memory);
        let input = CreateHoldInput {
            quote_id: "q1".into(),
            pricing_version: "v1".into(),
            max_amount: 100,
            settlement_authority: "authority@global".into(),
            meter_program_id: "p1".into(),
            expires_at: 1_000_000 + 60_000,
            idempotency_key: "k1".into(),
            context_hash: "a".repeat(64),
            beneficiary_plan_hash: finance_beneficiary_plan_hash(&[HoldBeneficiaryInput {
                user_id: "worker@global".into(),
                role: "agent".into(),
                max_amount: 100,
            }]),
            beneficiaries: vec![HoldBeneficiaryInput {
                user_id: "worker@global".into(),
                role: "agent".into(),
                max_amount: 100,
            }],
        };
        // A quote the pricing creature published, whose holdRequest is the exact
        // serialized create request.
        let quote = json!({
            "quoteId": "q1", "payerUserId": "payer@global", "maxAmount": 100,
            "expiresAt": 1_000_000 + 60_000, "projectId": "",
            "holdRequest": serde_json::to_value(&input).unwrap(),
        });
        put_billing_quote(&port, "q1", &quote).unwrap();
        memory
            .programs
            .lock()
            .unwrap()
            .insert("p1".to_owned(), ProgramRecord {
                id: "p1".into(), machine_id: "creator@global".into(),
                runtime: "wasm".into(), path: "/main".into(), comment: String::new(),
            });
        let outcome = create_hold(&port, "payer@global", input.clone()).unwrap();
        assert_eq!(outcome["applied"], true);
        // Payer's available balance fell by the held amount.
        assert_eq!(memory.balance("payer@global").unwrap(), 4900);
        assert_eq!(memory.counter(WalletCounter::Held, "payer@global").unwrap(), 100);
        // The hold document is stored open.
        let holds = memory.doc_ids(FinanceDoc::Hold).unwrap();
        assert_eq!(holds.len(), 1);
        let hold = get_finance_hold(&memory, &holds[0]).unwrap();
        assert_eq!(hold["status"], "open");
        assert_eq!(hold["maxAmount"], 100);
        // A retry with the same key is a no-op replay.
        let mut retry = input.clone();
        retry.idempotency_key = "k1".into();
        let again = create_hold(&port, "payer@global", retry).unwrap();
        assert_eq!(again["alreadyApplied"], true);
        assert_eq!(memory.balance("payer@global").unwrap(), 4900);
    }

    #[test]
    fn transfer_moves_withdrawable_and_repays_debt() {
        let memory = Memory::default();
        creature(&memory, "alice@global", 1000);
        creature(&memory, "bob@global", 0);
        memory
            .usernames
            .lock()
            .unwrap()
            .insert("bob@global".to_owned(), "bob@global".to_owned());
        memory
            .set_counter(WalletCounter::Withdrawable, "alice@global", 900)
            .unwrap();
        memory.set_counter(WalletCounter::Debt, "bob@global", 40).unwrap();
        let port = ports(&memory);
        let outcome = transfer(
            &port,
            "alice@global",
            TransferInput { amount: 100, to_username: "bob@global".into() },
        )
        .unwrap();
        assert_eq!(outcome["amount"], 100);
        assert_eq!(outcome["toUserId"], "bob@global");
        assert_eq!(outcome["debtRepaid"], 40);
        // Alice spent 100, all non-withdrawable (1000 balance - 900 withdrawable).
        assert_eq!(memory.balance("alice@global").unwrap(), 900);
        assert_eq!(memory.counter(WalletCounter::Withdrawable, "alice@global").unwrap(), 900);
        // Bob received 60 new (40 repayed debt). The legacy withdrawable rule keeps
        // the sender's composition: the whole debt was repaid from non-withdrawable
        // funds, so the 60 the payer sent lands fully withdrawable.
        assert_eq!(memory.balance("bob@global").unwrap(), 60);
        assert_eq!(memory.counter(WalletCounter::Withdrawable, "bob@global").unwrap(), 60);
        assert_eq!(memory.counter(WalletCounter::Debt, "bob@global").unwrap(), 0);
    }

    #[test]
    fn open_and_close_pool_refund_the_remaining() {
        let memory = Memory::default();
        memory.clock(2_000_000);
        creature(&memory, "payer@global", 1000);
        creature(&memory, "authority@global", 1000);
        let port = ports(&memory);
        let opened = open_pool(
            &port,
            "payer@global",
            OpenPoolInput {
                max_amount: 300,
                settlement_authority: "authority@global".into(),
                meter_program_id: "p1".into(),
                expires_at: 2_000_000 + 60_000,
                idempotency_key: "pk1".into(),
            },
        )
        .unwrap();
        assert_eq!(opened["applied"], true);
        assert_eq!(memory.balance("payer@global").unwrap(), 700);
        assert_eq!(memory.counter(WalletCounter::Held, "payer@global").unwrap(), 300);
        let pool_id = opened["pool"]["poolId"].as_str().unwrap().to_string();
        // Closing refunds the full remaining balance.
        let closed = close_pool(
            &port,
            "payer@global",
            ClosePoolInput { pool_id, close_id: "c1".into(), reason: String::new() },
        )
        .unwrap();
        assert_eq!(closed["applied"], true);
        assert_eq!(memory.balance("payer@global").unwrap(), 1000);
        assert_eq!(memory.counter(WalletCounter::Held, "payer@global").unwrap(), 0);
    }

    #[test]
    fn reconciliation_reports_a_drift_issue() {
        let memory = Memory::default();
        creature(&memory, "u@global", 0);
        // A hold says open with max 100 but the held counter is missing.
        let port = ports(&memory);
        memory.clock(3_000_000);
        let quote = json!({
            "quoteId": "q1", "payerUserId": "u@global", "maxAmount": 100,
            "expiresAt": 3_000_000 + 60_000, "projectId": "",
            "holdRequest": {
                "quoteId": "q1", "pricingVersion": "v1", "maxAmount": 100,
                "settlementAuthority": "authority@global", "meterProgramId": "p1",
                "expiresAt": 3_000_000 + 60_000, "idempotencyKey": "k1",
                "contextHash": "a".repeat(64), "beneficiaryPlanHash": "b".repeat(64),
                "beneficiaries": [{"userId": "worker@global", "role": "agent", "maxAmount": 100}]
            }
        });
        put_billing_quote(&port, "q1", &quote).unwrap();
        memory
            .programs
            .lock()
            .unwrap()
            .insert("p1".to_owned(), ProgramRecord {
                id: "p1".into(), machine_id: "creator@global".into(),
                runtime: "wasm".into(), path: "/main".into(), comment: String::new(),
            });
        let hold = json!({
            "holdId": "h1", "payerUserId": "u@global", "maxAmount": 100,
            "remainingAmount": 100, "status": "open", "projectId": "",
        });
        memory
            .docs
            .lock()
            .unwrap()
            .insert((FinanceDoc::Hold, "h1".into(), "hold".into()), hold);
        let report = reconcile_financial_system(&port, LEGACY_ROOT, ReconcileFinancialSystemInput::default()).unwrap();
        assert_eq!(report["healthy"], false);
        let codes = report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|issue| issue["code"].as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"held.mismatch"), "codes: {codes:?}");
    }
}
