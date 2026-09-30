use axum::{
    extract::State,
    response::Html,
    Json,
};

use crate::health::{
    DependencyStatus, GraphHealthStatus, HealthGraphObservation, HealthGraphResponse,
};
use crate::ApiState;

pub async fn get_health_graph(State(state): State<ApiState>) -> Json<HealthGraphResponse> {
    Json(collect_health_graph(&state).await)
}

pub async fn get_health_graph_view() -> Html<&'static str> {
    Html(HEALTH_GRAPH_VIEW)
}

const HEALTH_GRAPH_VIEW: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Dependency Health Graph</title><style>
:root{--ink:#20292f;--muted:#637078;--line:#d5dcdf;--paper:#f3f6f4;--healthy:#15734a;--degraded:#9b6200;--unhealthy:#b53732;--unknown:#68737b}
*{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:14px/1.45 system-ui,sans-serif}main{max-width:1160px;margin:0 auto;padding:26px 20px}h1{font-size:22px;margin:0 0 5px}.muted{color:var(--muted)}.auth{display:flex;align-items:end;gap:10px;flex-wrap:wrap;margin:18px 0}.auth label{display:grid;gap:4px;color:var(--muted)}input,button{font:inherit;padding:8px 10px;border:1px solid var(--line);border-radius:4px}button{background:#20292f;color:white;cursor:pointer}button:disabled{opacity:.6;cursor:wait}.error{color:var(--unhealthy);min-height:1.5em}.meta{margin:12px 0;color:var(--muted)}.canvas{overflow-x:auto;border-block:1px solid var(--line);padding:18px 0}svg{display:block;width:100%;min-width:900px;height:auto}.node rect{fill:#fff;stroke-width:2}.healthy rect{stroke:var(--healthy)}.degraded rect{stroke:var(--degraded)}.unhealthy rect{stroke:var(--unhealthy)}.unknown rect{stroke:var(--unknown);stroke-dasharray:5 4}.node-title{font-size:16px;font-weight:650;fill:var(--ink)}.node-state{font-size:12px;font-weight:700;text-transform:uppercase}.healthy .node-state{fill:var(--healthy)}.degraded .node-state{fill:var(--degraded)}.unhealthy .node-state{fill:var(--unhealthy)}.unknown .node-state{fill:var(--unknown)}.node-detail{font-size:10px;fill:var(--muted)}.edge{fill:none;stroke-width:2}.edge.critical{stroke:var(--unhealthy)}.edge.optional{stroke:#7f8c91;stroke-dasharray:5 5}.edge-label{fill:var(--muted);font-size:11px;text-anchor:middle}.legend{display:flex;flex-wrap:wrap;gap:14px;margin:14px 0;color:var(--muted)}.legend span{display:flex;align-items:center;gap:6px}.dot{width:10px;height:10px;border-radius:50%;background:currentColor}.impact{border-top:1px solid var(--line);padding-top:12px}.healthy-text{color:var(--healthy)}.degraded-text{color:var(--degraded)}.unhealthy-text{color:var(--unhealthy)}.unknown-text{color:var(--unknown)}
</style></head><body><main><h1>Dependency health graph</h1>
<p class="muted">Live status for Postgres, Redis, Vault, and the settlement network API.</p>
<form class="auth" id="auth-form"><label for="admin-key">Admin API key<input id="admin-key" type="password" autocomplete="current-password" required></label><button id="load-button" type="submit">Load graph</button></form>
<p class="error" id="error" role="alert" aria-live="polite"></p><p class="meta" id="meta" aria-live="polite"></p><div class="canvas" id="canvas" hidden><svg id="graph" viewBox="0 0 1080 410" role="img" aria-label="Service dependency health graph"></svg></div>
<div class="legend"><span class="healthy-text"><i class="dot"></i>Healthy</span><span class="degraded-text"><i class="dot"></i>Degraded</span><span class="unhealthy-text"><i class="dot"></i>Unhealthy</span><span class="unknown-text"><i class="dot"></i>Unknown</span><span>Solid edge: critical</span><span>Dashed edge: non-critical</span></div>
<p class="impact" id="impact">Service health and dependency health are reported separately.</p></main>
<script>
const ns="http://www.w3.org/2000/svg", positions={service:[430,65],postgres:[40,265],redis:[300,265],vault:[560,265],settlement_network:[820,265]};
const form=document.getElementById("auth-form"), keyInput=document.getElementById("admin-key"), button=document.getElementById("load-button"), error=document.getElementById("error"), meta=document.getElementById("meta"), canvas=document.getElementById("canvas"), svg=document.getElementById("graph"), impact=document.getElementById("impact");
function element(name,attrs={}){const node=document.createElementNS(ns,name);for(const [key,value] of Object.entries(attrs))node.setAttribute(key,String(value));return node}
function text(parent,x,y,value,className){const item=element("text",{x,y,class:className});item.textContent=value;parent.append(item);return item}
function draw(data){svg.replaceChildren();for(const edge of data.edges){const [sx,sy]=positions[edge.source], [tx,ty]=positions[edge.target], startX=sx+110, targetX=tx+110;const path=element("path",{d:`M ${startX} ${sy+94} C ${startX} ${sy+134}, ${targetX} ${ty-42}, ${targetX} ${ty}`,class:`edge ${edge.critical?"critical":"optional"}`,"marker-end":"url(#arrow)"});svg.append(path);text(svg,(startX+targetX)/2,220,edge.critical?"critical":"non-critical","edge-label")}
const defs=element("defs"), marker=element("marker",{id:"arrow",markerWidth:8,markerHeight:8,refX:7,refY:4,orient:"auto"}), arrow=element("path",{d:"M0,0 L8,4 L0,8 z",fill:"#7f8c91"});marker.append(arrow);defs.append(marker);svg.prepend(defs);
for(const node of data.nodes){const position=positions[node.id];if(!position)continue;const [x,y]=position;const group=element("g",{class:`node ${node.status}`}), rect=element("rect",{x,y,width:220,height:94,rx:5});group.append(rect);text(group,x+14,y+24,node.name,"node-title");text(group,x+14,y+47,node.status,"node-state");if(node.id==="service"){text(group,x+14,y+68,`own: ${node.own_status} · dependencies: ${node.dependency_status}`,"node-detail");text(group,x+14,y+84,node.detail||"","node-detail")}else{text(group,x+14,y+70,node.detail||"No detail available","node-detail")}svg.append(group)}
const root=data.nodes.find(node=>node.id==="service"), names=Object.fromEntries(data.nodes.map(node=>[node.id,node.name]));meta.textContent=`Overall: ${data.status} · Generated ${new Date(data.generated_at).toLocaleString()}`;impact.textContent=`Dependency impact: ${root.affected_by.length?root.affected_by.map(id=>names[id]||id).join(", "):"None"}. Service own status: ${root.own_status}; dependency status: ${root.dependency_status}.`;canvas.hidden=false}
form.addEventListener("submit",async event=>{event.preventDefault();error.textContent="";button.disabled=true;const key=keyInput.value;keyInput.value="";try{const response=await fetch("/admin/health/graph",{headers:{Authorization:`Bearer ${key}`},cache:"no-store"});if(!response.ok)throw new Error(response.status===401?"Admin authentication failed.":`Health graph request failed (${response.status}).`);draw(await response.json())}catch(err){error.textContent=err.message;canvas.hidden=true}finally{button.disabled=false}});
</script></body></html>"##;

async fn collect_health_graph(state: &ApiState) -> HealthGraphResponse {
    let app = &state.app_state;
    let dependencies = crate::health::check_health(
        crate::health::PostgresChecker::new(app.db.clone()),
        crate::health::RedisChecker::new(app.redis_url.clone()),
        crate::health::HorizonChecker::new(app.horizon_client.clone()),
        app.start_time,
    )
    .await;
    let mut observations: Vec<HealthGraphObservation> = dependencies
        .dependencies
        .into_iter()
        .map(|(id, status)| {
            let id = if id == "horizon" {
                "settlement_network".to_string()
            } else {
                id
            };
            dependency_observation(id, status)
        })
        .collect();

    let readiness = crate::readiness::dependency_readiness(
        app.secrets_store.as_ref(),
        std::time::Instant::now(),
    );
    if readiness.redis.status == "degraded" {
        if let Some(redis) = observations.iter_mut().find(|item| item.id == "redis") {
            if redis.status == GraphHealthStatus::Healthy {
                redis.status = GraphHealthStatus::Degraded;
            }
            let components = readiness.redis.degraded_components.join(", ");
            redis.detail = Some(format!(
                "Redis-dependent components degraded: {components}"
            ));
        }
    }

    let vault = readiness.vault;
    let (vault_status, vault_detail) = match vault.status {
        "ok" => (
            GraphHealthStatus::Healthy,
            Some("Vault secrets are fresh".to_string()),
        ),
        "degraded_cached_fallback" => (
            GraphHealthStatus::Degraded,
            Some(format!(
                "Vault unreachable for {}s; cached secrets have {}s remaining",
                vault.unreachable_for_secs.unwrap_or_default(),
                vault.fallback_remaining_secs.unwrap_or_default()
            )),
        ),
        "expired" => (
            GraphHealthStatus::Unhealthy,
            Some(format!(
                "Vault unreachable for {}s; cached secrets exceeded the {}s maximum age",
                vault.unreachable_for_secs.unwrap_or_default(),
                vault.max_fallback_age_secs.unwrap_or_default()
            )),
        ),
        _ => (
            GraphHealthStatus::Unknown,
            Some("Vault-backed secrets are not configured".to_string()),
        ),
    };
    observations.push(HealthGraphObservation {
        id: "vault".to_string(),
        status: vault_status,
        detail: vault_detail,
    });

    let own_status = if app.readiness.is_draining() {
        GraphHealthStatus::Degraded
    } else if app.readiness.is_ready() {
        GraphHealthStatus::Healthy
    } else {
        GraphHealthStatus::Unhealthy
    };
    let own_detail = match own_status {
        GraphHealthStatus::Healthy => Some("Service is accepting traffic".to_string()),
        GraphHealthStatus::Degraded => Some("Service is draining".to_string()),
        GraphHealthStatus::Unhealthy => Some("Service is not ready".to_string()),
        GraphHealthStatus::Unknown => None,
    };

    crate::health::build_health_graph(own_status, own_detail, observations)
}

fn dependency_observation(id: String, status: DependencyStatus) -> HealthGraphObservation {
    let (status, detail) = match status {
        DependencyStatus::Healthy {
            latency_ms, status, ..
        } => (
            parse_status(&status),
            Some(format!("Probe succeeded in {latency_ms}ms")),
        ),
        DependencyStatus::Unhealthy { status, .. } => (
            parse_status(&status),
            Some("Probe failed; inspect dependency logs for details".to_string()),
        ),
    };
    HealthGraphObservation { id, status, detail }
}

fn parse_status(status: &str) -> GraphHealthStatus {
    match status {
        "healthy" => GraphHealthStatus::Healthy,
        "degraded" => GraphHealthStatus::Degraded,
        "unhealthy" => GraphHealthStatus::Unhealthy,
        _ => GraphHealthStatus::Unknown,
    }
}

#[cfg(test)]
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
fn status_class(status: GraphHealthStatus) -> &'static str {
    match status {
        GraphHealthStatus::Healthy => "healthy",
        GraphHealthStatus::Degraded => "degraded",
        GraphHealthStatus::Unhealthy => "unhealthy",
        GraphHealthStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
fn node_card(node: &crate::health::HealthGraphNode, x: i32, y: i32) -> String {
    let raw_detail = node
        .detail
        .as_deref()
        .unwrap_or("No detail available");
    let mut detail: String = raw_detail.chars().take(36).collect();
    if raw_detail.chars().count() > 36 {
        detail.push_str("...");
    }
    format!(
        "<g class=\"node {class}\"><rect x=\"{x}\" y=\"{y}\" width=\"220\" height=\"94\" rx=\"5\"/><text class=\"node-title\" x=\"{}\" y=\"{}\">{}</text><text class=\"node-status\" x=\"{}\" y=\"{}\">{status}</text><text class=\"node-detail\" x=\"{}\" y=\"{}\">{detail}</text></g>",
        x + 14,
        y + 24,
        escape_html(&node.name),
        x + 14,
        y + 47,
        status = status_class(node.status),
        x = x + 14,
        y = y + 70,
        detail = escape_html(&detail),
        class = status_class(node.status),
    )
}

#[cfg(test)]
fn render_graph_view(graph: &HealthGraphResponse) -> String {
    let service = &graph.nodes[0];
    let dependency_positions = [(40, 270), (300, 270), (560, 270), (820, 270)];
    let service_pos = (430, 70);
    let mut svg = String::from(
        "<svg viewBox=\"0 0 1080 410\" role=\"img\" aria-labelledby=\"graph-title graph-desc\"><title id=\"graph-title\">Live dependency health graph</title><desc id=\"graph-desc\">Synapse Core depends on Postgres, Redis, Vault, and the settlement network API. Solid red edges indicate critical dependencies.</desc><defs><marker id=\"arrow\" markerWidth=\"8\" markerHeight=\"8\" refX=\"7\" refY=\"4\" orient=\"auto\"><path d=\"M0,0 L8,4 L0,8 z\" fill=\"context-stroke\"/></marker></defs>",
    );
    for (index, edge) in graph.edges.iter().enumerate() {
        let (x, _) = dependency_positions[index];
        let class = if edge.critical { "critical" } else { "optional" };
        svg.push_str(&format!(
            "<path class=\"edge {class}\" d=\"M {} 164 C {} 208, {} 218, {} 268\" marker-end=\"url(#arrow)\"/><text class=\"edge-label\" x=\"{}\" y=\"219\">{}</text>",
            service_pos.0 + 110,
            service_pos.0 + 110,
            x + 110,
            x + 110,
            (service_pos.0 + x + 220) / 2,
            if edge.critical { "critical" } else { "non-critical" }
        ));
    }
    svg.push_str(&node_card(service, service_pos.0, service_pos.1));
    for (node, (x, y)) in graph.nodes.iter().skip(1).zip(dependency_positions) {
        svg.push_str(&node_card(node, x, y));
    }
    svg.push_str("</svg>");

    let affected_by = if service.affected_by.is_empty() {
        "None".to_string()
    } else {
        service.affected_by.join(", ")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Dependency Health Graph</title><style>:root{{--ink:#20292f;--muted:#637078;--line:#d5dcdf;--paper:#f3f6f4;--healthy:#15734a;--degraded:#9b6200;--unhealthy:#b53732;--unknown:#68737b}}*{{box-sizing:border-box}}body{{margin:0;background:var(--paper);color:var(--ink);font:14px/1.45 system-ui,sans-serif}}main{{max-width:1160px;margin:0 auto;padding:26px 20px}}h1{{font-size:22px;margin:0 0 5px}}.meta{{color:var(--muted);margin:0 0 20px}}.canvas{{overflow-x:auto;border-block:1px solid var(--line);padding:20px 0}}svg{{display:block;width:100%;min-width:900px;height:auto}}.node rect{{fill:#fff;stroke-width:2}}.node.healthy rect{{stroke:var(--healthy)}}.node.degraded rect{{stroke:var(--degraded)}}.node.unhealthy rect{{stroke:var(--unhealthy)}}.node.unknown rect{{stroke:var(--unknown);stroke-dasharray:5 4}}.node-title{{font-size:16px;font-weight:650;fill:var(--ink)}}.node-status{{font-size:12px;font-weight:700;text-transform:uppercase}}.node.healthy .node-status{{fill:var(--healthy)}}.node.degraded .node-status{{fill:var(--degraded)}}.node.unhealthy .node-status{{fill:var(--unhealthy)}}.node.unknown .node-status{{fill:var(--unknown)}}.node-detail{{font-size:10px;fill:var(--muted)}}.edge{{fill:none;stroke-width:2}}.edge.critical{{stroke:var(--unhealthy)}}.edge.optional{{stroke:#7f8c91;stroke-dasharray:5 5}}.edge-label{{fill:var(--muted);font-size:11px;text-anchor:middle}}.legend{{display:flex;flex-wrap:wrap;gap:14px;margin-top:16px;color:var(--muted)}}.legend span{{display:flex;align-items:center;gap:6px}}.swatch{{width:10px;height:10px;border-radius:50%;background:currentColor}}.impact{{margin-top:18px;padding:12px 0;border-top:1px solid var(--line)}}.impact strong{{color:var(--unhealthy)}}code{{font:inherit;color:var(--ink)}}</style></head><body><main><h1>Dependency health graph</h1><p class=\"meta\">Overall: <strong class=\"{overall}\">{overall}</strong> · Generated {generated}</p><div class=\"canvas\">{svg}</div><div class=\"legend\"><span style=\"color:var(--healthy)\"><i class=\"swatch\"></i>Healthy</span><span style=\"color:var(--degraded)\"><i class=\"swatch\"></i>Degraded</span><span style=\"color:var(--unhealthy)\"><i class=\"swatch\"></i>Unhealthy</span><span style=\"color:var(--unknown)\"><i class=\"swatch\"></i>Unknown</span><span>Solid edge: critical dependency</span><span>Dashed edge: non-critical dependency</span></div><p class=\"impact\"><strong>Dependency impact:</strong> <code>{affected}</code>. The service node reports its own readiness separately from upstream health.</p><p class=\"meta\">JSON data: <a href=\"/admin/health/graph\">/admin/health/graph</a></p></main></body></html>",
        overall = status_class(graph.status),
        generated = graph.generated_at.to_rfc3339(),
        svg = svg,
        affected = escape_html(&affected_by),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{HealthGraphEdge, HealthGraphNode};

    #[test]
    fn public_viewer_fetches_only_the_protected_graph_endpoint() {
        assert!(HEALTH_GRAPH_VIEW.contains("type=\"password\""));
        assert!(HEALTH_GRAPH_VIEW.contains("fetch(\"/admin/health/graph\""));
        assert!(HEALTH_GRAPH_VIEW.contains("Authorization:`Bearer ${key}`"));
        assert!(HEALTH_GRAPH_VIEW.contains("keyInput.value=\"\""));
        assert!(!HEALTH_GRAPH_VIEW.contains("localStorage"));
        assert!(!HEALTH_GRAPH_VIEW.contains("sessionStorage"));
    }

    #[test]
    fn graph_view_escapes_upstream_detail() {
        let graph = HealthGraphResponse {
            generated_at: chrono::Utc::now(),
            status: GraphHealthStatus::Degraded,
            nodes: vec![
                HealthGraphNode {
                    id: "service".into(),
                    name: "Synapse Core".into(),
                    kind: "service".into(),
                    status: GraphHealthStatus::Degraded,
                    own_status: GraphHealthStatus::Healthy,
                    dependency_status: GraphHealthStatus::Unhealthy,
                    dependencies: vec!["redis".into()],
                    affected_by: vec!["redis".into()],
                    detail: None,
                },
                HealthGraphNode {
                    id: "redis".into(),
                    name: "Redis <down>".into(),
                    kind: "dependency".into(),
                    status: GraphHealthStatus::Unhealthy,
                    own_status: GraphHealthStatus::Unhealthy,
                    dependency_status: GraphHealthStatus::Healthy,
                    dependencies: Vec::new(),
                    affected_by: Vec::new(),
                    detail: Some("connection refused <script>".into()),
                },
            ],
            edges: vec![HealthGraphEdge {
                source: "service".into(),
                target: "redis".into(),
                critical: false,
            }],
        };
        let html = render_graph_view(&graph);
        assert!(html.contains("Redis &lt;down&gt;"));
        assert!(html.contains("connection refused &lt;script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("own readiness separately"));
    }
}