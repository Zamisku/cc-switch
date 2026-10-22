//! Explicit opt-in native smoke test for the DSH adapter.
//!
//! It never touches the real `~/.dsh`: it only runs when `CCS_DSH_SMOKE_HOME` points at a
//! **copy** of a DSH home. Use it to check a real profile against a new DSH release:
//!
//! ```sh
//! cp -R ~/.dsh /tmp/dsh-smoke
//! CCS_DSH_SMOKE_HOME=/tmp/dsh-smoke \
//!   cargo test --manifest-path src-tauri/Cargo.toml --lib dsh_smoke -- --ignored --test-threads=1
//! ```

use crate::dsh_config::{self, DshPaths};
use crate::provider::Provider;
use serde_json::json;

fn smoke_paths() -> DshPaths {
    let Some(root) = std::env::var_os("CCS_DSH_SMOKE_HOME") else {
        panic!("CCS_DSH_SMOKE_HOME must point at a copy of a DSH home");
    };
    let paths = DshPaths::at(root);
    assert!(
        paths.patch_path().exists(),
        "no profile patch at {}; point CCS_DSH_SMOKE_HOME at a copied DSH home",
        paths.patch_path().display()
    );
    paths
}

fn smoke_provider() -> Provider {
    Provider::with_id(
        "ccs-smoke".to_string(),
        "CCS Smoke".to_string(),
        json!({
            "kind": "custom",
            "route": "ccs-smoke",
            "displayName": "CCS Smoke",
            "api": "anthropic-messages",
            "baseURL": "http://127.0.0.1:8787",
            "apiKey": "sk-smoke",
            "models": [{"id": "smoke-model", "name": "Smoke Model"}],
            "reasoningEffort": "low"
        }),
        None,
    )
}

#[test]
#[ignore = "manual: set CCS_DSH_SMOKE_HOME to a COPY of a real DSH home"]
fn roundtrips_a_real_profile_copy() {
    let paths = smoke_paths();
    let before = std::fs::read_to_string(paths.patch_path()).expect("read patch");
    let existing = dsh_config::read_live_providers(&paths).expect("read providers");
    let provider = smoke_provider();

    let switched = dsh_config::switch_to(&paths, &provider).expect("switch to smoke route");
    assert_eq!(switched.route, "ccs-smoke");
    assert_eq!(switched.model, "smoke-model");

    let after = std::fs::read_to_string(paths.patch_path()).expect("read patch");
    // 用户手写的其它行必须原样保留。
    for row in [
        "- id: agent-default-model",
        "- id: permission",
        "- id: ui-chat",
        "- id: ui-settings",
    ] {
        if before.contains(row) {
            assert!(after.contains(row), "row {row} disappeared");
        }
    }
    assert!(after.contains("ccs-smoke:"));
    assert_eq!(after.matches("- id: llm-pi-ai").count(), 1);

    let credentials = std::fs::read_to_string(paths.credentials_path()).expect("read credentials");
    assert!(credentials.contains("CCS_SMOKE_API_KEY"));

    let providers = dsh_config::read_live_providers(&paths).expect("read providers back");
    let smoke = providers
        .iter()
        .find(|item| item.provider.id == "ccs-smoke")
        .expect("smoke route imported");
    assert!(smoke.is_current, "smoke route must own the default pointer");
    assert_eq!(providers.len(), existing.len() + 1);

    let warnings = dsh_config::remove_provider_route(&paths, &provider).expect("remove");
    // 副本里原本没有别的路由：删掉 smoke 路由后指针要回落到 DSH 自带默认值，
    // 而不是留下指向已删路由的悬空指针。
    assert!(
        warnings
            .iter()
            .all(|warning| warning == "dsh.default_model_reset_to_default"),
        "unexpected warnings: {warnings:?}"
    );

    let cleaned = std::fs::read_to_string(paths.patch_path()).expect("read patch");
    assert!(!cleaned.contains("ccs-smoke"));
    assert!(!cleaned.contains("- id: agent-default-model"));
    assert_eq!(
        cleaned.matches("- id: llm-pi-ai").count(),
        before.matches("- id: llm-pi-ai").count()
    );
    let credentials = std::fs::read_to_string(paths.credentials_path()).expect("read credentials");
    assert!(!credentials.contains("CCS_SMOKE_API_KEY"));

    // MCP：一条 stdio 行的写入 → 禁用 → 删除。
    let mcp = dsh_config::upsert_mcp_server(
        &paths,
        "ccs-smoke-mcp",
        "ccs-smoke-mcp",
        &json!({
            "transport": "stdio",
            "command": "echo",
            "args": ["ccs-smoke"]
        }),
    )
    .expect("write MCP row");
    assert_eq!(mcp.row_id, "mcp-ccs-ccs-smoke-mcp");
    assert!(dsh_config::read_mcp_servers(&paths)
        .expect("read MCP rows")
        .iter()
        .any(|entry| entry.server_name.as_deref() == Some("ccs-smoke-mcp")));

    dsh_config::disable_mcp_server(&paths, "ccs-smoke-mcp", "ccs-smoke-mcp")
        .expect("disable MCP row");
    assert!(dsh_config::read_mcp_servers(&paths)
        .expect("read MCP rows")
        .iter()
        .any(|entry| entry.disabled));

    dsh_config::remove_owned_mcp_server(&paths, "ccs-smoke-mcp").expect("remove MCP row");
    assert!(!std::fs::read_to_string(paths.patch_path())
        .expect("read patch")
        .contains("ccs-smoke-mcp"));
}
