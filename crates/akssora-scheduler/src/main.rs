#![allow(dead_code)]

use std::net::SocketAddr;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

mod circuit_breaker;
mod demand_tracker;
mod host_registry;
mod migration;
mod placement;
mod prewarm_policy;
mod region_registry;
mod sharding;

use demand_tracker::DemandTracker;
use host_registry::{HostRegistry, ResourceRequest};
use migration::{ConnectionRedirectTable, MigrationOrchestrator, MigrationRequest};
use placement::{PlacementDecision, best_fit_place};
use prewarm_policy::{AdaptivePrewarmPolicy, compute_prewarm_targets};
use region_registry::RegionRegistry;

#[derive(Clone)]
struct SchedulerState {
    registry: std::sync::Arc<HostRegistry>,
    region_registry: std::sync::Arc<RegionRegistry>,
    demand_tracker: std::sync::Arc<DemandTracker>,
    prewarm_policy: std::sync::Arc<AdaptivePrewarmPolicy>,
    migration_orchestrator: std::sync::Arc<MigrationOrchestrator>,
    redirect_table: std::sync::Arc<ConnectionRedirectTable>,
}

#[derive(Debug, Deserialize)]
struct HeartbeatRequest {
    host_id: Uuid,
    address: SocketAddr,
    total_vcpus: u32,
    total_mem_mib: u32,
    used_vcpus: u32,
    used_mem_mib: u32,
    sandbox_count: u32,
}

#[derive(Debug, Serialize)]
struct HeartbeatResponse {
    acknowledged: bool,
}

#[derive(Debug, Deserialize)]
struct ScheduleRequest {
    vcpus: u32,
    mem_mib: u32,
    template_id: Option<String>,
    region: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScheduleResponse {
    host_id: Uuid,
    address: SocketAddr,
    free_vcpus: u32,
    free_mem_mib: u32,
}

#[derive(Debug, Serialize)]
struct FleetMetricsResponse {
    total_hosts: usize,
    total_sandboxes: u32,
    hosts: Vec<host_registry::HostSnapshot>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    hosts: usize,
}

#[derive(Debug, Deserialize)]
struct StartMigrationRequest {
    sandbox_id: uuid::Uuid,
    source_host_id: uuid::Uuid,
    target_host_id: Option<uuid::Uuid>,
    template_id: Option<String>,
    #[serde(default = "default_migration_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_drain_secs")]
    drain_secs: u64,
}

fn default_migration_timeout() -> u64 {
    60
}
fn default_drain_secs() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct MigrationIdPath {
    migration_id: uuid::Uuid,
}

#[derive(Debug, Deserialize)]
struct DrainHostRequestDto {
    host_id: uuid::Uuid,
    target_host_id: Option<uuid::Uuid>,
    #[serde(default = "default_max_concurrent")]
    max_concurrent: usize,
}

fn default_max_concurrent() -> usize {
    5
}

#[tracing::instrument(skip(state), fields(host_id = %req.host_id))]
async fn heartbeat_handler(
    State(state): State<SchedulerState>,
    Json(req): Json<HeartbeatRequest>,
) -> (StatusCode, Json<HeartbeatResponse>) {
    state
        .registry
        .upsert_host(req.host_id, req.address, req.total_vcpus, req.total_mem_mib);
    state.registry.heartbeat(
        &req.host_id,
        req.used_vcpus,
        req.used_mem_mib,
        req.sandbox_count,
    );

    (
        StatusCode::OK,
        Json(HeartbeatResponse { acknowledged: true }),
    )
}

#[tracing::instrument(skip(state))]
async fn schedule_handler(
    State(state): State<SchedulerState>,
    Json(req): Json<ScheduleRequest>,
) -> Result<(StatusCode, Json<ScheduleResponse>), (StatusCode, String)> {
    let resource_req = ResourceRequest {
        vcpus: req.vcpus,
        mem_mib: req.mem_mib,
    };

    let template_id = req
        .template_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    state.demand_tracker.record(&template_id);

    let placement_decision = if let Some(ref region) = req.region {
        info!(region = %region, "region-aware scheduling requested");
        state
            .region_registry
            .place_sandbox(&resource_req, Some(region))
    } else {
        let hosts = state.registry.available_hosts();
        best_fit_place(&hosts, &resource_req)
    };

    match placement_decision {
        PlacementDecision::Placed { host } => {
            info!(host_id = %host.host_id, template = %template_id, "sandbox placed");
            Ok((
                StatusCode::OK,
                Json(ScheduleResponse {
                    host_id: host.host_id,
                    address: host.address,
                    free_vcpus: host.free_vcpus(),
                    free_mem_mib: host.free_mem_mib(),
                }),
            ))
        }
        PlacementDecision::Unplaced { reason } => {
            tracing::warn!(?reason, "no host available for scheduling");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("no host available: {reason:?}"),
            ))
        }
    }
}

async fn fleet_handler(State(state): State<SchedulerState>) -> Json<FleetMetricsResponse> {
    Json(FleetMetricsResponse {
        total_hosts: state.registry.host_count(),
        total_sandboxes: state.registry.total_sandboxes(),
        hosts: state.registry.available_hosts(),
    })
}

async fn host_handler(
    State(state): State<SchedulerState>,
    Path(host_id): Path<Uuid>,
) -> Result<Json<host_registry::HostSnapshot>, StatusCode> {
    state
        .registry
        .get_host(&host_id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn report_failure_handler(
    State(state): State<SchedulerState>,
    Path(host_id): Path<Uuid>,
) -> StatusCode {
    state.registry.record_failure(&host_id);
    StatusCode::OK
}

async fn decommission_handler(
    State(state): State<SchedulerState>,
    Path(host_id): Path<Uuid>,
) -> StatusCode {
    if state.registry.remove_host(&host_id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn health_handler(State(state): State<SchedulerState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        hosts: state.registry.host_count(),
    })
}

async fn demand_handler(
    State(state): State<SchedulerState>,
) -> Json<Vec<demand_tracker::TemplateDemand>> {
    Json(state.demand_tracker.all_demands())
}

async fn demand_top_handler(
    State(state): State<SchedulerState>,
    Path(n): Path<usize>,
) -> Json<Vec<demand_tracker::TemplateDemand>> {
    Json(state.demand_tracker.top_templates(n))
}

#[derive(Debug, Serialize)]
struct PrewarmResponse {
    targets: std::collections::HashMap<String, usize>,
    total_desired: usize,
}

async fn prewarm_handler(State(state): State<SchedulerState>) -> Json<PrewarmResponse> {
    let targets = compute_prewarm_targets(&state.demand_tracker, &*state.prewarm_policy, 20);
    let total_desired: usize = targets.values().sum();

    Json(PrewarmResponse {
        targets,
        total_desired,
    })
}

async fn start_migration_handler(
    State(state): State<SchedulerState>,
    Json(req): Json<StartMigrationRequest>,
) -> Result<(StatusCode, Json<migration::MigrationRecordSnapshot>), (StatusCode, String)> {
    let migration_req = MigrationRequest {
        sandbox_id: req.sandbox_id,
        source_host_id: req.source_host_id,
        target_host_id: req.target_host_id,
        template_id: req.template_id,
        timeout_secs: req.timeout_secs,
        drain_secs: req.drain_secs,
    };

    let migration_id = state
        .migration_orchestrator
        .start_migration(migration_req)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let _ = state.migration_orchestrator.advance(&migration_id).await;

    let snapshot = state
        .migration_orchestrator
        .get_record(&migration_id)
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "record not found".into()))?;

    Ok((StatusCode::CREATED, Json(snapshot)))
}

async fn advance_migration_handler(
    State(state): State<SchedulerState>,
    Path(params): Path<MigrationIdPath>,
) -> Result<Json<migration::MigrationRecordSnapshot>, (StatusCode, String)> {
    let new_state = state
        .migration_orchestrator
        .advance(&params.migration_id)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    if new_state == migration::MigrationState::Complete
        && let Some(record) = state
            .migration_orchestrator
            .get_record(&params.migration_id)
        && let Some(target_id) = record.target_host_id
        && let Some(host) = state.registry.get_host(&target_id)
    {
        state
            .redirect_table
            .register_redirect(record.sandbox_id, host);
    }

    let snapshot = state
        .migration_orchestrator
        .get_record(&params.migration_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "migration not found".into()))?;

    Ok(Json(snapshot))
}

async fn rollback_migration_handler(
    State(state): State<SchedulerState>,
    Path(params): Path<MigrationIdPath>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<migration::MigrationRecordSnapshot>, (StatusCode, String)> {
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("manual rollback requested")
        .to_string();

    state
        .migration_orchestrator
        .rollback(&params.migration_id, &reason)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    let snapshot = state
        .migration_orchestrator
        .get_record(&params.migration_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "migration not found".into()))?;

    Ok(Json(snapshot))
}

async fn list_migrations_handler(
    State(state): State<SchedulerState>,
) -> Json<Vec<migration::MigrationRecordSnapshot>> {
    Json(state.migration_orchestrator.active_migrations())
}

async fn get_migration_handler(
    State(state): State<SchedulerState>,
    Path(params): Path<MigrationIdPath>,
) -> Result<Json<migration::MigrationRecordSnapshot>, StatusCode> {
    state
        .migration_orchestrator
        .get_record(&params.migration_id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn completed_migrations_handler(
    State(state): State<SchedulerState>,
) -> Json<Vec<migration::MigrationRecordSnapshot>> {
    Json(state.migration_orchestrator.completed_migrations())
}

async fn redirect_handler(
    State(state): State<SchedulerState>,
    Path(sandbox_id): Path<uuid::Uuid>,
) -> Result<Json<migration::MigrationRecordSnapshot>, StatusCode> {
    if let Some(host) = state.redirect_table.get_redirect(&sandbox_id) {
        let snapshot = migration::MigrationRecordSnapshot {
            migration_id: uuid::Uuid::nil(),
            sandbox_id,
            source_host_id: uuid::Uuid::nil(),
            target_host_id: Some(host.host_id),
            state: migration::MigrationState::Complete,
            elapsed_ms: 0,
            created_at: chrono::Utc::now(),
            state_history: vec![],
        };
        Ok(Json(snapshot))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[derive(Debug, Serialize)]
struct RegionListResponse {
    regions: Vec<region_registry::RegionInfo>,
}

async fn list_regions_handler(State(state): State<SchedulerState>) -> Json<RegionListResponse> {
    Json(RegionListResponse {
        regions: state.region_registry.list_region_info(),
    })
}

async fn get_region_handler(
    State(state): State<SchedulerState>,
    Path(region_id): Path<String>,
) -> Result<Json<region_registry::RegionInfo>, StatusCode> {
    state
        .region_registry
        .get_region_info(&region_id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

fn build_router(state: SchedulerState) -> Router {
    Router::new()
        .route("/heartbeat", post(heartbeat_handler))
        .route("/schedule", post(schedule_handler))
        .route("/fleet", get(fleet_handler))
        .route("/regions", get(list_regions_handler))
        .route("/regions/{id}", get(get_region_handler))
        .route("/hosts/{id}", get(host_handler))
        .route("/hosts/{id}/report-failure", post(report_failure_handler))
        .route("/hosts/{id}", axum::routing::delete(decommission_handler))
        .route("/demand", get(demand_handler))
        .route("/demand/top/{n}", get(demand_top_handler))
        .route("/prewarm", get(prewarm_handler))
        .route("/migrations", post(start_migration_handler))
        .route("/migrations", get(list_migrations_handler))
        .route("/migrations/completed", get(completed_migrations_handler))
        .route("/migrations/{migration_id}", get(get_migration_handler))
        .route(
            "/migrations/{migration_id}/advance",
            post(advance_migration_handler),
        )
        .route(
            "/migrations/{migration_id}/rollback",
            post(rollback_migration_handler),
        )
        .route("/redirects/{sandbox_id}", get(redirect_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "akssora_scheduler=info,akssora_core=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());
    tracing::info!(
        otlp_endpoint = %otlp_endpoint,
        "Scheduler tracing initialized (default-on)"
    );

    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(4000);

    let registry = std::sync::Arc::new(HostRegistry::default());
    let region_registry = std::sync::Arc::new(RegionRegistry::new(registry.clone()));

    region_registry.register_region("us-east-1", "US East (Virginia)", "Virginia, USA");
    region_registry.register_region("us-west-2", "US West (Oregon)", "Oregon, USA");
    region_registry.register_region("eu-west-1", "EU West (Ireland)", "Dublin, Ireland");
    region_registry.register_region("ap-south-1", "Asia Pacific (Mumbai)", "Mumbai, India");

    let demand_tracker = std::sync::Arc::new(DemandTracker::default());
    let prewarm_policy = std::sync::Arc::new(AdaptivePrewarmPolicy::new());
    let migration_orchestrator = std::sync::Arc::new(MigrationOrchestrator::new(registry.clone()));
    let redirect_table = std::sync::Arc::new(ConnectionRedirectTable::new());
    let state = SchedulerState {
        registry,
        region_registry,
        demand_tracker,
        prewarm_policy,
        migration_orchestrator,
        redirect_table,
    };

    let app = build_router(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Akssora Scheduler listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut stream) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received, terminating gracefully");
}
