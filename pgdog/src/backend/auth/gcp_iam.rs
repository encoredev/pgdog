use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::backend::{Error, pool::Address};

/// Environment variable overriding the metadata server's host, the same one
/// Google's client libraries honour.
const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";
const METADATA_HOST: &str = "metadata.google.internal";

/// The scope Cloud SQL and AlloyDB accept an access token under as a database
/// password.
const SCOPE: &str = "https://www.googleapis.com/auth/sqlservice.login";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Fetch a fresh OAuth2 access token for the service account attached to the
/// workload (GCE, GKE Workload Identity, Cloud Run) from the metadata server.
///
/// Cloud SQL and AlloyDB IAM database authentication take this token as the
/// password of the user named after the service account's email, minus
/// `.gserviceaccount.com`. The token is the same for every database, so
/// `addr` is only used for error messages.
///
/// This is the raw fetcher passed to [`TokenCache::get_or_fetch`] and
/// called by the monitor's refresh loop. Callers should never invoke it
/// directly — go through [`TokenCache::global`] instead.
pub(crate) async fn token(addr: Address) -> Result<(String, SystemTime), Error> {
    let host = std::env::var(METADATA_HOST_ENV).unwrap_or_else(|_| METADATA_HOST.to_owned());
    let url = format!(
        "http://{host}/computeMetadata/v1/instance/service-accounts/default/token?scopes={SCOPE}"
    );

    let error = |message: String| {
        Error::GcpIamToken(format!(
            "{} for {}@{}:{}",
            message, addr.user, addr.host, addr.port
        ))
    };

    let response = reqwest::Client::new()
        .get(&url)
        .header("Metadata-Flavor", "Google")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| error(format!("failed to reach the metadata server: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(error(format!(
            "metadata server returned {status}: {}",
            body.trim()
        )));
    }

    let response: TokenResponse = response
        .json()
        .await
        .map_err(|e| error(format!("invalid metadata server response: {e}")))?;

    let expires_at = SystemTime::now() + Duration::from_secs(response.expires_in);
    Ok((response.access_token, expires_at))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::config::ServerAuth;
    use crate::test_utils::set_env_var;

    fn addr() -> Address {
        Address {
            host: "10.0.0.3".into(),
            port: 5432,
            database_name: "postgres".into(),
            user: "my-sa@my-project.iam".into(),
            server_auth: ServerAuth::GcpIam,
            ..Default::default()
        }
    }

    async fn metadata_server() -> (MockServer, impl Drop) {
        let server = MockServer::start().await;
        let host = server.uri().trim_start_matches("http://").to_owned();
        let guard = set_env_var(METADATA_HOST_ENV, host);
        (server, guard)
    }

    #[tokio::test]
    async fn test_token_from_metadata_server() {
        let (server, _guard) = metadata_server().await;

        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/token",
            ))
            .and(query_param("scopes", SCOPE))
            .and(header("Metadata-Flavor", "Google"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "ya29.token",
                "expires_in": 3599,
                "token_type": "Bearer",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let before = SystemTime::now();
        let (token, expires_at) = token(addr()).await.unwrap();

        assert_eq!(token, "ya29.token");
        assert!(expires_at >= before + Duration::from_secs(3599));
        assert!(expires_at <= SystemTime::now() + Duration::from_secs(3599));
    }

    #[tokio::test]
    async fn test_token_metadata_server_error() {
        let (server, _guard) = metadata_server().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("no service account\n"))
            .mount(&server)
            .await;

        let err = token(addr()).await.unwrap_err().to_string();

        assert!(err.contains("404"), "{err}");
        assert!(err.contains("no service account"), "{err}");
        assert!(err.contains("my-sa@my-project.iam@10.0.0.3:5432"), "{err}");
    }

    #[tokio::test]
    async fn test_token_invalid_response() {
        let (server, _guard) = metadata_server().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "error": "nope" })))
            .mount(&server)
            .await;

        let err = token(addr()).await.unwrap_err().to_string();

        assert!(err.contains("invalid metadata server response"), "{err}");
    }
}
