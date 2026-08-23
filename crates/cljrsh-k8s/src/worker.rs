//! The per-client Kubernetes worker: one OS thread hosting a Tokio runtime,
//! a kube `Client`, and a discovery cache. Commands and replies are plain
//! `Send` data (serde_json / strings) — `GcPtr`s never cross the boundary.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;

use kube::api::{
    Api, AttachParams, DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams,
};
use kube::core::{ApiResource, GroupVersionKind};
use kube::discovery::{Discovery, Scope};
use k8s_openapi::api::core::v1::Pod;
use kube::{Client, Config};
use serde_json::Value as Json;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

/// How the caller names a resource kind: `Pod`, `pods`, `Deployment`,
/// `CausewayWorkspace`, optionally scoped by API group.
#[derive(Debug, Clone)]
pub struct KindRef {
    pub name: String,
    pub group: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    Get {
        kind: KindRef,
        name: String,
        ns: Option<String>,
    },
    List {
        kind: KindRef,
        ns: Option<String>,
        all_namespaces: bool,
        label_selector: Option<String>,
        field_selector: Option<String>,
    },
    Apply {
        manifest: Json,
    },
    PatchMerge {
        kind: KindRef,
        name: String,
        ns: Option<String>,
        patch: Json,
    },
    Delete {
        kind: KindRef,
        name: String,
        ns: Option<String>,
    },
    Logs {
        name: String,
        ns: Option<String>,
        container: Option<String>,
        tail: Option<i64>,
    },
    Exec {
        name: String,
        ns: Option<String>,
        container: Option<String>,
        command: Vec<String>,
    },
    Raw {
        path: String,
    },
    PortForward {
        name: String,
        ns: Option<String>,
        remote: u16,
        local: Option<u16>,
    },
    StopForward {
        id: u64,
    },
    ApiResources,
}

pub type Reply = Result<Json, String>;
pub type CmdTx = mpsc::UnboundedSender<(Cmd, std_mpsc::Sender<Reply>)>;

pub struct WorkerHandle {
    pub tx: CmdTx,
    _thread: std::thread::JoinHandle<()>,
}

impl WorkerHandle {
    pub fn call(&self, cmd: Cmd) -> Reply {
        let (rtx, rrx) = std_mpsc::channel();
        self.tx
            .send((cmd, rtx))
            .map_err(|_| "k8s worker is gone".to_string())?;
        rrx.recv().map_err(|_| "k8s worker dropped the reply".to_string())?
    }
}

/// Spawn the worker; connects (kubeconfig context / in-cluster) on first use.
pub fn spawn(context: Option<String>, kubeconfig: Option<String>) -> Result<WorkerHandle, String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Cmd, std_mpsc::Sender<Reply>)>();
    let thread = std::thread::Builder::new()
        .name("cljrsh-k8s".into())
        .spawn(move || {
            // Both ring (reqwest) and aws-lc (kube) are compiled in; rustls
            // needs one chosen explicitly. Idempotent across workers.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("k8s worker runtime");
            rt.block_on(async move {
                let mut state = match WorkerState::connect(context, kubeconfig).await {
                    Ok(s) => s,
                    Err(e) => {
                        // Fail every command with the connect error.
                        while let Some((_, reply)) = rx.recv().await {
                            let _ = reply.send(Err(e.clone()));
                        }
                        return;
                    }
                };
                while let Some((cmd, reply)) = rx.recv().await {
                    let result = state.handle(cmd).await;
                    let _ = reply.send(result);
                }
            });
        })
        .map_err(|e| format!("spawning k8s worker: {e}"))?;
    Ok(WorkerHandle { tx, _thread: thread })
}

struct Forward {
    stop: oneshot::Sender<()>,
}

struct WorkerState {
    client: Client,
    discovery: Discovery,
    forwards: HashMap<u64, Forward>,
    next_forward: u64,
}

impl WorkerState {
    async fn connect(context: Option<String>, kubeconfig: Option<String>) -> Result<Self, String> {
        let config = if context.is_some() || kubeconfig.is_some() {
            if let Some(path) = &kubeconfig {
                // SAFETY-free: parse the named kubeconfig file.
                let kc = kube::config::Kubeconfig::read_from(path)
                    .map_err(|e| format!("reading kubeconfig {path}: {e}"))?;
                Config::from_custom_kubeconfig(
                    kc,
                    &kube::config::KubeConfigOptions {
                        context: context.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| format!("kubeconfig: {e}"))?
            } else {
                Config::from_kubeconfig(&kube::config::KubeConfigOptions {
                    context: context.clone(),
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("kubeconfig: {e}"))?
            }
        } else {
            Config::infer()
                .await
                .map_err(|e| format!("no kube config (kubeconfig or in-cluster): {e}"))?
        };
        let client = Client::try_from(config).map_err(|e| format!("kube client: {e}"))?;
        let discovery = Discovery::new(client.clone())
            .run()
            .await
            .map_err(|e| format!("api discovery: {e}"))?;
        Ok(Self {
            client,
            discovery,
            forwards: HashMap::new(),
            next_forward: 1,
        })
    }

    /// Resolve a kind/plural name (optionally group-scoped) via discovery.
    fn resolve(&self, kind: &KindRef) -> Result<(ApiResource, Scope), String> {
        let want = kind.name.to_ascii_lowercase();
        let mut matches: Vec<(ApiResource, Scope, String)> = Vec::new();
        for group in self.discovery.groups() {
            if let Some(g) = &kind.group
                && group.name() != g
            {
                continue;
            }
            for (ar, caps) in group.recommended_resources() {
                if ar.kind.to_ascii_lowercase() == want || ar.plural.to_ascii_lowercase() == want {
                    matches.push((ar, caps.scope, group.name().to_string()));
                }
            }
        }
        match matches.len() {
            0 => Err(format!(
                "no such resource kind {:?}{} (try (k8s/api-resources c))",
                kind.name,
                kind.group
                    .as_deref()
                    .map(|g| format!(" in group {g:?}"))
                    .unwrap_or_default()
            )),
            1 => {
                let (ar, scope, _) = matches.remove(0);
                Ok((ar, scope))
            }
            _ => {
                // Prefer the core group, else demand disambiguation.
                if let Some(pos) = matches.iter().position(|(_, _, g)| g.is_empty()) {
                    let (ar, scope, _) = matches.remove(pos);
                    return Ok((ar, scope));
                }
                let groups: Vec<String> = matches.iter().map(|(_, _, g)| g.clone()).collect();
                Err(format!(
                    "kind {:?} is ambiguous across groups {groups:?}; pass :group",
                    kind.name
                ))
            }
        }
    }

    /// Re-run API discovery (CRDs may have been installed since connect).
    /// Retries briefly: aggregated API groups (metrics, webhooks) answer 503
    /// while a fresh cluster warms up, which fails the whole discovery run.
    async fn refresh_discovery(&mut self) -> Result<(), String> {
        let mut last_err = String::new();
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            // Aggregated discovery (2 requests, served from the apiserver's
            // own cache) doesn't touch each group's backing service, so a
            // warming-up aggregated apiservice can't 503 the whole run.
            let fresh = match Discovery::new(self.client.clone()).run_aggregated().await {
                Ok(d) => Ok(d),
                Err(_) => Discovery::new(self.client.clone()).run().await,
            };
            match fresh {
                Ok(d) => {
                    self.discovery = d;
                    return Ok(());
                }
                Err(e) => last_err = format!("api discovery: {e}"),
            }
        }
        Err(last_err)
    }

    /// Resolve, refreshing the discovery cache once on a miss so kinds from
    /// CRDs installed after connect (or by this very session) still resolve.
    async fn resolve_fresh(&mut self, kind: &KindRef) -> Result<(ApiResource, Scope), String> {
        match self.resolve(kind) {
            Ok(found) => Ok(found),
            Err(first_miss) => {
                self.refresh_discovery().await?;
                self.resolve(kind).map_err(|_| first_miss)
            }
        }
    }

    async fn api(&mut self, kind: &KindRef, ns: Option<&str>) -> Result<Api<DynamicObject>, String> {
        let (ar, scope) = self.resolve_fresh(kind).await?;
        Ok(match (scope, ns) {
            (Scope::Namespaced, Some(ns)) => Api::namespaced_with(self.client.clone(), ns, &ar),
            (Scope::Namespaced, None) => {
                Api::default_namespaced_with(self.client.clone(), &ar)
            }
            (Scope::Cluster, _) => Api::all_with(self.client.clone(), &ar),
        })
    }

    fn pods(&self, ns: Option<&str>) -> Api<Pod> {
        match ns {
            Some(ns) => Api::namespaced(self.client.clone(), ns),
            None => Api::default_namespaced(self.client.clone()),
        }
    }

    async fn handle(&mut self, cmd: Cmd) -> Reply {
        match cmd {
            Cmd::Get { kind, name, ns } => {
                let api = self.api(&kind, ns.as_deref()).await?;
                let obj = api.get(&name).await.map_err(fmt_err)?;
                serde_json::to_value(obj).map_err(|e| e.to_string())
            }
            Cmd::List {
                kind,
                ns,
                all_namespaces,
                label_selector,
                field_selector,
            } => {
                let (ar, scope) = self.resolve_fresh(&kind).await?;
                let api: Api<DynamicObject> = if all_namespaces || scope == Scope::Cluster {
                    Api::all_with(self.client.clone(), &ar)
                } else {
                    match ns.as_deref() {
                        Some(ns) => Api::namespaced_with(self.client.clone(), ns, &ar),
                        None => Api::default_namespaced_with(self.client.clone(), &ar),
                    }
                };
                let mut lp = ListParams::default();
                if let Some(l) = label_selector {
                    lp = lp.labels(&l);
                }
                if let Some(f) = field_selector {
                    lp = lp.fields(&f);
                }
                let list = api.list(&lp).await.map_err(fmt_err)?;
                let items: Vec<Json> = list
                    .items
                    .into_iter()
                    .map(|o| serde_json::to_value(o).unwrap_or(Json::Null))
                    .collect();
                Ok(Json::Array(items))
            }
            Cmd::Apply { manifest } => {
                let (gvk, name, ns) = manifest_coords(&manifest)?;
                let resolved = match self.discovery.resolve_gvk(&gvk) {
                    Some((ar, caps)) => Some((ar, caps.scope)),
                    None => {
                        self.refresh_discovery().await?;
                        self.discovery
                            .resolve_gvk(&gvk)
                            .map(|(ar, caps)| (ar, caps.scope))
                    }
                };
                let (ar, scope) =
                    resolved.ok_or_else(|| format!("cluster does not serve {gvk:?}"))?;
                let api: Api<DynamicObject> = match (scope, ns.as_deref()) {
                    (Scope::Namespaced, Some(ns)) => {
                        Api::namespaced_with(self.client.clone(), ns, &ar)
                    }
                    (Scope::Namespaced, None) => {
                        Api::default_namespaced_with(self.client.clone(), &ar)
                    }
                    (Scope::Cluster, _) => Api::all_with(self.client.clone(), &ar),
                };
                let obj = api
                    .patch(
                        &name,
                        &PatchParams::apply("cljrsh").force(),
                        &Patch::Apply(&manifest),
                    )
                    .await
                    .map_err(fmt_err)?;
                serde_json::to_value(obj).map_err(|e| e.to_string())
            }
            Cmd::PatchMerge {
                kind,
                name,
                ns,
                patch,
            } => {
                let api = self.api(&kind, ns.as_deref()).await?;
                let obj = api
                    .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                    .await
                    .map_err(fmt_err)?;
                serde_json::to_value(obj).map_err(|e| e.to_string())
            }
            Cmd::Delete { kind, name, ns } => {
                let api = self.api(&kind, ns.as_deref()).await?;
                api.delete(&name, &DeleteParams::default())
                    .await
                    .map_err(fmt_err)?;
                Ok(Json::Null)
            }
            Cmd::Logs {
                name,
                ns,
                container,
                tail,
            } => {
                let api = self.pods(ns.as_deref());
                let lp = LogParams {
                    container,
                    tail_lines: tail,
                    ..Default::default()
                };
                let text = api.logs(&name, &lp).await.map_err(fmt_err)?;
                Ok(Json::String(text))
            }
            Cmd::Exec {
                name,
                ns,
                container,
                command,
            } => {
                let api = self.pods(ns.as_deref());
                let ap = AttachParams {
                    container,
                    ..AttachParams::default().stdout(true).stderr(true)
                };
                let mut proc = api
                    .exec(&name, command, &ap)
                    .await
                    .map_err(fmt_err)?;
                let stdout = read_stream(proc.stdout()).await;
                let stderr = read_stream(proc.stderr()).await;
                let status = proc.take_status();
                proc.join().await.map_err(|e| e.to_string())?;
                let exit = match status {
                    Some(fut) => fut.await.and_then(|s| s.status).unwrap_or_default(),
                    None => String::new(),
                };
                Ok(serde_json::json!({
                    "out": stdout,
                    "err": stderr,
                    "success": exit != "Failure",
                }))
            }
            Cmd::Raw { path } => {
                let req = http::Request::get(&path)
                    .body(Vec::new())
                    .map_err(|e| e.to_string())?;
                let text = self
                    .client
                    .request_text(req)
                    .await
                    .map_err(fmt_err)?;
                Ok(serde_json::from_str(&text).unwrap_or(Json::String(text)))
            }
            Cmd::PortForward {
                name,
                ns,
                remote,
                local,
            } => {
                let api = self.pods(ns.as_deref());
                let listener = tokio::net::TcpListener::bind((
                    "127.0.0.1",
                    local.unwrap_or(0),
                ))
                .await
                .map_err(|e| format!("binding local port: {e}"))?;
                let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();
                let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
                let id = self.next_forward;
                self.next_forward += 1;
                self.forwards.insert(id, Forward { stop: stop_tx });
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = &mut stop_rx => break,
                            accepted = listener.accept() => {
                                let Ok((mut sock, _)) = accepted else { break };
                                // One API port-forward per accepted connection.
                                match api.portforward(&name, &[remote]).await {
                                    Ok(mut pf) => {
                                        if let Some(mut upstream) = pf.take_stream(remote) {
                                            let _ = tokio::io::copy_bidirectional(
                                                &mut sock,
                                                &mut upstream,
                                            )
                                            .await;
                                            let _ = upstream.shutdown().await;
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("cljrsh: port-forward {name}:{remote}: {e}");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });
                Ok(serde_json::json!({ "id": id, "local-port": local_port }))
            }
            Cmd::StopForward { id } => {
                if let Some(fwd) = self.forwards.remove(&id) {
                    let _ = fwd.stop.send(());
                }
                Ok(Json::Null)
            }
            Cmd::ApiResources => {
                let mut out = Vec::new();
                for group in self.discovery.groups() {
                    for (ar, caps) in group.recommended_resources() {
                        out.push(serde_json::json!({
                            "group": group.name(),
                            "version": ar.version,
                            "kind": ar.kind,
                            "plural": ar.plural,
                            "namespaced": caps.scope == Scope::Namespaced,
                        }));
                    }
                }
                Ok(Json::Array(out))
            }
        }
    }
}

async fn read_stream(stream: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut s) = stream else {
        return String::new();
    };
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    String::from_utf8_lossy(&out).into_owned()
}

fn fmt_err(e: kube::Error) -> String {
    match e {
        kube::Error::Api(err) => format!(
            "{} ({}): {}",
            err.reason, err.code, err.message
        ),
        other => other.to_string(),
    }
}

fn manifest_coords(manifest: &Json) -> Result<(GroupVersionKind, String, Option<String>), String> {
    let api_version = manifest
        .get("apiVersion")
        .and_then(|v| v.as_str())
        .ok_or("manifest needs apiVersion")?;
    let kind = manifest
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or("manifest needs kind")?;
    let name = manifest
        .pointer("/metadata/name")
        .and_then(|v| v.as_str())
        .ok_or("manifest needs metadata.name")?
        .to_string();
    let ns = manifest
        .pointer("/metadata/namespace")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let (group, version) = match api_version.split_once('/') {
        Some((g, v)) => (g.to_string(), v.to_string()),
        None => (String::new(), api_version.to_string()),
    };
    Ok((GroupVersionKind::gvk(&group, &version, kind), name, ns))
}
