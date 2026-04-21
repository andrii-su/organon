use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use organon_core::config::OrgConfig;
use serde::Serialize;
use tauri::State;

#[derive(Default)]
struct DesktopState {
    api_base_url: Mutex<Option<String>>,
    server_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiBootstrap {
    base_url: String,
    db_path: String,
    source: String,
}

#[tauri::command]
async fn bootstrap_api(state: State<'_, DesktopState>) -> Result<ApiBootstrap, String> {
    let config = OrgConfig::load();
    let db_path = resolve_db_path(&config);

    let cached_url = state.api_base_url.lock().unwrap().clone();
    if let Some(base_url) = cached_url {
        if is_healthy(&base_url).await {
            return Ok(ApiBootstrap {
                base_url,
                db_path: db_path.display().to_string(),
                source: "cached".to_string(),
            });
        }
    }

    let configured_url = format!("http://{}:{}", config.server.host, config.server.port);
    if is_healthy(&configured_url).await {
        *state.api_base_url.lock().unwrap() = Some(configured_url.clone());
        return Ok(ApiBootstrap {
            base_url: configured_url,
            db_path: db_path.display().to_string(),
            source: "existing".to_string(),
        });
    }

    let preferred_port = if portpicker::is_free(config.server.port) {
        config.server.port
    } else {
        portpicker::pick_unused_port().ok_or("could not pick an open localhost port")?
    };
    let host = "127.0.0.1".to_string();
    let base_url = format!("http://{host}:{preferred_port}");

    let needs_spawn = state.server_task.lock().unwrap().is_none();
    if needs_spawn {
        let config_clone = config.clone();
        let db_path_clone = db_path.clone();
        let host_clone = host.clone();
        let handle = tauri::async_runtime::spawn(async move {
            if let Err(error) = organon_cli::api::serve(
                db_path_clone,
                config_clone,
                Some(host_clone),
                Some(preferred_port),
            )
            .await
            {
                eprintln!("organon desktop: api server failed: {error}");
            }
        });
        *state.server_task.lock().unwrap() = Some(handle);
    }

    wait_for_health(&base_url).await?;
    *state.api_base_url.lock().unwrap() = Some(base_url.clone());

    Ok(ApiBootstrap {
        base_url,
        db_path: db_path.display().to_string(),
        source: "spawned".to_string(),
    })
}

fn resolve_db_path(config: &OrgConfig) -> PathBuf {
    std::env::var("ORGANON_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(&config.indexer.db_path))
}

async fn wait_for_health(base_url: &str) -> Result<(), String> {
    for _ in 0..40 {
        if is_healthy(base_url).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(format!(
        "failed to start local Organon API at {base_url}. Check ORGANON_DB/config and try again."
    ))
}

async fn is_healthy(base_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(600))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    match client.get(format!("{base_url}/health")).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DesktopState::default())
        .invoke_handler(tauri::generate_handler![bootstrap_api])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
