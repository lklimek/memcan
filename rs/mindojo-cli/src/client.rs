//! MCP HTTP client wrapper for thin CLI operations.

use std::path::Path;

use rmcp::{
    RoleClient, ServiceExt, model::CallToolRequestParams, service::RunningService,
    transport::StreamableHttpClientTransport,
    transport::streamable_http_client::StreamableHttpClientTransportConfig,
};

use crate::CliConfig;

pub struct McpClient {
    service: RunningService<RoleClient, ()>,
}

impl McpClient {
    pub async fn connect(config: &CliConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let mut url = config.url.clone();
        if !url.ends_with("/mcp") {
            url = format!("{}/mcp", url.trim_end_matches('/'));
        }

        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url);
        if let Some(ref key) = config.api_key {
            transport_config = transport_config.auth_header(format!("Bearer {key}"));
        }

        let transport =
            StreamableHttpClientTransport::with_client(reqwest::Client::new(), transport_config);
        let client = ().serve(transport).await?;

        Ok(Self { service: client })
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let json_obj: rmcp::model::JsonObject = arguments
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let params = CallToolRequestParams::new(name.to_string()).with_arguments(json_obj);

        let result = self.service.call_tool(params).await?;

        let texts: Vec<String> = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.to_string()))
            .collect();

        Ok(texts.join("\n"))
    }

    pub async fn close(self) {
        drop(self.service);
    }
}

pub fn load_config() -> CliConfig {
    let config_path = dirs::config_dir()
        .map(|d| d.join("mindojo").join(".env"))
        .unwrap_or_default();

    if config_path.exists() {
        let _ = dotenvy::from_path(&config_path);
    }

    let cwd_env = Path::new(".env");
    if cwd_env.exists() {
        let _ = dotenvy::from_path_override(cwd_env);
    }

    CliConfig {
        url: std::env::var("MINDOJO_URL").unwrap_or_else(|_| "http://localhost:8190".to_string()),
        api_key: std::env::var("MINDOJO_API_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
    }
}
